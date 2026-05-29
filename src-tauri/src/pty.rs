use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use tauri::ipc::Channel;

use crate::error::{AppError, AppResult};

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

pub struct LaunchOpts {
    /// Executable to run. Empty/None falls back to the user's `$SHELL`.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    pid: Option<u32>,
}

#[derive(Debug, Clone)]
struct AccountEnv {
    home: Option<String>,
    shell: Option<String>,
    user: Option<String>,
}

#[derive(Default)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<u32, PtySession>>>,
    next_id: AtomicU32,
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a shell in a new PTY. Output bytes are streamed back over `on_data`.
    pub fn spawn(&self, opts: LaunchOpts, on_data: Channel<Vec<u8>>) -> AppResult<u32> {
        let size = PtySize {
            rows: opts.rows.max(1),
            cols: opts.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| AppError::Pty(format!("openpty failed: {e}")))?;

        let account = account_env();
        let use_default_shell =
            opts.command.as_ref().is_none_or(|c| c.is_empty()) && opts.args.is_empty();
        let mut cmd = if use_default_shell {
            CommandBuilder::new_default_prog()
        } else {
            let command = opts
                .command
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| default_shell(&account));
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
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("PATH", shell_path());
        apply_account_env(&mut cmd, &account);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::Pty(format!("spawn failed: {e}")))?;
        let pid = child.process_id();

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::Pty(format!("clone reader failed: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| AppError::Pty(format!("take writer failed: {e}")))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        self.lock().insert(
            id,
            PtySession {
                master: pair.master,
                writer,
                child,
                pid,
            },
        );

        // Reader pump: forward PTY output to the frontend until EOF/close.
        let sessions = Arc::clone(&self.sessions);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if on_data.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            // Shell exited or pipe closed: drop the session.
            sessions
                .lock()
                .expect("pty sessions mutex poisoned")
                .remove(&id);
        });

        Ok(id)
    }

    pub fn write(&self, id: u32, data: &[u8]) -> AppResult<()> {
        let mut sessions = self.lock();
        let s = sessions
            .get_mut(&id)
            .ok_or_else(|| AppError::Pty("no such pty".to_string()))?;
        s.writer.write_all(data)?;
        s.writer.flush()?;
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
        if let Some(mut s) = self.lock().remove(&id) {
            let _ = s.child.kill();
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
}

fn default_shell(account: &AccountEnv) -> String {
    account
        .shell
        .clone()
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string())
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

fn shell_path() -> String {
    merge_path_parts([
        std::env::var("PATH").ok(),
        macos_path_helper_path(),
        Some(default_unix_path().to_string()),
    ])
}

fn default_unix_path() -> &'static str {
    "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:~/.cargo/bin:~/.local/bin"
}

fn merge_path_parts(parts: impl IntoIterator<Item = Option<String>>) -> String {
    let mut paths = Vec::<PathBuf>::new();
    for part in parts.into_iter().flatten() {
        paths.extend(std::env::split_paths(&expand_home_in_path_list(&part)));
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

fn expand_home_in_path_list(path: &str) -> String {
    let Some(home) = account_env().home else {
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
        let merged = merge_path_parts([
            Some("/usr/bin:/bin".to_string()),
            Some("/bin:/opt/homebrew/bin".to_string()),
        ]);

        assert_eq!(merged, "/usr/bin:/bin:/opt/homebrew/bin");
    }
}
