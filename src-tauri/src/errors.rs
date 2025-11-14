use std::io;

use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("failed to resolve required path: {0}")]
    Path(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Keychain(#[from] keyring::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    #[error("parse error: {0}")]
    Parse(String),
}
