use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("{0}")]
    Other(String),
}

pub type AppResult<T> = Result<T, AppError>;

/// Tauri commands return `Result<_, String>` because the error crosses the IPC
/// boundary to the webview. Keep those strings stable and user-safe; the full
/// internal detail is logged for local debugging.
pub fn command_error(error: AppError) -> String {
    eprintln!("IPC command failed: {error}");
    match error {
        AppError::Pty(message) => format!("terminal error: {message}"),
        AppError::InvalidConfig(message) => format!("invalid config: {message}"),
        AppError::Io(_) => "filesystem operation failed".to_string(),
        AppError::Sqlite(_) => "session database operation failed".to_string(),
        AppError::Json(_) => "stored data could not be decoded".to_string(),
        AppError::Other(_) => "operation failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_hides_io_details_from_ipc() {
        let error = AppError::Io(std::io::Error::other(
            "permission denied: /Users/steve/Library/Application Support/phantom/terminal/phantom.db",
        ));

        let visible = command_error(error);

        assert_eq!(visible, "filesystem operation failed");
        assert!(!visible.contains("/Users/steve"));
    }

    #[test]
    fn command_error_hides_sqlite_details_from_ipc() {
        let error = AppError::Sqlite(rusqlite::Error::InvalidPath(
            "/Users/steve/private/phantom.db".into(),
        ));

        let visible = command_error(error);

        assert_eq!(visible, "session database operation failed");
        assert!(!visible.contains("/Users/steve"));
    }

    #[test]
    fn command_error_preserves_user_facing_config_and_pty_messages() {
        assert_eq!(
            command_error(AppError::InvalidConfig(
                "font size must be between 8 and 48".into()
            )),
            "invalid config: font size must be between 8 and 48"
        );
        assert_eq!(
            command_error(AppError::Pty("too many terminals".into())),
            "terminal error: too many terminals"
        );
    }
}
