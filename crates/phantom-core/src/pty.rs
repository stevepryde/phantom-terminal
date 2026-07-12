use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;

use crate::error::{AppError, AppResult};

const MAX_LIVE_PTY_SESSIONS: usize = 256;

/// Input is forwarded to a per-session writer thread in chunks of this size
/// through a queue of [`WRITE_QUEUE_CHUNKS`] entries, bounding buffered input
/// to ~1 MB per session. PTY master writes block when the child stops reading
/// (suspended job, `Ctrl+S` flow control), so they must never run on the UI
/// thread; when the queue fills, [`PtyManager::write`] fails instead of
/// blocking.
const WRITE_CHUNK_BYTES: usize = 4096;
const WRITE_QUEUE_CHUNKS: usize = 256;
const MAX_LAUNCH_ENV_VARS: usize = 128;
const MAX_LAUNCH_ENV_KEY_LEN: usize = 128;
const MAX_LAUNCH_ENV_VALUE_LEN: usize = 16 * 1024;
const MAX_SHELL_PATH_LEN: usize = 64 * 1024;

/// Consumer of a PTY session's output.
///
/// The reader thread owns the sink and calls [`on_bytes`](PtySink::on_bytes) for
/// each chunk read from the PTY master, then [`on_eof`](PtySink::on_eof) exactly
/// once when the shell exits or the pipe closes. This keeps `phantom-core`
/// UI-agnostic: the native app feeds the bytes straight into the terminal
/// emulator in-process via this sink.
pub trait PtySink: Send + 'static {
    /// Handle a chunk of PTY output. Return `false` to stop the reader (e.g. the
    /// consumer has gone away); returning `true` continues reading.
    fn on_bytes(&mut self, bytes: &[u8]) -> bool;

    /// Called once after the PTY reaches EOF or the reader stops. Default no-op.
    fn on_eof(&mut self) {}
}

#[derive(Debug, Deserialize)]
pub struct SpawnOpts {
    /// Shell profile to resolve on the backend. Empty/None falls back to the
    /// configured default profile.
    #[serde(default)]
    pub shell_profile_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug)]
pub struct LaunchOpts {
    /// Executable to run. Empty/None falls back to the user's `$SHELL`.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Validated per-task environment additions. Reserved terminal/account
    /// variables are owned by Phantom and cannot be supplied by manifests.
    pub env: BTreeMap<String, String>,
    /// Resolve and pass PATH as initialized by the user's login shell. This is
    /// reserved for explicit contextual actions; ordinary profiles retain
    /// their existing launch behavior.
    pub inherit_shell_path: bool,
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    write_tx: SyncSender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
    pid: Option<u32>,
}

#[derive(Debug, Clone)]
struct AccountEnv {
    home: Option<String>,
    shell: Option<String>,
    user: Option<String>,
}

struct ProcessEnv {
    account: AccountEnv,
    path: String,
    login_shell_path: OnceLock<String>,
}

impl ProcessEnv {
    /// Explicit contextual programs must see the same PATH initialization as a
    /// normal tab's login shell. Resolve it lazily so ordinary shell tabs do
    /// not run startup files twice, then retain the GUI-safe fallback entries.
    fn explicit_program_path(&self) -> &str {
        self.login_shell_path.get_or_init(|| {
            login_shell_path(&self.account, &self.path)
                .map(|path| {
                    merge_path_parts(
                        [Some(path), Some(self.path.clone())],
                        self.account.home.as_deref(),
                    )
                })
                .unwrap_or_else(|| self.path.clone())
        })
    }
}

pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<u32, PtySession>>>,
    next_id: AtomicU32,
    live_sessions: Arc<AtomicUsize>,
    max_sessions: usize,
    reaper_tx: SyncSender<PtySession>,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU32::new(0),
            live_sessions: Arc::new(AtomicUsize::new(0)),
            max_sessions: MAX_LIVE_PTY_SESSIONS,
            reaper_tx: spawn_reaper(),
        }
    }
}

/// One thread that kills and reaps sessions handed off by [`PtyManager::kill`],
/// keeping the SIGHUP grace sleep and the `wait()` off the UI thread. If the
/// thread can't be spawned, the dangling sender makes every `try_send` fail and
/// `kill` falls back to inline kill+reap.
fn spawn_reaper() -> SyncSender<PtySession> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<PtySession>(64);
    let _ = std::thread::Builder::new()
        .name("pty-reaper".to_string())
        .spawn(move || {
            while let Ok(mut s) = rx.recv() {
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        });
    tx
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_max_sessions(max_sessions: usize) -> Self {
        Self {
            max_sessions,
            ..Self::default()
        }
    }

    /// Spawn a shell in a new PTY. Output bytes are delivered to `sink` from a
    /// dedicated reader thread until the shell exits or the pipe closes.
    pub fn spawn<S: PtySink>(&self, opts: LaunchOpts, mut sink: S) -> AppResult<u32> {
        validate_launch_env(&opts.env)?;
        let reservation = self.reserve_session()?;
        let size = PtySize {
            rows: opts.rows.max(1),
            cols: opts.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| AppError::Pty(format!("openpty failed: {e}")))?;

        let env = process_env();
        let account = &env.account;
        let use_default_shell =
            opts.command.as_ref().is_none_or(|c| c.is_empty()) && opts.args.is_empty();
        let launch_path = if opts.inherit_shell_path && !use_default_shell {
            env.explicit_program_path()
        } else {
            &env.path
        };
        let mut cmd = if use_default_shell {
            CommandBuilder::new_default_prog()
        } else {
            let command = opts
                .command
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| default_shell(account));
            let command = resolve_named_program(&command, launch_path)?;
            let mut cmd = CommandBuilder::new(command);
            for arg in &opts.args {
                cmd.arg(arg);
            }
            cmd
        };
        if let Some(cwd) = opts.cwd.as_ref().filter(|c| !c.is_empty()) {
            cmd.cwd(cwd);
        }
        // TERM advertises a capable terminal; PATH is normalized because macOS
        // GUI apps launch with a sparse environment compared with login shells.
        apply_terminal_env(&mut cmd);
        cmd.env("PATH", launch_path);
        apply_account_env(&mut cmd, account);
        apply_launch_env(&mut cmd, &opts.env);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::Pty(format!("spawn failed: {e}")))?;
        let pid = child.process_id();

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::Pty(format!("clone reader failed: {e}")))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| AppError::Pty(format!("take writer failed: {e}")))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        // Writer pump: the PTY master blocks when the child stops reading, so
        // all writes happen here, off the UI thread. The thread exits when the
        // session (and with it `write_tx`) is dropped, or when a write fails
        // (EIO after the child dies).
        let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE_CHUNKS);
        std::thread::Builder::new()
            .name(format!("pty-writer-{id}"))
            .spawn(move || {
                while let Ok(data) = write_rx.recv() {
                    if writer
                        .write_all(&data)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| AppError::Pty(format!("writer thread failed: {e}")))?;

        self.lock().insert(
            id,
            PtySession {
                master: pair.master,
                write_tx,
                child,
                pid,
            },
        );

        // Reader pump: forward PTY output to the sink until EOF/close.
        let sessions = Arc::clone(&self.sessions);
        let live_sessions = Arc::clone(&self.live_sessions);
        std::thread::Builder::new()
            .name(format!("pty-reader-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if !sink.on_bytes(&buf[..n]) {
                                break;
                            }
                        }
                    }
                }
                sink.on_eof();
                // Shell exited or pipe closed: drop the session. Bind before
                // the `if let` so the map lock is released first, then reap —
                // dropping a Child never calls wait(), and an unreaped child
                // is a zombie for the rest of the app's lifetime.
                let session = sessions
                    .lock()
                    .expect("pty sessions mutex poisoned")
                    .remove(&id);
                if let Some(mut s) = session {
                    live_sessions.fetch_sub(1, Ordering::AcqRel);
                    let _ = s.child.wait();
                }
            })
            .map_err(|e| {
                let session = self.lock().remove(&id);
                if let Some(mut s) = session {
                    let _ = s.child.kill();
                    let _ = s.child.wait();
                }
                AppError::Pty(format!("reader thread failed: {e}"))
            })?;
        reservation.disarm();

        Ok(id)
    }

    /// Queue `data` for the session's writer thread. Fails fast instead of
    /// blocking when the child has stopped reading input and the queue is full.
    pub fn write(&self, id: u32, data: &[u8]) -> AppResult<()> {
        let tx = self
            .lock()
            .get(&id)
            .ok_or_else(|| AppError::Pty("no such pty".to_string()))?
            .write_tx
            .clone();
        for chunk in data.chunks(WRITE_CHUNK_BYTES) {
            tx.try_send(chunk.to_vec()).map_err(|e| match e {
                TrySendError::Full(_) => {
                    AppError::Pty("terminal is not accepting input".to_string())
                }
                TrySendError::Disconnected(_) => AppError::Pty("no such pty".to_string()),
            })?;
        }
        Ok(())
    }

    pub fn resize(&self, id: u32, rows: u16, cols: u16) -> AppResult<()> {
        let sessions = self.lock();
        let s = sessions
            .get(&id)
            .ok_or_else(|| AppError::Pty("no such pty".to_string()))?;
        s.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Pty(e.to_string()))
    }

    pub fn kill(&self, id: u32) -> AppResult<()> {
        // Bind before destructuring: in edition 2021 the `if let` scrutinee
        // temporary (the MutexGuard) would otherwise live across the kill.
        let session = self.lock().remove(&id);
        if let Some(s) = session {
            self.live_sessions.fetch_sub(1, Ordering::AcqRel);
            // portable-pty's kill() sleeps through a SIGHUP grace period
            // (~250 ms) before SIGKILL, and the child must be reaped after —
            // hand both to the reaper thread instead of the calling (UI)
            // thread. try_send returns the session on failure, so the
            // fallback (a brief stall, better than a leak) kills inline.
            if let Err(error) = self.reaper_tx.try_send(s) {
                let mut s = match error {
                    TrySendError::Full(s) | TrySendError::Disconnected(s) => s,
                };
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        }
        Ok(())
    }

    /// Current working directory of the shell (used for session restore).
    pub fn cwd(&self, id: u32) -> Option<String> {
        let pid = self.lock().get(&id)?.pid?;
        cwd_of_pid(pid)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u32, PtySession>> {
        self.sessions.lock().expect("pty sessions mutex poisoned")
    }

    fn reserve_session(&self) -> AppResult<SessionReservation> {
        loop {
            let current = self.live_sessions.load(Ordering::Acquire);
            if current >= self.max_sessions {
                return Err(AppError::Pty("too many terminals".to_string()));
            }
            if self
                .live_sessions
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(SessionReservation {
                    live_sessions: Arc::clone(&self.live_sessions),
                    active: true,
                });
            }
        }
    }
}

struct SessionReservation {
    live_sessions: Arc<AtomicUsize>,
    active: bool,
}

impl SessionReservation {
    fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for SessionReservation {
    fn drop(&mut self) {
        if self.active {
            self.live_sessions.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn default_shell(account: &AccountEnv) -> String {
    account
        .shell
        .clone()
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Seed the static terminal-capability and color environment.
///
/// `TERM`/`COLORTERM` advertise a truecolor-capable terminal. `CLICOLOR`
/// switches on BSD `ls` colorization (so directories render blue out of the
/// box, the macOS equivalent of `ls -G`), and `LSCOLORS` supplies a default
/// scheme with bold directories. These are seeded as the spawn environment, so
/// anything the user exports from their shell rc still takes precedence.
fn apply_terminal_env(cmd: &mut CommandBuilder) {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("CLICOLOR", "1");
    cmd.env("LSCOLORS", "ExFxBxDxCxegedabagacad");
}

fn apply_account_env(cmd: &mut CommandBuilder, account: &AccountEnv) {
    if let Some(home) = &account.home {
        cmd.env("HOME", home);
    }
    if let Some(shell) = &account.shell {
        cmd.env("SHELL", shell);
    }
    if let Some(user) = &account.user {
        cmd.env("USER", user);
        cmd.env("LOGNAME", user);
    }
}

fn apply_launch_env(cmd: &mut CommandBuilder, env: &BTreeMap<String, String>) {
    for (key, value) in env {
        cmd.env(key, value);
    }
}

/// Revalidate task environment additions at the final PTY boundary so callers
/// cannot bypass the trusted-manifest policy by constructing `LaunchOpts`.
pub(crate) fn validate_launch_env(env: &BTreeMap<String, String>) -> AppResult<()> {
    if env.len() > MAX_LAUNCH_ENV_VARS {
        return Err(AppError::InvalidConfig(format!(
            "launch environment may contain no more than {MAX_LAUNCH_ENV_VARS} variables"
        )));
    }
    for (key, value) in env {
        let mut chars = key.chars();
        let valid_key = !key.is_empty()
            && key.len() <= MAX_LAUNCH_ENV_KEY_LEN
            && chars
                .next()
                .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
            && chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
        if !valid_key {
            return Err(AppError::InvalidConfig(format!(
                "invalid launch environment key '{key}'"
            )));
        }
        let upper = key.to_ascii_uppercase();
        let reserved = matches!(
            upper.as_str(),
            "PATH"
                | "HOME"
                | "USER"
                | "LOGNAME"
                | "SHELL"
                | "TERM"
                | "COLORTERM"
                | "TERM_PROGRAM"
                | "TERM_PROGRAM_VERSION"
                | "CLICOLOR"
                | "LSCOLORS"
        ) || upper.starts_with("LD_")
            || upper.starts_with("DYLD_");
        if reserved {
            return Err(AppError::InvalidConfig(format!(
                "launch environment may not override reserved variable '{key}'"
            )));
        }
        if value.len() > MAX_LAUNCH_ENV_VALUE_LEN || value.chars().any(char::is_control) {
            return Err(AppError::InvalidConfig(format!(
                "launch environment value for '{key}' is invalid"
            )));
        }
    }
    Ok(())
}

fn process_env() -> &'static ProcessEnv {
    static PROCESS_ENV: OnceLock<ProcessEnv> = OnceLock::new();
    PROCESS_ENV.get_or_init(|| {
        let account = account_env();
        let path = shell_path(&account);
        ProcessEnv {
            account,
            path,
            login_shell_path: OnceLock::new(),
        }
    })
}

#[cfg(unix)]
fn login_shell_path(account: &AccountEnv, fallback_path: &str) -> Option<String> {
    let shell = default_shell(account);
    let mut command = Command::new(shell);
    command
        // The leading NUL separates any interactive startup output from the
        // first environment entry, even when PATH happens to be emitted first.
        .args(["-ilc", "/usr/bin/printf '\\0'; /usr/bin/env -0"])
        .env("PATH", fallback_path);
    if let Some(home) = account.home.as_deref() {
        command.env("HOME", home);
    }
    if let Some(shell) = account.shell.as_deref() {
        command.env("SHELL", shell);
    }
    if let Some(user) = account.user.as_deref() {
        command.env("USER", user).env("LOGNAME", user);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_env_path(&output.stdout)
}

#[cfg(not(unix))]
fn login_shell_path(_account: &AccountEnv, _fallback_path: &str) -> Option<String> {
    None
}

fn parse_env_path(output: &[u8]) -> Option<String> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"PATH="))
        .filter(|path| !path.is_empty() && path.len() <= MAX_SHELL_PATH_LEN)
        .filter_map(|path| std::str::from_utf8(path).ok())
        .next_back()
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
fn account_env() -> AccountEnv {
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if entry.is_null() {
        return fallback_account_env();
    }

    let entry = unsafe { &*entry };
    AccountEnv {
        home: c_string(entry.pw_dir).or_else(|| std::env::var("HOME").ok()),
        shell: c_string(entry.pw_shell).or_else(|| std::env::var("SHELL").ok()),
        user: c_string(entry.pw_name)
            .or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("LOGNAME").ok()),
    }
}

#[cfg(target_os = "macos")]
fn c_string(ptr: *const libc::c_char) -> Option<String> {
    use std::ffi::CStr;

    if ptr.is_null() {
        return None;
    }

    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(target_os = "macos"))]
fn account_env() -> AccountEnv {
    fallback_account_env()
}

fn fallback_account_env() -> AccountEnv {
    AccountEnv {
        home: std::env::var("HOME").ok(),
        shell: std::env::var("SHELL").ok(),
        user: std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("LOGNAME").ok()),
    }
}

fn shell_path(account: &AccountEnv) -> String {
    merge_path_parts(
        [
            std::env::var("PATH").ok(),
            macos_path_helper_path(),
            Some(default_unix_path().to_string()),
        ],
        account.home.as_deref(),
    )
}

fn default_unix_path() -> &'static str {
    "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:~/bin:~/.cargo/bin:~/.local/bin"
}

#[cfg(unix)]
fn resolve_named_program(program: &str, path: &str) -> AppResult<String> {
    if Path::new(program).components().count() > 1 {
        return Ok(program.to_string());
    }
    for directory in std::env::split_paths(path) {
        let candidate = directory.join(program);
        if is_executable_file(&candidate) {
            return candidate.into_os_string().into_string().map_err(|_| {
                AppError::Pty(format!("program '{program}' path is not valid UTF-8"))
            });
        }
    }
    Err(AppError::Pty(format!(
        "program '{program}' was not found in the normalized PATH"
    )))
}

#[cfg(not(unix))]
fn resolve_named_program(program: &str, _path: &str) -> AppResult<String> {
    Ok(program.to_string())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn merge_path_parts(parts: impl IntoIterator<Item = Option<String>>, home: Option<&str>) -> String {
    let mut paths = Vec::<PathBuf>::new();
    for part in parts.into_iter().flatten() {
        paths.extend(std::env::split_paths(&expand_home_in_path_list(
            &part, home,
        )));
    }

    let mut merged = Vec::<PathBuf>::new();
    for path in paths {
        if !merged.iter().any(|existing| existing == &path) {
            merged.push(path);
        }
    }

    std::env::join_paths(merged)
        .ok()
        .and_then(|path| path.into_string().ok())
        .unwrap_or_else(|| default_unix_path().to_string())
}

fn expand_home_in_path_list(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };

    path.split(':')
        .map(|part| {
            part.strip_prefix("~/")
                .map(|rest| format!("{home}/{rest}"))
                .unwrap_or_else(|| part.to_string())
        })
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(target_os = "macos")]
fn macos_path_helper_path() -> Option<String> {
    let output = Command::new("/usr/libexec/path_helper")
        .arg("-s")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.split(';').find_map(|part| {
        let value = part.trim().strip_prefix("PATH=")?;
        Some(unquote_shell_value(value).to_string())
    })
}

#[cfg(not(target_os = "macos"))]
fn macos_path_helper_path() -> Option<String> {
    None
}

#[cfg(any(target_os = "macos", test))]
fn unquote_shell_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quoted = (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'');
        if quoted {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(target_os = "linux")]
fn cwd_of_pid(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn cwd_of_pid(pid: u32) -> Option<String> {
    // No /proc on macOS; query the kernel directly for the process's vnode info
    // rather than shelling out to `lsof` (faster, no subprocess, no PATH reliance).
    use std::os::raw::c_void;

    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut c_void,
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    // libc types `vip_path` as `[[c_char; 32]; 32]`, so flatten before reading.
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    if bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cwd_of_pid(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_shell_value_handles_path_helper_output() {
        assert_eq!(unquote_shell_value(r#""/usr/bin:/bin""#), "/usr/bin:/bin");
        assert_eq!(
            unquote_shell_value("'/opt/homebrew/bin'"),
            "/opt/homebrew/bin"
        );
        assert_eq!(unquote_shell_value("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn merge_path_parts_preserves_order_and_removes_duplicates() {
        let merged = merge_path_parts(
            [
                Some("/usr/bin:/bin".to_string()),
                Some("/bin:/opt/homebrew/bin".to_string()),
            ],
            None,
        );

        assert_eq!(merged, "/usr/bin:/bin:/opt/homebrew/bin");
    }

    #[test]
    fn env_path_parser_ignores_shell_noise_and_uses_nul_delimited_path() {
        let output =
            b"startup message\nHOME=/home/x\0PATH=/home/x/.bun/bin:/usr/bin\0SHELL=/bin/zsh\0";

        assert_eq!(
            parse_env_path(output).as_deref(),
            Some("/home/x/.bun/bin:/usr/bin")
        );
    }

    #[test]
    fn env_path_parser_rejects_empty_invalid_or_oversized_values() {
        assert_eq!(parse_env_path(b"PATH=\0"), None);
        assert_eq!(parse_env_path(b"PATH=\xff\0"), None);
        let oversized = format!("PATH={}\0", "a".repeat(MAX_SHELL_PATH_LEN + 1));
        assert_eq!(parse_env_path(oversized.as_bytes()), None);
    }

    fn account(home: Option<&str>, shell: Option<&str>, user: Option<&str>) -> AccountEnv {
        AccountEnv {
            home: home.map(str::to_string),
            shell: shell.map(str::to_string),
            user: user.map(str::to_string),
        }
    }

    #[test]
    fn default_shell_prefers_account_shell() {
        let account = account(None, Some("/usr/bin/fish"), None);
        assert_eq!(default_shell(&account), "/usr/bin/fish");
    }

    #[test]
    fn default_shell_falls_back_to_sh_without_account_or_env() {
        // The account has no shell; the only fallbacks are $SHELL then /bin/sh.
        // We cannot rely on $SHELL being unset in CI, so just assert the result
        // is a non-empty absolute path (either the env shell or /bin/sh).
        let account = account(None, None, None);
        let shell = default_shell(&account);
        assert!(
            shell.starts_with('/'),
            "expected absolute shell, got {shell:?}"
        );
    }

    #[test]
    fn expand_home_in_path_list_expands_tilde_segments() {
        let expanded =
            expand_home_in_path_list("~/.cargo/bin:/usr/bin:~/.local/bin", Some("/home/x"));
        assert_eq!(expanded, "/home/x/.cargo/bin:/usr/bin:/home/x/.local/bin");
    }

    #[test]
    fn expand_home_in_path_list_is_noop_without_home() {
        let path = "~/.cargo/bin:/usr/bin";
        assert_eq!(expand_home_in_path_list(path, None), path);
    }

    #[test]
    fn merge_path_parts_expands_home_once_and_dedupes() {
        let merged = merge_path_parts(
            [
                Some("~/.local/bin:/usr/bin".to_string()),
                Some("/usr/bin:~/.local/bin".to_string()),
            ],
            Some("/home/x"),
        );
        assert_eq!(merged, "/home/x/.local/bin:/usr/bin");
    }

    #[cfg(unix)]
    #[test]
    fn named_programs_resolve_from_the_normalized_path() {
        assert_eq!(
            resolve_named_program("sh", "/definitely-missing:/bin").unwrap(),
            "/bin/sh"
        );
        assert_eq!(
            resolve_named_program("./serve.sh", "/bin").unwrap(),
            "./serve.sh"
        );
    }

    #[test]
    fn gui_path_fallback_includes_personal_bin_directories() {
        let expanded = expand_home_in_path_list(default_unix_path(), Some("/Users/steve"));

        assert!(expanded.split(':').any(|path| path == "/Users/steve/bin"));
        assert!(expanded
            .split(':')
            .any(|path| path == "/Users/steve/.cargo/bin"));
        assert!(expanded
            .split(':')
            .any(|path| path == "/Users/steve/.local/bin"));
    }

    #[test]
    fn apply_terminal_env_sets_capability_and_color_vars() {
        let mut cmd = CommandBuilder::new("/bin/true");
        apply_terminal_env(&mut cmd);

        assert_eq!(
            cmd.get_env("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        );
        assert_eq!(
            cmd.get_env("COLORTERM"),
            Some(std::ffi::OsStr::new("truecolor"))
        );
        // CLICOLOR enables BSD `ls` colorization (blue directories by default).
        assert_eq!(cmd.get_env("CLICOLOR"), Some(std::ffi::OsStr::new("1")));
        assert_eq!(
            cmd.get_env("LSCOLORS"),
            Some(std::ffi::OsStr::new("ExFxBxDxCxegedabagacad"))
        );
    }

    #[test]
    fn apply_account_env_sets_identity_vars() {
        let account = account(Some("/home/x"), Some("/bin/zsh"), Some("x"));
        let mut cmd = CommandBuilder::new("/bin/true");
        apply_account_env(&mut cmd, &account);

        assert_eq!(cmd.get_env("HOME"), Some(std::ffi::OsStr::new("/home/x")));
        assert_eq!(cmd.get_env("SHELL"), Some(std::ffi::OsStr::new("/bin/zsh")));
        assert_eq!(cmd.get_env("USER"), Some(std::ffi::OsStr::new("x")));
        assert_eq!(cmd.get_env("LOGNAME"), Some(std::ffi::OsStr::new("x")));
    }

    #[test]
    fn apply_account_env_skips_missing_fields() {
        let account = account(None, None, None);
        let mut cmd = CommandBuilder::new("/bin/true");
        // `CommandBuilder::new` seeds the process env; clear it so the test
        // observes only what `apply_account_env` adds (i.e. nothing here).
        cmd.env_clear();
        apply_account_env(&mut cmd, &account);

        assert_eq!(cmd.get_env("HOME"), None);
        assert_eq!(cmd.get_env("SHELL"), None);
        assert_eq!(cmd.get_env("USER"), None);
        assert_eq!(cmd.get_env("LOGNAME"), None);
    }

    #[test]
    fn apply_launch_env_sets_validated_task_values() {
        let mut cmd = CommandBuilder::new("/bin/true");
        let env = BTreeMap::from([
            ("RUST_LOG".to_string(), "phantom=debug".to_string()),
            ("SOULFIRE_MODE".to_string(), "local".to_string()),
        ]);

        apply_launch_env(&mut cmd, &env);

        assert_eq!(
            cmd.get_env("RUST_LOG"),
            Some(std::ffi::OsStr::new("phantom=debug"))
        );
        assert_eq!(
            cmd.get_env("SOULFIRE_MODE"),
            Some(std::ffi::OsStr::new("local"))
        );
    }

    #[test]
    fn final_launch_boundary_rejects_reserved_or_control_environment() {
        let reserved = BTreeMap::from([("PATH".to_string(), "/tmp".to_string())]);
        assert!(validate_launch_env(&reserved).is_err());

        let control = BTreeMap::from([("RUST_LOG".to_string(), "info\nPATH=/tmp".to_string())]);
        assert!(validate_launch_env(&control).is_err());
    }

    #[test]
    fn process_env_is_cached_for_process_lifetime() {
        let first = process_env() as *const ProcessEnv;
        let second = process_env() as *const ProcessEnv;

        assert_eq!(first, second);
        assert!(!process_env().path.is_empty());
    }

    #[test]
    fn pty_session_limit_refuses_new_reservations_at_cap() {
        let manager = PtyManager::with_max_sessions(1);
        let reservation = manager.reserve_session().unwrap();

        let error = match manager.reserve_session() {
            Ok(_) => panic!("expected session cap error"),
            Err(error) => error.to_string(),
        };

        assert_eq!(error, "pty error: too many terminals");
        drop(reservation);
        assert!(manager.reserve_session().is_ok());
    }

    #[cfg(unix)]
    struct EofFlag(Arc<AtomicUsize>);

    #[cfg(unix)]
    impl PtySink for EofFlag {
        fn on_bytes(&mut self, _bytes: &[u8]) -> bool {
            true
        }
        fn on_eof(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }

    /// A shell that exits on its own must be reaped (no zombie left behind).
    /// Signal 0 succeeds for zombies, so it only starts failing with ESRCH
    /// once the child has been wait()ed on.
    #[test]
    #[cfg(unix)]
    fn natural_exit_reaps_child() {
        let manager = PtyManager::new();
        let eof = Arc::new(AtomicUsize::new(0));
        let id = manager
            .spawn(
                LaunchOpts {
                    command: Some("/bin/sh".to_string()),
                    args: vec!["-c".to_string(), "exit 0".to_string()],
                    env: BTreeMap::new(),
                    inherit_shell_path: false,
                    cwd: None,
                    rows: 24,
                    cols: 80,
                },
                EofFlag(Arc::clone(&eof)),
            )
            .expect("spawn shell");
        let pid = {
            let sessions = manager.lock();
            sessions.get(&id).and_then(|s| s.pid).expect("child pid")
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let reaped = loop {
            let eof_seen = eof.load(Ordering::Acquire) > 0;
            let gone = unsafe { libc::kill(pid as libc::c_int, 0) } == -1;
            if eof_seen && gone {
                break true;
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(reaped, "child {pid} was not reaped after natural exit");
    }

    #[test]
    fn pty_session_limit_can_be_zero_for_tests() {
        let manager = PtyManager::with_max_sessions(0);

        let error = match manager.reserve_session() {
            Ok(_) => panic!("expected session cap error"),
            Err(error) => error.to_string(),
        };

        assert_eq!(error, "pty error: too many terminals");
    }
}
