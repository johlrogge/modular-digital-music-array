//! Bandcamp API client
//!
//! Async HTTP client with rate limiting for interacting with Bandcamp.

use crate::error::BandcampError;
use crate::types::*;
use governor::{Quota, RateLimiter};
use music_facts::{Artist, Title, Year};
use nonzero_ext::nonzero;
use reqwest::{Client, Method};
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Bandcamp API client with rate limiting
pub struct BandcampClient {
    http: Client,
    rate_limiter: RateLimiter<
        governor::state::NotKeyed,
        governor::state::InMemoryState,
        governor::clock::DefaultClock,
    >,
}

impl BandcampClient {
    /// Create a new client with the given cookie jar
    pub fn new(cookies: Arc<reqwest::cookie::Jar>) -> Self {
        let http = Client::builder()
            .cookie_provider(cookies)
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0")
            .build()
            .expect("Failed to build HTTP client");

        // Rate limit: 3 requests per second
        let rate_limiter = RateLimiter::direct(Quota::per_second(nonzero!(3u32)));

        Self { http, rate_limiter }
    }

    /// Wait for rate limiter before making a request
    async fn wait_for_rate_limit(&self) {
        self.rate_limiter.until_ready().await;
    }

    /// Make a rate-limited HTTP request
    async fn request(&self, method: Method, url: &str) -> Result<reqwest::Response, BandcampError> {
        self.wait_for_rate_limit().await;

        let response = self.http.request(method, url).send().await?;

        if response.status().is_success() {
            Ok(response)
        } else if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            Err(BandcampError::RateLimited {
                retry_after_secs: 60,
            })
        } else if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            Err(BandcampError::NotLoggedIn)
        } else {
            Err(BandcampError::Http(
                response.error_for_status().unwrap_err(),
            ))
        }
    }

    /// Fetch the user's collection
    pub async fn get_collection(
        &self,
        username: &str,
    ) -> Result<Vec<CollectionItem>, BandcampError> {
        let url = format!("https://bandcamp.com/{}", username);
        tracing::debug!("Fetching collection for user: {}", username);

        let response = self.request(Method::GET, &url).await?;
        let body = response.text().await?;

        // Parse the page to extract the data blob
        let fanpage_data = self.parse_fanpage_data(&body)?;

        // Check if this is the user's own page
        if fanpage_data.fan_data.is_own_page != Some(true) {
            return Err(BandcampError::Auth(format!(
                "Not logged in as '{}'. Check your cookies.",
                username
            )));
        }

        // Get initial items from page
        let mut items = self.parse_collection_items(&fanpage_data)?;

        // Fetch remaining items via pagination API
        if let Some(token) = fanpage_data.collection_data.last_token.as_ref() {
            let fan_id = self.extract_fan_id(&fanpage_data.fan_data)?;
            let more_items = self.fetch_remaining_items(&fan_id, token).await?;
            items.extend(more_items);
        }

        tracing::info!("Found {} items in collection", items.len());
        Ok(items)
    }

    /// Parse the data blob from a user's collection page
    fn parse_fanpage_data(&self, html: &str) -> Result<ParsedFanpageData, BandcampError> {
        let document = Html::parse_document(html);
        let selector = Selector::parse("#pagedata")
            .map_err(|e| BandcampError::HtmlParse(format!("Failed to create selector: {:?}", e)))?;

        let element = document.select(&selector).next().ok_or_else(|| {
            BandcampError::HtmlParse("Could not find #pagedata element".to_string())
        })?;

        let data_blob = element
            .value()
            .attr("data-blob")
            .ok_or_else(|| BandcampError::HtmlParse("Missing data-blob attribute".to_string()))?;

        serde_json::from_str(data_blob)
            .map_err(|e| BandcampError::HtmlParse(format!("Failed to parse data blob: {}", e)))
    }

    /// Extract fan ID from parsed data
    fn extract_fan_id(&self, fan_data: &FanData) -> Result<String, BandcampError> {
        match &fan_data.fan_id {
            serde_json::Value::String(s) => Ok(s.clone()),
            serde_json::Value::Number(n) => Ok(n.to_string()),
            _ => Err(BandcampError::HtmlParse(
                "Invalid fan_id format".to_string(),
            )),
        }
    }

    /// Parse collection items from fanpage data
    fn parse_collection_items(
        &self,
        data: &ParsedFanpageData,
    ) -> Result<Vec<CollectionItem>, BandcampError> {
        let mut items = Vec::new();

        // Get download URLs
        let download_urls = data
            .collection_data
            .redownload_urls
            .as_ref()
            .cloned()
            .unwrap_or_default();

        // Match items to download URLs
        for (id, url) in download_urls {
            // Find the item in the cache
            if let Some(item) = data
                .item_cache
                .collection
                .values()
                .find(|i| format!("{}{}", i.sale_item_type, i.sale_item_id) == id)
            {
                items.push(CollectionItem {
                    id: ItemId::new(&id),
                    artist: Artist::new(&item.band_name),
                    title: Title::new(&item.item_title),
                    item_type: if item.sale_item_type == "t" {
                        ItemType::Track
                    } else {
                        ItemType::Album
                    },
                    purchased: self.parse_purchase_date(&item.purchased),
                    download_url: url,
                });
            }
        }

        Ok(items)
    }

    /// Parse purchase date string to DateTime
    fn parse_purchase_date(
        &self,
        date_str: &Option<String>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        date_str.as_ref().and_then(|s| {
            // Format: "30 Jan 2026 02:51:12 GMT"
            chrono::DateTime::parse_from_str(s, "%d %b %Y %H:%M:%S %Z")
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok()
        })
    }

    /// Fetch remaining collection items via pagination API
    async fn fetch_remaining_items(
        &self,
        fan_id: &str,
        initial_token: &str,
    ) -> Result<Vec<CollectionItem>, BandcampError> {
        let mut all_items = Vec::new();
        let mut last_token = initial_token.to_string();

        loop {
            let url = "https://bandcamp.com/api/fancollection/1/collection_items";
            let body = serde_json::json!({
                "fan_id": fan_id,
                "older_than_token": last_token,
                "count": 100
            });

            self.wait_for_rate_limit().await;

            let response = self.http.post(url).json(&body).send().await?;

            if !response.status().is_success() {
                return Err(BandcampError::CollectionFetch(format!(
                    "API returned status {}",
                    response.status()
                )));
            }

            let data: ParsedCollectionItems = response.json().await?;

            // Add items
            for (id, url) in data.redownload_urls {
                if let Some(item) = data
                    .items
                    .iter()
                    .find(|i| format!("{}{}", i.sale_item_type, i.sale_item_id) == id)
                {
                    all_items.push(CollectionItem {
                        id: ItemId::new(&id),
                        artist: Artist::new(&item.band_name),
                        title: Title::new(&item.item_title),
                        item_type: if item.sale_item_type == "t" {
                            ItemType::Track
                        } else {
                            ItemType::Album
                        },
                        purchased: self.parse_purchase_date(&item.purchased),
                        download_url: url,
                    });
                }
            }

            if !data.more_available {
                break;
            }

            last_token = data.last_token;
        }

        Ok(all_items)
    }

    /// Get detailed item information including download formats
    pub async fn get_item_details(&self, download_url: &str) -> Result<DigitalItem, BandcampError> {
        tracing::debug!("Fetching item details from: {}", download_url);

        let response = self.request(Method::GET, download_url).await?;
        let body = response.text().await?;

        self.parse_digital_item(&body)
    }

    /// Parse digital item from download page
    fn parse_digital_item(&self, html: &str) -> Result<DigitalItem, BandcampError> {
        let document = Html::parse_document(html);
        let selector = Selector::parse("#pagedata")
            .map_err(|e| BandcampError::HtmlParse(format!("Failed to create selector: {:?}", e)))?;

        let element = document.select(&selector).next().ok_or_else(|| {
            BandcampError::HtmlParse("Could not find #pagedata element".to_string())
        })?;

        let data_blob = element
            .value()
            .attr("data-blob")
            .ok_or_else(|| BandcampError::HtmlParse("Missing data-blob attribute".to_string()))?;

        let parsed: serde_json::Value = serde_json::from_str(data_blob)?;

        // Extract digital_items array
        let digital_items = parsed
            .get("digital_items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| BandcampError::HtmlParse("No digital_items found".to_string()))?;

        let item_data: ParsedDigitalItem =
            serde_json::from_value(digital_items.first().cloned().ok_or_else(|| {
                BandcampError::HtmlParse("Empty digital_items array".to_string())
            })?)?;

        // Parse formats
        let mut formats = HashMap::new();
        if let Some(downloads) = item_data.downloads {
            for (format_name, download) in downloads {
                if let Some(format) = AudioFormat::from_bandcamp_name(&format_name) {
                    formats.insert(format, download.url);
                }
            }
        }

        // Determine item type
        let item_type = if item_data.download_type == Some("t".to_string())
            || item_data.download_type_str == "track"
            || item_data.item_type == "track"
        {
            ItemType::Track
        } else {
            ItemType::Album
        };

        // Parse release year
        let release_year = item_data
            .package_release_date
            .as_ref()
            .and_then(|d| d.split('-').next().and_then(|y| y.parse().ok()).map(Year));

        Ok(DigitalItem {
            artist: Artist::new(&item_data.artist),
            title: Title::new(&item_data.title),
            item_type,
            release_year,
            formats,
            tracks: Vec::new(), // TODO: Parse track list if needed
        })
    }

    /// Download an item to the specified path
    pub async fn download_item<F>(
        &self,
        item: &DigitalItem,
        format: AudioFormat,
        dest: &Path,
        progress: F,
    ) -> Result<std::path::PathBuf, BandcampError>
    where
        F: Fn(DownloadProgress),
    {
        let url = item
            .formats
            .get(&format)
            .ok_or_else(|| BandcampError::Download(format!("Format {} not available", format)))?;

        tracing::info!("Downloading {} - {} as {}", item.artist, item.title, format);

        // Create parent directory
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Start download
        self.wait_for_rate_limit().await;
        let response = self.http.get(url).send().await?;

        if !response.status().is_success() {
            return Err(BandcampError::Download(format!(
                "Download failed with status {}",
                response.status()
            )));
        }

        let total_size = response.content_length();
        let mut downloaded: u64 = 0;

        // Stream to file
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = response.bytes_stream();

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;

            downloaded += chunk.len() as u64;
            progress(DownloadProgress {
                downloaded,
                total: total_size,
            });
        }

        file.flush().await?;

        tracing::info!("Download complete: {:?}", dest);
        Ok(dest.to_path_buf())
    }
}
