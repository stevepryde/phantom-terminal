//! Opens a validated local file with the OS association or VS Code fallback.

use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
};

use crate::{AppEvent, PtyOutbox};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorCommand {
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(target_os = "macos")]
const EDITOR_COMMANDS: &[EditorCommand] = &[
    EditorCommand {
        program: "open",
        args: &[],
    },
    EditorCommand {
        program: "open",
        args: &["-a", "Visual Studio Code"],
    },
];

#[cfg(target_os = "linux")]
const EDITOR_COMMANDS: &[EditorCommand] = &[
    EditorCommand {
        program: "xdg-open",
        args: &[],
    },
    EditorCommand {
        program: "code",
        args: &[],
    },
];

#[cfg(target_os = "windows")]
const EDITOR_COMMANDS: &[EditorCommand] = &[
    EditorCommand {
        program: "explorer.exe",
        args: &[],
    },
    EditorCommand {
        program: "code",
        args: &[],
    },
];

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const EDITOR_COMMANDS: &[EditorCommand] = &[EditorCommand {
    program: "code",
    args: &[],
}];

/// Launches the editor chain off the UI thread. `status()` can block for a long
/// time (xdg-open may exec the handler directly and never return), so waiting
/// for each candidate and falling back happens on a short-lived worker thread;
/// failure is reported back through the event outbox.
pub(crate) fn open(path: PathBuf, outbox: Arc<dyn PtyOutbox>) {
    let _ = thread::Builder::new()
        .name("external-editor".to_string())
        .spawn(move || {
            if let Err(error) = open_blocking(&path) {
                outbox.send(AppEvent::EditorOpenFailed {
                    error: error.to_string(),
                });
            }
        });
}

fn open_blocking(path: &Path) -> io::Result<()> {
    try_commands(path, EDITOR_COMMANDS, |candidate, path| {
        Command::new(candidate.program)
            .args(candidate.args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
    })
}

fn try_commands(
    path: &Path,
    candidates: &[EditorCommand],
    mut launch: impl FnMut(EditorCommand, &Path) -> io::Result<bool>,
) -> io::Result<()> {
    let mut last_error = None;
    for candidate in candidates {
        match launch(*candidate, path) {
            Ok(true) => return Ok(()),
            Ok(false) => {
                last_error = Some(io::Error::other(format!(
                    "{} did not open the file",
                    candidate.program
                )));
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("no editor command is available")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_default_handler_falls_back_and_stops_after_success() {
        let candidates = [
            EditorCommand {
                program: "default-editor",
                args: &[],
            },
            EditorCommand {
                program: "code",
                args: &[],
            },
        ];
        let mut launched = Vec::new();

        try_commands(
            Path::new("/project/.phantom.yml"),
            &candidates,
            |command, _| {
                launched.push(command.program);
                Ok(command.program == "code")
            },
        )
        .unwrap();

        assert_eq!(launched, ["default-editor", "code"]);
    }

    #[test]
    fn reports_failure_when_no_editor_can_open_the_file() {
        let candidates = [EditorCommand {
            program: "missing-editor",
            args: &[],
        }];

        let error = try_commands(Path::new("/project/.phantom.yml"), &candidates, |_, _| {
            Err(io::Error::new(io::ErrorKind::NotFound, "missing"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
