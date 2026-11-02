//! Bandcamp API client
//!
//! Async HTTP client with rate limiting for interacting with Bandcamp.

use crate::error::BandcampError;
use crate::html::extract_data_blob;
use crate::playlist::{
    assemble_playlist, parse_more_tracks, parse_playlist_page, parse_playlists_response, Playlist,
    PlaylistSummary,
};
use crate::types::*;
use governor::{Quota, RateLimiter};
use music_facts::{Artist, Title, Year};
use nonzero_ext::nonzero;
use reqwest::{Client, Method};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Upper bound on paginated requests for a single listing, to avoid runaway loops
const MAX_PAGINATION_ITERATIONS: usize = 100;

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
        self.send_checked(self.http.request(method, url)).await
    }

    /// Rate-limit, send, and map non-success statuses to errors
    async fn send_checked(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, BandcampError> {
        self.wait_for_rate_limit().await;

        let response = builder.send().await?;

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

        Self::require_own_page(&fanpage_data.fan_data, username)?;

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

    /// Fail unless the fan page was served as the logged-in user's own page
    fn require_own_page(fan_data: &FanData, username: &str) -> Result<(), BandcampError> {
        if fan_data.is_own_page != Some(true) {
            return Err(BandcampError::Auth(format!(
                "Not logged in as '{}'. Check your cookies.",
                username
            )));
        }
        Ok(())
    }

    /// Parse the data blob from a user's collection page
    fn parse_fanpage_data(&self, html: &str) -> Result<ParsedFanpageData, BandcampError> {
        let data_blob = extract_data_blob(html, "pagedata")?;

        serde_json::from_str(&data_blob)
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

        // Get download URLs — iterate redownload_urls as primary source, enrich with metadata
        let download_urls = data
            .collection_data
            .redownload_urls
            .as_ref()
            .cloned()
            .unwrap_or_default();

        for (id, url) in &download_urls {
            // Find the item in the cache — try both direct key and constructed key lookup
            let cached = data.item_cache.collection.get(id).or_else(|| {
                data.item_cache
                    .collection
                    .values()
                    .find(|i| format!("{}{}", i.sale_item_type, i.sale_item_id) == *id)
            });

            if let Some(item) = cached {
                items.push(CollectionItem {
                    id: ItemId::new(id),
                    artist: Artist::new(&item.band_name),
                    title: Title::new(&item.item_title),
                    item_type: if item.sale_item_type == "t" {
                        ItemType::Track
                    } else {
                        ItemType::Album
                    },
                    purchased: self.parse_purchase_date(&item.purchased),
                    download_url: url.clone(),
                });
            } else {
                tracing::warn!("Item {} has download URL but no cache entry", id);
                items.push(CollectionItem {
                    id: ItemId::new(id),
                    artist: Artist::new("Unknown"),
                    title: Title::new("Unknown"),
                    item_type: ItemType::Album,
                    purchased: None,
                    download_url: url.clone(),
                });
            }
        }

        // Log gap detection
        if let Some(expected) = data.collection_data.item_count {
            let actual = items.len();
            if actual < expected as usize {
                tracing::info!(
                    "Initial page has {}/{} items, remaining will be fetched via pagination",
                    actual,
                    expected
                );
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

            // Match items to download URLs (pagination API items have consistent key format)
            for item in &data.items {
                let constructed_key = format!("{}{}", item.sale_item_type, item.sale_item_id);
                if let Some(url) = data.redownload_urls.get(&constructed_key) {
                    all_items.push(CollectionItem {
                        id: ItemId::new(&constructed_key),
                        artist: Artist::new(&item.band_name),
                        title: Title::new(&item.item_title),
                        item_type: if item.sale_item_type == "t" {
                            ItemType::Track
                        } else {
                            ItemType::Album
                        },
                        purchased: self.parse_purchase_date(&item.purchased),
                        download_url: url.clone(),
                    });
                } else {
                    tracing::warn!(
                        "Paginated item without download URL: {} - {} ({})",
                        item.band_name,
                        item.item_title,
                        constructed_key
                    );
                }
            }

            if !data.more_available {
                break;
            }

            last_token = data.last_token;
        }

        Ok(all_items)
    }

    /// Build a playlist URL from either a slug (e.g. `november-2`) or a full URL.
    ///
    /// Full URLs (with or without scheme) must be on `bandcamp.com` or one of its
    /// subdomains, so the service never fetches arbitrary hosts.
    pub fn playlist_url(username: &str, slug_or_url: &str) -> Result<String, BandcampError> {
        let arg = slug_or_url.trim();
        let has_scheme = arg.starts_with("http://") || arg.starts_with("https://");
        // Slugs never contain dots; a dotted first segment is a scheme-less host
        let first_segment = arg.split('/').next().unwrap_or_default();
        if !has_scheme && !first_segment.contains('.') {
            return Ok(format!(
                "https://bandcamp.com/{}/playlist/{}",
                username,
                arg.trim_matches('/')
            ));
        }

        let candidate = if has_scheme {
            arg.to_string()
        } else {
            format!("https://{}", arg)
        };
        let invalid = || BandcampError::InvalidUrl(slug_or_url.to_string());
        let parsed = reqwest::Url::parse(&candidate).map_err(|_| invalid())?;
        // Cookies carry no Secure flag: never send them over cleartext, to a
        // custom port, or with embedded credentials.
        if parsed.scheme() != "https"
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(invalid());
        }
        let host = parsed.host_str().ok_or_else(invalid)?;
        if host == "bandcamp.com" || host.ends_with(".bandcamp.com") {
            Ok(parsed.to_string())
        } else {
            Err(invalid())
        }
    }

    /// POST a JSON body (rate-limited) and return the response text
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<String, BandcampError> {
        let builder = self
            .http
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/json; charset=UTF-8",
            )
            .body(body.to_string());
        Ok(self.send_checked(builder).await?.text().await?)
    }

    /// Verify a pagination cursor advanced and the iteration cap was not hit
    fn check_cursor_progress(
        previous: Option<u64>,
        next: u64,
        iterations: usize,
    ) -> Result<(), BandcampError> {
        if previous == Some(next) {
            return Err(BandcampError::CollectionFetch(format!(
                "Pagination cursor {} did not advance",
                next
            )));
        }
        if iterations >= MAX_PAGINATION_ITERATIONS {
            return Err(BandcampError::CollectionFetch(format!(
                "Pagination exceeded {} requests",
                MAX_PAGINATION_ITERATIONS
            )));
        }
        Ok(())
    }

    /// List the fan's playlists (including private ones when logged in)
    pub async fn list_playlists(
        &self,
        username: &str,
    ) -> Result<Vec<PlaylistSummary>, BandcampError> {
        let url = format!("https://bandcamp.com/{}", username);
        tracing::debug!("Fetching fan page for playlists: {}", username);
        let body = self.request(Method::GET, &url).await?.text().await?;
        let fanpage_data = self.parse_fanpage_data(&body)?;
        Self::require_own_page(&fanpage_data.fan_data, username)?;
        let fan_id = self.extract_fan_id(&fanpage_data.fan_data)?;
        let fan_id_json = fan_id
            .parse::<u64>()
            .map(serde_json::Value::from)
            .unwrap_or_else(|_| serde_json::Value::String(fan_id.clone()));

        let mut playlists = Vec::new();
        let mut cursor: Option<u64> = None;
        let mut iterations = 0;
        loop {
            let mut req = serde_json::json!({ "page_size": 20, "page_fan_id": fan_id_json });
            if let Some(c) = cursor {
                req["next_cursor"] = c.into();
            }
            let text = self
                .post_json("https://bandcamp.com/api/fan_collection/1/playlists", &req)
                .await?;
            let page = parse_playlists_response(&text)?;
            playlists.extend(page.playlists);
            iterations += 1;

            match page.next_cursor {
                None => break,
                Some(next) => {
                    Self::check_cursor_progress(cursor, next, iterations)?;
                    cursor = Some(next);
                }
            }
        }

        tracing::info!("Found {} playlists for {}", playlists.len(), username);
        Ok(playlists)
    }

    /// Fetch a playlist (page plus all further track chunks) by URL
    pub async fn get_playlist(&self, url: &str) -> Result<Playlist, BandcampError> {
        tracing::debug!("Fetching playlist: {}", url);
        let body = self.request(Method::GET, url).await?.text().await?;
        let page = parse_playlist_page(&body)?;

        let mut more = Vec::new();
        let mut cursor = page.chunk.next_cursor;
        let mut previous: Option<u64> = None;
        let mut iterations = 0;
        while let Some(next) = cursor {
            Self::check_cursor_progress(previous, next, iterations)?;
            let req = serde_json::json!({
                "item_type": "playlist",
                "item_id": page.id,
                "next_cursor": next,
            });
            let text = self
                .post_json("https://bandcamp.com/api/player/2/player_data_web", &req)
                .await?;
            let chunk = parse_more_tracks(&text)?;
            iterations += 1;
            previous = Some(next);
            cursor = chunk.next_cursor;
            more.push(chunk);
        }

        Ok(assemble_playlist(page, more))
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
        let data_blob = extract_data_blob(html, "pagedata")?;

        let parsed: serde_json::Value = serde_json::from_str(&data_blob)?;

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
        let release_year = item_data.package_release_date.as_ref().and_then(|d| {
            d.split('-')
                .next()
                .and_then(|y| y.parse().ok())
                .map(Year::new)
        });

        Ok(DigitalItem {
            artist: Artist::new(&item_data.artist),
            title: Title::new(&item_data.title),
            item_type,
            release_year,
            formats,
            tracks: Vec::new(), // TODO: Parse track list if needed
        })
    }

    /// Download an item to the specified path, returning a stream of progress events.
    ///
    /// `download_page_url` is the Bandcamp download page URL (from redownload_urls),
    /// used to refresh expired download signatures if needed.
    pub fn download_item(
        &self,
        item: &DigitalItem,
        format: AudioFormat,
        dest: &Path,
        download_page_url: &str,
    ) -> impl tokio_stream::Stream<Item = DownloadEvent> + '_ {
        let url = item.formats.get(&format).cloned();
        let dest = dest.to_path_buf();
        let artist = item.artist.clone();
        let title = item.title.clone();
        let page_url = download_page_url.to_string();

        async_stream::stream! {
            let mut url = match url {
                Some(u) => u,
                None => {
                    yield DownloadEvent::Failed {
                        error: format!("Format {} not available", format),
                    };
                    return;
                }
            };

            tracing::info!("Downloading {} - {} as {}", artist, title, format);

            // Create parent directory
            if let Some(parent) = dest.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    yield DownloadEvent::Failed {
                        error: format!("Failed to create directory: {}", e),
                    };
                    return;
                }
            }

            // Resolve the actual download URL via Bandcamp's statdownload endpoint.
            // The format URL from digital_items is not a direct download — it needs
            // to go through /statdownload/ to get the real CDN URL with a valid token.
            let download_url = match Self::resolve_download_url(&self.http, &url).await {
                Ok(u) => u,
                Err(BandcampError::DownloadExpired) => {
                    // Sig expired — re-fetch the download page for a fresh sig
                    tracing::info!("Download sig expired, re-fetching download page for {} - {}", artist, title);
                    self.wait_for_rate_limit().await;
                    match self.get_item_details(&page_url).await {
                        Ok(fresh_item) => {
                            match fresh_item.formats.get(&format) {
                                Some(fresh_url) => {
                                    url = fresh_url.clone();
                                    match Self::resolve_download_url(&self.http, &url).await {
                                        Ok(u) => u,
                                        Err(BandcampError::DownloadExpired) => {
                                            yield DownloadEvent::Failed {
                                                error: format!(
                                                    "Download links for '{} - {}' have permanently expired. \
                                                     Visit bandcamp.com, go to your collection, click this item, \
                                                     and re-request the download link (you may need to enter your \
                                                     purchase email). Then re-sync.",
                                                    artist, title
                                                ),
                                            };
                                            return;
                                        }
                                        Err(e) => {
                                            yield DownloadEvent::Failed {
                                                error: format!("Failed to resolve after refresh: {}", e),
                                            };
                                            return;
                                        }
                                    }
                                }
                                None => {
                                    yield DownloadEvent::Failed {
                                        error: format!("Format {} not available after refresh", format),
                                    };
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            yield DownloadEvent::Failed {
                                error: format!("Failed to refresh download page: {}", e),
                            };
                            return;
                        }
                    }
                }
                Err(e) => {
                    yield DownloadEvent::Failed {
                        error: format!("Failed to resolve download URL: {}", e),
                    };
                    return;
                }
            };

            tracing::debug!("Resolved download URL: {}", download_url);

            // Download the actual file
            self.wait_for_rate_limit().await;
            let response = match self.http.get(&download_url).send().await {
                Ok(r) => r,
                Err(e) => {
                    yield DownloadEvent::Failed {
                        error: format!("HTTP request failed: {}", e),
                    };
                    return;
                }
            };

            if !response.status().is_success() {
                yield DownloadEvent::Failed {
                    error: format!("Download failed with status {}", response.status()),
                };
                return;
            }

            // Verify we got audio, not HTML
            let content_type = response.headers().get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            if content_type.contains("text/html") {
                yield DownloadEvent::Failed {
                    error: format!("Got HTML instead of audio from resolved URL (content-type: {})", content_type),
                };
                return;
            }

            let total_size = response.content_length();
            yield DownloadEvent::Started { total: total_size };

            let mut downloaded: u64 = 0;

            // Stream to file
            let mut file = match tokio::fs::File::create(&dest).await {
                Ok(f) => f,
                Err(e) => {
                    yield DownloadEvent::Failed {
                        error: format!("Failed to create file: {}", e),
                    };
                    return;
                }
            };

            let mut stream = response.bytes_stream();

            use futures_util::StreamExt;
            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield DownloadEvent::Failed {
                            error: format!("Download error: {}", e),
                        };
                        return;
                    }
                };

                if let Err(e) = file.write_all(&chunk).await {
                    yield DownloadEvent::Failed {
                        error: format!("Write error: {}", e),
                    };
                    return;
                }

                downloaded += chunk.len() as u64;
                yield DownloadEvent::Progress(DownloadProgress {
                    downloaded,
                    total: total_size,
                });
            }

            if let Err(e) = file.flush().await {
                yield DownloadEvent::Failed {
                    error: format!("Flush error: {}", e),
                };
                return;
            }

            tracing::info!("Download complete: {:?}", dest);
            yield DownloadEvent::Completed { path: dest };
        }
    }

    /// Resolve a Bandcamp format URL to the actual CDN download URL via the statdownload endpoint.
    ///
    /// Bandcamp's download URLs from digital_items are not direct links to audio files.
    /// They must go through a /statdownload/ endpoint which returns JSON with the real URL.
    async fn resolve_download_url(
        http: &Client,
        format_url: &str,
    ) -> Result<String, BandcampError> {
        // Transform /download/ to /statdownload/ and add required params
        let stat_url = format_url.replace("/download/", "/statdownload/");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let stat_url = format!("{}&.rand={}&.vrs=1", stat_url, now_ms);

        tracing::debug!("Calling statdownload: {}", stat_url);

        let response = http.get(&stat_url).send().await?;

        if !response.status().is_success() {
            return Err(BandcampError::Download(format!(
                "statdownload returned status {}",
                response.status()
            )));
        }

        let body = response.text().await?;
        tracing::debug!(
            "statdownload response body (first 500 chars): {}",
            &body[..body.len().min(500)]
        );

        // The response is JavaScript: `if ( window.Downloads ) { Downloads.statResult( {...} ) };`
        // Extract the JSON object from inside statResult( ... )
        let json_str = if body.trim_start().starts_with('{') {
            body.clone()
        } else if let Some(start) = body.find("statResult") {
            // Find the JSON object after "statResult(" or "statResult ("
            let after_stat = &body[start..];
            let paren_start = after_stat.find('(').ok_or_else(|| {
                BandcampError::Download("statResult without opening paren".to_string())
            })?;
            let json_start = after_stat[paren_start..].find('{').ok_or_else(|| {
                BandcampError::Download("No JSON object after statResult(".to_string())
            })?;
            let json_begin = start + paren_start + json_start;
            // Find the matching closing brace
            let remaining = &body[json_begin..];
            let mut depth = 0;
            let mut end = 0;
            for (i, c) in remaining.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end == 0 {
                return Err(BandcampError::Download(
                    "Unmatched braces in statResult".to_string(),
                ));
            }
            remaining[..=end].to_string()
        } else {
            return Err(BandcampError::Download(format!(
                "statdownload returned unexpected response: {}",
                &body[..body.len().min(200)]
            )));
        };

        let parsed: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| {
            BandcampError::Download(format!(
                "Failed to parse statdownload JSON: {} — body: {}",
                e,
                &json_str[..json_str.len().min(300)]
            ))
        })?;

        // Check for error result with retry_url
        if let Some(result) = parsed.get("result").and_then(|v| v.as_str()) {
            if result == "ok" {
                if let Some(url) = parsed.get("download_url").and_then(|v| v.as_str()) {
                    return Ok(url.to_string());
                }
            } else if result == "err" {
                if let Some(retry_url) = parsed
                    .get("retry_url")
                    .and_then(|v| v.as_str())
                    .filter(|u| !u.is_empty())
                {
                    tracing::debug!("statdownload returned retry_url, retrying: {}", retry_url);
                    // Retry with the provided URL
                    let retry_response = http.get(retry_url).send().await?;
                    let retry_body = retry_response.text().await?;
                    let retry_json: serde_json::Value =
                        serde_json::from_str(&if retry_body.trim_start().starts_with('{') {
                            retry_body
                        } else {
                            retry_body
                                .find('{')
                                .and_then(|s| {
                                    retry_body.rfind('}').map(|e| retry_body[s..=e].to_string())
                                })
                                .unwrap_or(retry_body)
                        })?;

                    if let Some(url) = retry_json.get("download_url").and_then(|v| v.as_str()) {
                        return Ok(url.to_string());
                    }
                }
                let error_type = parsed
                    .get("errortype")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                if error_type == "ExpirationError" {
                    tracing::info!("statdownload returned ExpirationError — sig expired");
                    return Err(BandcampError::DownloadExpired);
                }
                return Err(BandcampError::Download(format!(
                    "statdownload returned error: {}",
                    error_type
                )));
            }
        }

        Err(BandcampError::Download(format!(
            "Unexpected statdownload response: {}",
            &json_str[..json_str.len().min(300)]
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_url_from_slug() {
        assert_eq!(
            BandcampClient::playlist_url("testuser", "november-2").unwrap(),
            "https://bandcamp.com/testuser/playlist/november-2"
        );
    }

    #[test]
    fn playlist_url_passes_full_url_through() {
        let url = "https://bandcamp.com/testuser/playlist/november-2";
        assert_eq!(BandcampClient::playlist_url("other", url).unwrap(), url);
    }

    #[test]
    fn playlist_url_accepts_subdomain() {
        let url = "https://artist.bandcamp.com/album/x";
        assert_eq!(BandcampClient::playlist_url("u", url).unwrap(), url);
    }

    #[test]
    fn playlist_url_scheme_less_is_treated_as_url() {
        assert_eq!(
            BandcampClient::playlist_url("other", "bandcamp.com/testuser/playlist/n-2").unwrap(),
            "https://bandcamp.com/testuser/playlist/n-2"
        );
    }

    #[test]
    fn playlist_url_rejects_non_bandcamp_hosts() {
        for input in [
            "https://example.com/testuser/playlist/x",
            "https://evilbandcamp.com/x",
            "https://bandcamp.com.evil.com/x",
            "https://bandcamp.com@evil.com/x",
            "evil.com/testuser/playlist/x",
            "http://127.0.0.1:8080/x",
            "http://bandcamp.com/x",
            "https://bandcamp.com:8080/x",
            "https://u:p@bandcamp.com/x",
        ] {
            assert!(
                matches!(
                    BandcampClient::playlist_url("u", input),
                    Err(BandcampError::InvalidUrl(_))
                ),
                "should reject {input}"
            );
        }
    }

    #[test]
    fn own_page_required() {
        let own = FanData {
            fan_id: serde_json::Value::from(1),
            is_own_page: Some(true),
        };
        assert!(BandcampClient::require_own_page(&own, "u").is_ok());
        for flag in [Some(false), None] {
            let other = FanData {
                fan_id: serde_json::Value::from(1),
                is_own_page: flag,
            };
            assert!(matches!(
                BandcampClient::require_own_page(&other, "u"),
                Err(BandcampError::Auth(_))
            ));
        }
    }

    #[test]
    fn cursor_must_advance() {
        assert!(BandcampClient::check_cursor_progress(None, 49, 0).is_ok());
        assert!(BandcampClient::check_cursor_progress(Some(49), 99, 1).is_ok());
        assert!(BandcampClient::check_cursor_progress(Some(49), 49, 1).is_err());
    }

    #[test]
    fn iteration_cap_enforced() {
        assert!(
            BandcampClient::check_cursor_progress(Some(1), 2, MAX_PAGINATION_ITERATIONS).is_err()
        );
    }
}
