//! Dependency-free routing for Phantom's small non-GUI command surface.

use std::ffi::OsString;
use std::path::PathBuf;

use phantom_core::{validate_context_manifest, CONTEXT_MANIFEST_FILE};

use crate::skill_install::{install_for_target, SkillTarget};

const HELP: &str = "Phantom Terminal\n\nUsage:\n  phantom [--cwd <directory> | --normal]\n  phantom context validate [directory]\n  phantom skill install [--target all|codex|claude] [--force]\n";
const SKILL_NAME: &str = "phantom-workflows";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Validate { directory: PathBuf },
    Install { target: SkillTarget, force: bool },
}

#[derive(Debug)]
pub struct CliError {
    message: String,
    usage: bool,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: true,
        }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: false,
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.usage {
            2
        } else {
            1
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Run a recognized non-GUI command. `Ok(false)` preserves the existing GUI
/// launch path, including its tolerant handling of unrelated launch arguments.
pub fn dispatch_from_env() -> Result<bool, CliError> {
    let Some(command) = parse(std::env::args_os().skip(1).collect())? else {
        return Ok(false);
    };
    match command {
        Command::Help => print!("{HELP}"),
        Command::Validate { directory } => {
            let loaded = validate_context_manifest(&directory)
                .map_err(|error| CliError::failure(error.to_string()))?;
            let tab_count = loaded.manifest.tabs.len();
            let tab_label = if tab_count == 1 { "tab" } else { "tabs" };
            println!(
                "Valid {}: {} ({} {}) at {}",
                CONTEXT_MANIFEST_FILE,
                loaded.manifest.name,
                tab_count,
                tab_label,
                loaded.root.join(CONTEXT_MANIFEST_FILE).display()
            );
        }
        Command::Install { target, force } => {
            let results = install_for_target(target, force).map_err(CliError::failure)?;
            for result in results {
                println!(
                    "{} {SKILL_NAME} for {} at {}",
                    result.status.label(),
                    result.target,
                    result.path.display()
                );
            }
        }
    }
    Ok(true)
}

fn parse(args: Vec<OsString>) -> Result<Option<Command>, CliError> {
    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return Ok(None);
    };
    match first {
        "--help" | "-h" | "help" => {
            if args.len() == 1 {
                Ok(Some(Command::Help))
            } else {
                Err(CliError::usage("help does not accept arguments"))
            }
        }
        "context" => parse_context(&args[1..]).map(Some),
        "skill" => parse_skill(&args[1..]).map(Some),
        _ => Ok(None),
    }
}

fn parse_context(args: &[OsString]) -> Result<Command, CliError> {
    if args.first().and_then(|arg| arg.to_str()) != Some("validate") || args.len() > 2 {
        return Err(CliError::usage(
            "usage: phantom context validate [directory]",
        ));
    }
    let directory = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(Command::Validate { directory })
}

fn parse_skill(args: &[OsString]) -> Result<Command, CliError> {
    if args.first().and_then(|arg| arg.to_str()) != Some("install") {
        return Err(CliError::usage(
            "usage: phantom skill install [--target all|codex|claude] [--force]",
        ));
    }
    let mut target = SkillTarget::All;
    let mut saw_target = false;
    let mut force = false;
    let mut index = 1;
    while index < args.len() {
        let Some(argument) = args[index].to_str() else {
            return Err(CliError::usage("skill install arguments must be UTF-8"));
        };
        if argument == "--force" {
            if force {
                return Err(CliError::usage("--force may only be supplied once"));
            }
            force = true;
            index += 1;
            continue;
        }
        let value = if argument == "--target" {
            index += 1;
            args.get(index)
                .and_then(|value| value.to_str())
                .ok_or_else(|| CliError::usage("--target requires all, codex, or claude"))?
        } else if let Some(value) = argument.strip_prefix("--target=") {
            value
        } else {
            return Err(CliError::usage(format!(
                "unknown skill install argument '{argument}'"
            )));
        };
        if saw_target {
            return Err(CliError::usage("--target may only be supplied once"));
        }
        target = match value {
            "all" => SkillTarget::All,
            "codex" => SkillTarget::Codex,
            "claude" => SkillTarget::Claude,
            _ => return Err(CliError::usage("--target requires all, codex, or claude")),
        };
        saw_target = true;
        index += 1;
    }
    Ok(Command::Install { target, force })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn ordinary_launch_arguments_still_route_to_gui() {
        assert_eq!(parse(Vec::new()).unwrap(), None);
        assert_eq!(parse(args(&["--normal"])).unwrap(), None);
        assert_eq!(parse(args(&["--cwd", "/tmp"])).unwrap(), None);
        assert_eq!(parse(args(&["unknown"])).unwrap(), None);
    }

    #[test]
    fn parses_context_validation() {
        assert_eq!(
            parse(args(&["context", "validate"])).unwrap(),
            Some(Command::Validate {
                directory: PathBuf::from(".")
            })
        );
        assert_eq!(
            parse(args(&["context", "validate", "/project"])).unwrap(),
            Some(Command::Validate {
                directory: PathBuf::from("/project")
            })
        );
        assert!(parse(args(&["context", "validate", "a", "b"])).is_err());
    }

    #[test]
    fn parses_skill_install_targets_and_force() {
        assert_eq!(
            parse(args(&["skill", "install"])).unwrap(),
            Some(Command::Install {
                target: SkillTarget::All,
                force: false
            })
        );
        assert_eq!(
            parse(args(&["skill", "install", "--target=claude", "--force"])).unwrap(),
            Some(Command::Install {
                target: SkillTarget::Claude,
                force: true
            })
        );
        assert!(parse(args(&["skill", "install", "--target", "other"])).is_err());
        assert!(parse(args(&["skill", "install", "--force", "--force"])).is_err());
    }
}
