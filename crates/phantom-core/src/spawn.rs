//! Spawn orchestration shared by every front-end.
//!
//! This is the trust boundary for launching shells: callers pass a *profile id*
//! and an optional working directory, never a command line. The command/args
//! come exclusively from a stored, validated [`ShellProfile`]. Keeping this in
//! `phantom-core` means the native app and the Tauri build enforce identical
//! rules.

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};
use crate::pty::LaunchOpts;

const MAX_SPAWN_CWD_LEN: usize = 4096;

/// The user's home directory, used as the final working-directory fallback and
/// for rendering `~`-relative tab names. Returns `None` if it cannot be resolved.
pub fn default_home_dir() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().into_owned())
}

/// Resolve a stored shell profile into concrete [`LaunchOpts`].
///
/// `profile_id` selects a profile (falling back to the configured default, then
/// the first profile). `cwd` overrides the profile's working directory when
/// non-empty; otherwise the profile cwd, then the home directory, are used. The
/// resolved cwd is validated before it can reach `openpty`.
pub fn resolve_launch_opts(
    config: &AppConfig,
    profile_id: Option<&str>,
    cwd: Option<String>,
    rows: u16,
    cols: u16,
) -> AppResult<LaunchOpts> {
    let profile = config
        .profile(profile_id)
        .ok_or_else(|| AppError::InvalidConfig("no shell profile available".to_string()))?;
    let cwd = cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .or_else(|| profile.cwd.clone())
        .or_else(default_home_dir);
    validate_spawn_cwd(cwd.as_deref())?;

    Ok(LaunchOpts {
        command: if profile.command.trim().is_empty() {
            None
        } else {
            Some(profile.command.clone())
        },
        args: profile.args.clone(),
        cwd,
        rows,
        cols,
    })
}

fn validate_spawn_cwd(cwd: Option<&str>) -> AppResult<()> {
    if let Some(cwd) = cwd {
        if cwd.len() > MAX_SPAWN_CWD_LEN {
            return Err(AppError::InvalidConfig(
                "working directory path is too long".to_string(),
            ));
        }
        if cwd.contains('\0') {
            return Err(AppError::InvalidConfig(
                "working directory cannot contain NUL bytes".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_spawn_cwd_accepts_none_and_normal_paths() {
        validate_spawn_cwd(None).unwrap();
        validate_spawn_cwd(Some("/Users/steve/projects/phantom-terminal")).unwrap();
    }

    #[test]
    fn validate_spawn_cwd_rejects_too_long_paths() {
        let cwd = "a".repeat(MAX_SPAWN_CWD_LEN + 1);

        let error = validate_spawn_cwd(Some(&cwd)).unwrap_err().to_string();

        assert_eq!(error, "invalid config: working directory path is too long");
    }

    #[test]
    fn validate_spawn_cwd_rejects_nul_bytes() {
        let error = validate_spawn_cwd(Some("/tmp\0/phantom"))
            .unwrap_err()
            .to_string();

        assert_eq!(
            error,
            "invalid config: working directory cannot contain NUL bytes"
        );
    }

    #[test]
    fn resolve_launch_opts_uses_default_profile_and_passes_dimensions() {
        let config = AppConfig::default();
        let opts = resolve_launch_opts(&config, None, Some("/tmp".to_string()), 40, 120).unwrap();

        // Default profile has an empty command (meaning "use $SHELL").
        assert_eq!(opts.command, None);
        assert!(opts.args.is_empty());
        assert_eq!(opts.cwd.as_deref(), Some("/tmp"));
        assert_eq!((opts.rows, opts.cols), (40, 120));
    }

    #[test]
    fn resolve_launch_opts_rejects_invalid_cwd() {
        let config = AppConfig::default();
        let error = resolve_launch_opts(&config, None, Some("/bad\0".to_string()), 24, 80)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid config: working directory cannot contain NUL bytes"
        );
    }
}
