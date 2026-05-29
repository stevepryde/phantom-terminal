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
/// boundary to the webview. This flattens an `AppError` into that string.
pub fn command_error(error: AppError) -> String {
    error.to_string()
}
