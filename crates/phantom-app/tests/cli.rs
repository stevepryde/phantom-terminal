use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "phantom-cli-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn phantom() -> Command {
    Command::new(env!("CARGO_BIN_EXE_phantom"))
}

#[test]
fn context_validate_is_a_non_gui_success_or_failure() {
    let temp = TestDir::new("context");
    fs::create_dir_all(temp.path().join("services/api")).unwrap();
    fs::write(
        temp.path().join(".phantom.yml"),
        "version: 1\nname: Example\ntabs:\n  - id: api\n    title: API\n    cwd: services/api\n    run:\n      program: cargo\n      args: [run]\n",
    )
    .unwrap();

    let valid = phantom()
        .args(["context", "validate"])
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(valid.status.success());
    let stdout = String::from_utf8(valid.stdout).unwrap();
    assert!(stdout.contains("Valid .phantom.yml: Example (1 tab)"));
    assert!(stdout.contains(&temp.path().canonicalize().unwrap().display().to_string()));

    fs::remove_file(temp.path().join(".phantom.yml")).unwrap();
    let missing = phantom()
        .args(["context", "validate"])
        .arg(temp.path())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("does not contain .phantom.yml"));
}

#[test]
fn skill_install_targets_both_agents_and_is_idempotent() {
    let temp = TestDir::new("skills");
    let codex_home = temp.path().join("codex");
    let claude_home = temp.path().join("claude");

    let install = phantom()
        .args(["skill", "install"])
        .env("CODEX_HOME", &codex_home)
        .env("CLAUDE_CONFIG_DIR", &claude_home)
        .output()
        .unwrap();
    assert!(install.status.success());
    let stdout = String::from_utf8(install.stdout).unwrap();
    assert!(stdout.contains("Installed phantom-workflows for Codex"));
    assert!(stdout.contains("Installed phantom-workflows for Claude"));

    let codex_skill = codex_home.join("skills/phantom-workflows/SKILL.md");
    let claude_skill = claude_home.join("skills/phantom-workflows/SKILL.md");
    assert_eq!(
        fs::read(&codex_skill).unwrap(),
        fs::read(&claude_skill).unwrap()
    );

    let current = phantom()
        .args(["skill", "install"])
        .env("CODEX_HOME", &codex_home)
        .env("CLAUDE_CONFIG_DIR", &claude_home)
        .output()
        .unwrap();
    assert!(current.status.success());
    let stdout = String::from_utf8(current.stdout).unwrap();
    assert!(stdout.contains("Already current phantom-workflows for Codex"));
    assert!(stdout.contains("Already current phantom-workflows for Claude"));
}

#[test]
fn skill_target_selection_and_usage_errors_are_explicit() {
    let temp = TestDir::new("target");
    let codex_home = temp.path().join("codex");
    let claude_home = temp.path().join("claude");
    let selected = phantom()
        .args(["skill", "install", "--target", "codex"])
        .env("CODEX_HOME", &codex_home)
        .env("CLAUDE_CONFIG_DIR", &claude_home)
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert!(codex_home
        .join("skills/phantom-workflows/SKILL.md")
        .is_file());
    assert!(!claude_home.exists());

    let invalid = phantom()
        .args(["skill", "install", "--target", "other"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn invalid_cwd_exits_before_launching_the_gui() {
    let temp = TestDir::new("invalid-cwd");
    let missing = temp.path().join("does-not-exist");

    let output = phantom().arg("--cwd").arg(&missing).output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("phantom: invalid --cwd"));
    assert!(stderr.contains(&missing.display().to_string()));
}
