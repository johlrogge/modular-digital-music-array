//! Bandcamp API client for MDMA
//!
//! This crate provides an async HTTP client for interacting with Bandcamp's
//! API to fetch collection information and download purchased music.
//!
//! # Features
//!
//! - Rate-limited requests (3 req/sec)
//! - Cookie-based authentication
//! - Collection pagination
//! - Multiple audio format support (FLAC, WAV, MP3, etc.)
//! - Progress tracking for downloads
//!
//! # Example
//!
//! ```ignore
//! use bandcamp_api::{BandcampClient, load_cookies, AudioFormat};
//! use std::path::Path;
//!
//! let cookies = load_cookies(Path::new("/etc/mdma/bandcamp-cookies.json"))?;
//! let client = BandcampClient::new(cookies);
//!
//! // Fetch collection
//! let collection = client.get_collection("username").await?;
//!
//! // Download an item
//! for item in &collection {
//!     let details = client.get_item_details(&item.download_url).await?;
//!     client.download_item(&details, AudioFormat::Flac, Path::new("./music"), |p| {
//!         println!("Progress: {:?}", p.percentage());
//!     }).await?;
//! }
//! ```

mod client;
mod cookies;
mod error;
mod types;

pub use client::BandcampClient;
pub use cookies::load_cookies;
pub use error::BandcampError;
pub use types::{
    AudioFormat, CollectionItem, DigitalItem, DownloadProgress, FanId, ItemId, ItemType, TrackInfo,
};

// Re-export commonly used types from music_facts
pub use music_facts::{Album, Artist, Title, Year};
