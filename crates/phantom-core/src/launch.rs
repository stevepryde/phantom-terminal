use serde::Serialize;

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

const MAX_LAUNCH_CWD_LEN: usize = 4096;

#[derive(Clone, Debug, Serialize)]
pub struct LaunchContext {
    pub cwd: Option<String>,
    pub remember_tabs: bool,
}

pub struct LaunchState {
    context: LaunchContext,
}

impl LaunchState {
    pub fn from_env() -> Self {
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        parsed_args_to_state(parse_launch_args_with_home(
            env::args_os().skip(1),
            &current_dir,
            default_home_dir().as_deref(),
        ))
    }

    pub fn context(&self) -> LaunchContext {
        self.context.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedLaunch {
    cwd: Option<String>,
    remember_tabs: bool,
}

impl Default for ParsedLaunch {
    fn default() -> Self {
        Self {
            cwd: None,
            remember_tabs: true,
        }
    }
}

fn parsed_args_to_state(parsed: ParsedLaunch) -> LaunchState {
    LaunchState {
        context: LaunchContext {
            cwd: parsed.cwd,
            remember_tabs: parsed.remember_tabs,
        },
    }
}

fn parse_launch_args_with_home<I>(
    args: I,
    current_dir: &Path,
    _home_dir: Option<&Path>,
) -> ParsedLaunch
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    if args.iter().any(|arg| arg == "--normal") {
        return ParsedLaunch {
            cwd: None,
            remember_tabs: true,
        };
    }

    let mut saw_empty_cwd_request = false;
    for (index, arg) in args.iter().enumerate() {
        if arg == "--cwd" {
            if let Some(path) = args
                .get(index + 1)
                .filter(|path| !option_like(path) && !path.is_empty())
            {
                return ParsedLaunch {
                    cwd: resolve_launch_cwd(path, current_dir),
                    remember_tabs: false,
                };
            }
            saw_empty_cwd_request = true;
        }

        if let Some(value) = option_value(arg, "--cwd=") {
            if value.is_empty() {
                saw_empty_cwd_request = true;
                continue;
            }
            return ParsedLaunch {
                cwd: resolve_launch_cwd(&OsString::from(value), current_dir),
                remember_tabs: false,
            };
        }
    }

    if saw_empty_cwd_request {
        return ParsedLaunch::default();
    }

    ParsedLaunch::default()
}

fn option_like(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|text| text.starts_with('-'))
}

fn option_value(arg: &OsStr, prefix: &str) -> Option<String> {
    let text = arg.to_str()?;
    text.strip_prefix(prefix).map(ToOwned::to_owned)
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

fn default_home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
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

    fn parse(args: &[&str], current_dir: &Path, home_dir: Option<&Path>) -> ParsedLaunch {
        parse_launch_args_with_home(args.iter().map(OsString::from), current_dir, home_dir)
    }

    #[test]
    fn no_args_in_home_keeps_remembered_tabs_enabled() {
        let cwd = env::current_dir().unwrap();
        let state = parsed_args_to_state(parse(&[], &cwd, Some(&cwd)));

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn no_args_at_root_keeps_remembered_tabs_enabled() {
        let state = parsed_args_to_state(parse(&[], Path::new("/"), None));

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn no_args_in_non_home_directory_keeps_remembered_tabs_enabled() {
        let cwd = env::current_dir().unwrap();
        let state = parsed_args_to_state(parse(&[], &cwd, cwd.parent()));

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn cwd_option_starts_ephemeral_launch_in_that_cwd() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd", "src"], &cwd, Some(&cwd));
        let state = parsed_args_to_state(parsed);

        assert!(!state.context().remember_tabs);
        assert!(state.context().cwd.as_deref().unwrap().ends_with("src"));
    }

    #[test]
    fn bare_cwd_option_keeps_remembered_tabs_enabled() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd"], &cwd, cwd.parent());
        let state = parsed_args_to_state(parsed);

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn empty_cwd_option_value_keeps_remembered_tabs_enabled() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd="], &cwd, cwd.parent());
        let state = parsed_args_to_state(parsed);

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn cwd_option_with_invalid_path_still_disables_remembering() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd", "src/does-not-exist"], &cwd, Some(&cwd));
        let state = parsed_args_to_state(parsed);

        assert!(!state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn file_argument_uses_parent_directory() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd", "src/lib.rs"], &cwd, Some(&cwd));

        assert!(parsed.cwd.as_deref().unwrap().ends_with("src"));
    }

    #[test]
    fn cwd_option_accepts_file_urls() {
        let cwd = env::current_dir().unwrap();
        let encoded = cwd.join("src").to_string_lossy().replace(' ', "%20");
        let parsed = parse(&[&format!("--cwd=file://{encoded}")], &cwd, Some(&cwd));

        assert!(parsed.cwd.as_deref().unwrap().ends_with("src"));
    }

    #[test]
    fn raw_path_argument_no_longer_triggers_ephemeral_mode() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["src"], &cwd, Some(&cwd));
        let state = parsed_args_to_state(parsed);

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }

    #[test]
    fn normal_option_forces_remembered_tabs_even_with_cwd() {
        let cwd = env::current_dir().unwrap();
        let parsed = parse(&["--cwd", "src", "--normal"], &cwd, cwd.parent());
        let state = parsed_args_to_state(parsed);

        assert!(state.context().remember_tabs);
        assert_eq!(state.context().cwd, None);
    }
}
