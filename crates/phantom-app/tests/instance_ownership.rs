//! Native process-boundary coverage for remembered-session ownership.

#![cfg(target_os = "macos")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use phantom_core::{RememberedTabsLock, SessionStore, TabRecord};

const OWNER_HELPER_ENV: &str = "PHANTOM_TEST_INSTANCE_OWNER";
const OWNER_READY_ENV: &str = "PHANTOM_TEST_OWNER_READY_FILE";
const STARTUP_READY_ENV: &str = "PHANTOM_TEST_STARTUP_READY";
const STARTUP_READY: &str = "PHANTOM_STARTUP_READY";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("phantom-instance-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn kill_and_wait(mut self) -> std::io::Result<ExitStatus> {
        let mut child = self.0.take().unwrap();
        let kill = child.kill();
        let status = child.wait();
        kill?;
        status
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child_mut().try_wait().unwrap() {
                self.0.take();
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn isolate<'a>(command: &'a mut Command, root: &Path) -> &'a mut Command {
    command
        .env("HOME", root)
        .env("XDG_DATA_HOME", root.join("xdg-data"))
        .env("APPDATA", root.join("app-data"))
        .env("LOCALAPPDATA", root.join("local-app-data"))
}

fn wait_for_marker(child: &mut ChildGuard, path: &Path, marker: &str) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if std::fs::read_to_string(path).is_ok_and(|contents| contents.contains(marker)) {
            return;
        }
        assert!(
            child.child_mut().try_wait().unwrap().is_none(),
            "process exited before reporting readiness"
        );
        assert!(Instant::now() < deadline, "timed out waiting for readiness");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn remembered_tab(cwd: &Path) -> TabRecord {
    TabRecord {
        id: Some("remembered".into()),
        title: "remembered".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        sort_order: 0,
        is_active: true,
        shell_profile_id: None,
        created_at: None,
        updated_at: None,
    }
}

#[test]
fn remembered_instance_owner_helper() {
    if std::env::var_os(OWNER_HELPER_ENV).is_none() {
        return;
    }

    let _lock = RememberedTabsLock::acquire().unwrap();
    let store = SessionStore::open().unwrap();
    let cwd = std::env::current_dir().unwrap();
    store.save_tabs(&[remembered_tab(&cwd)]).unwrap();
    std::fs::write(std::env::var_os(OWNER_READY_ENV).unwrap(), b"ready").unwrap();

    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
    let tabs = store.load_tabs().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].id.as_deref(), Some("remembered"));
    assert_eq!(tabs[0].title, "remembered");
}

#[test]
fn native_secondaries_obey_remembered_session_ownership() {
    let scratch = ScratchDir::new();
    let owner_ready = scratch.0.join("owner-ready");
    let mut owner_command = Command::new(std::env::current_exe().unwrap());
    isolate(&mut owner_command, &scratch.0)
        .arg("--exact")
        .arg("remembered_instance_owner_helper")
        .env(OWNER_HELPER_ENV, "1")
        .env(OWNER_READY_ENV, &owner_ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut owner = ChildGuard::new(owner_command.spawn().unwrap());
    wait_for_marker(&mut owner, &owner_ready, "ready");

    let rejected_stderr = scratch.0.join("rejected-stderr");
    let mut rejected_command = Command::new(env!("CARGO_BIN_EXE_phantom"));
    isolate(&mut rejected_command, &scratch.0)
        .arg("--normal")
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(&rejected_stderr).unwrap(),
        ));
    let mut rejected = ChildGuard::new(rejected_command.spawn().unwrap());
    let rejected_status = rejected
        .wait_for_exit(PROCESS_TIMEOUT)
        .expect("rejected normal launch did not exit within the timeout");
    assert!(!rejected_status.success());
    let error = std::fs::read_to_string(&rejected_stderr).unwrap();
    assert!(
        error.contains("another normal Phantom instance already owns remembered tab state"),
        "{error}"
    );

    let ephemeral_ready = scratch.0.join("ephemeral-ready");
    let mut ephemeral_command = Command::new(env!("CARGO_BIN_EXE_phantom"));
    isolate(&mut ephemeral_command, &scratch.0)
        .arg("--cwd")
        .arg(&scratch.0)
        .env(STARTUP_READY_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            std::fs::File::create(&ephemeral_ready).unwrap(),
        ))
        .stderr(Stdio::null());
    let mut ephemeral = ChildGuard::new(ephemeral_command.spawn().unwrap());
    wait_for_marker(&mut ephemeral, &ephemeral_ready, STARTUP_READY);
    ephemeral.kill_and_wait().unwrap();

    owner
        .child_mut()
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    assert!(owner
        .wait_for_exit(PROCESS_TIMEOUT)
        .expect("owner helper did not exit within the timeout")
        .success());
}
