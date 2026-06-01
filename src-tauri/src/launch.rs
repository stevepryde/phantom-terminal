use serde::Serialize;

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_LAUNCH_CWD_LEN: usize = 4096;
const MAX_LAUNCH_COMMAND_ARGS: usize = 256;
const MAX_LAUNCH_COMMAND_ARG_LEN: usize = 4096;

#[derive(Clone, Debug, Serialize)]
pub struct LaunchContext {
    pub cwd: Option<String>,
    pub remember_tabs: bool,
    pub command_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCommand {
    pub command: String,
    pub args: Vec<String>,
}

pub struct LaunchState {
    context: LaunchContext,
    command: Mutex<Option<LaunchCommand>>,
}

impl LaunchState {
    pub fn from_env() -> Self {
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        parsed_args_to_state(parse_launch_args(env::args_os().skip(1), &current_dir))
    }

    pub fn context(&self) -> LaunchContext {
        self.context.clone()
    }

    pub fn take_command(&self) -> Option<LaunchCommand> {
        self.command
            .lock()
            .expect("launch command mutex poisoned")
            .take()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedLaunch {
    cwd: Option<String>,
    command: Option<LaunchCommand>,
}

fn parsed_args_to_state(parsed: ParsedLaunch) -> LaunchState {
    let command_available = parsed.command.is_some();
    let remember_tabs = parsed.cwd.is_none() && !command_available;
    LaunchState {
        context: LaunchContext {
            cwd: parsed.cwd,
            remember_tabs,
            command_available,
        },
        command: Mutex::new(parsed.command),
    }
}

fn parse_launch_args<I>(args: I, current_dir: &Path) -> ParsedLaunch
where
    I: IntoIterator<Item = OsString>,
{
    let mut cwd: Option<String> = None;
    let mut command: Option<LaunchCommand> = None;
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        if arg == "--" || arg == "-e" || arg == "--execute" {
            command = launch_command(iter.collect());
            break;
        }

        if arg == "--working-directory" || arg == "--cwd" {
            if let Some(path) = iter.next() {
                cwd = resolve_launch_cwd(&path, current_dir).or(cwd);
            }
            continue;
        }

        if let Some(value) =
            option_value(&arg, "--working-directory=").or_else(|| option_value(&arg, "--cwd="))
        {
            cwd = resolve_launch_cwd(&OsString::from(value), current_dir).or(cwd);
            continue;
        }

        if option_like(&arg) {
            continue;
        }

        cwd = resolve_launch_cwd(&arg, current_dir).or(cwd);
    }

    if command.is_some() && cwd.is_none() {
        cwd = normalize_cwd_path(current_dir);
    }

    ParsedLaunch { cwd, command }
}

fn option_value(arg: &OsStr, prefix: &str) -> Option<String> {
    let text = arg.to_str()?;
    text.strip_prefix(prefix).map(ToOwned::to_owned)
}

fn option_like(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|text| text.starts_with('-'))
}

fn launch_command(args: Vec<OsString>) -> Option<LaunchCommand> {
    if args.is_empty() || args.len() > MAX_LAUNCH_COMMAND_ARGS + 1 {
        return None;
    }

    let mut strings = Vec::with_capacity(args.len());
    for arg in args {
        let value = arg.to_string_lossy().into_owned();
        if value.is_empty() || value.contains('\0') || value.len() > MAX_LAUNCH_COMMAND_ARG_LEN {
            return None;
        }
        strings.push(value);
    }

    let command = strings.remove(0);
    Some(LaunchCommand {
        command,
        args: strings,
    })
}

fn resolve_launch_cwd(arg: &OsStr, current_dir: &Path) -> Option<String> {
    let path = if let Some(file_path) = file_url_path(arg) {
        PathBuf::from(file_path)
    } else {
        PathBuf::from(arg)
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    };
    let metadata = fs::metadata(&absolute).ok()?;
    let dir = if metadata.is_dir() {
        absolute
    } else {
        absolute.parent()?.to_path_buf()
    };
    let canonical = fs::canonicalize(&dir).unwrap_or(dir);
    normalize_cwd_path(&canonical)
}

fn normalize_cwd_path(path: &Path) -> Option<String> {
    let cwd = path.to_string_lossy().into_owned();
    if cwd.is_empty() || cwd.contains('\0') || cwd.len() > MAX_LAUNCH_CWD_LEN {
        return None;
    }
    Some(cwd)
}

fn file_url_path(arg: &OsStr) -> Option<String> {
    let text = arg.to_str()?;
    let rest = text.strip_prefix("file://")?;
    let path = rest
        .strip_prefix("localhost/")
        .map(|path| format!("/{path}"))
        .unwrap_or_else(|| rest.to_string());
    if !path.starts_with('/') {
        return None;
    }
    percent_decode(&path)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex_value(*bytes.get(index + 1)?)?;
            let lo = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], current_dir: &Path) -> ParsedLaunch {
        parse_launch_args(args.iter().map(OsString::from), current_dir)
    }

    #[test]
    fn no_args_keeps_remembered_tabs_enabled() {
        let state = parsed_args_to_state(ParsedLaunch::default());

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
        assert!(!state.context().command_available);
    }

    #[test]
    fn positional_directory_starts_ephemeral_launch_in_that_cwd() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["src"], &cwd);
        let state = parsed_args_to_state(parsed);

        assert!(!state.context().remember_tabs);
        assert!(state.context().cwd.as_deref().unwrap().ends_with("src"));
        assert!(!state.context().command_available);
    }

    #[test]
    fn file_argument_uses_parent_directory() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["src/lib.rs"], &cwd);

        assert!(parsed.cwd.as_deref().unwrap().ends_with("src"));
    }

    #[test]
    fn working_directory_option_accepts_file_urls() {
        let cwd = env::current_dir().unwrap();
        let encoded = cwd.join("src").to_string_lossy().replace(' ', "%20");
        let parsed = parse(&[&format!("--cwd=file://{encoded}")], &cwd);

        assert!(parsed.cwd.as_deref().unwrap().ends_with("src"));
    }

    #[test]
    fn execute_args_are_backend_owned_and_disable_remembering() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--execute", "echo", "hello"], &cwd);
        let state = parsed_args_to_state(parsed);
        let cwd_text = cwd.to_string_lossy();

        assert!(!state.context().remember_tabs);
        assert_eq!(state.context().cwd.as_deref(), Some(cwd_text.as_ref()));
        assert!(state.context().command_available);
        assert_eq!(
            state.take_command(),
            Some(LaunchCommand {
                command: "echo".to_string(),
                args: vec!["hello".to_string()]
            })
        );
        assert_eq!(state.take_command(), None);
    }
}
