//! Error types for Bandcamp API

use thiserror::Error;

/// Errors that can occur when interacting with the Bandcamp API
#[derive(Debug, Error)]
pub enum BandcampError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Failed to parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Failed to parse HTML: {0}")]
    HtmlParse(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Cookie file not found: {path}")]
    CookieFileNotFound { path: String },

    #[error("Invalid cookie format: {0}")]
    InvalidCookieFormat(String),

    #[error("Missing required cookie: {name}")]
    MissingCookie { name: String },

    #[error("Collection fetch failed: {0}")]
    CollectionFetch(String),

    #[error("Download failed: {0}")]
    Download(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP extraction failed: {0}")]
    ZipExtraction(String),

    #[error("Rate limited, retry after {retry_after_secs} seconds")]
    RateLimited { retry_after_secs: u64 },

    #[error("Not logged in - cookies may have expired")]
    NotLoggedIn,
}
