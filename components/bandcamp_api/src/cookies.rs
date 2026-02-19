//! Cookie handling for Bandcamp authentication
//!
//! Supports two formats:
//! - JSON format (Cookie Quick Manager extension)
//! - Netscape/txt format (Get cookies.txt extension)

use crate::error::BandcampError;
use reqwest::cookie::Jar;
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

/// Required cookie names for Bandcamp authentication
const REQUIRED_COOKIES: &[&str] = &["identity"];

/// Cookie data from JSON format (Cookie Quick Manager)
#[derive(Debug, Deserialize)]
struct JsonCookie {
    name: String,
    value: String,
    domain: String,
    path: Option<String>,
    #[serde(default)]
    secure: bool,
    #[serde(rename = "httpOnly", default)]
    http_only: bool,
}

/// Load cookies from a file (auto-detects format)
pub fn load_cookies(path: &Path) -> Result<Arc<Jar>, BandcampError> {
    let content = std::fs::read_to_string(path).map_err(|_| BandcampError::CookieFileNotFound {
        path: path.display().to_string(),
    })?;

    let cookies = parse_cookies(&content)?;
    let jar = build_cookie_jar(&cookies)?;

    Ok(Arc::new(jar))
}

/// Parse cookies from string content (auto-detects format)
fn parse_cookies(content: &str) -> Result<Vec<(String, String, String)>, BandcampError> {
    let content = content.trim();

    // Try JSON format first
    if content.starts_with('[') || content.starts_with('{') {
        parse_json_cookies(content)
    } else {
        parse_netscape_cookies(content)
    }
}

/// Parse JSON format cookies (Cookie Quick Manager)
fn parse_json_cookies(content: &str) -> Result<Vec<(String, String, String)>, BandcampError> {
    // Could be array or object with cookies array
    let cookies: Vec<JsonCookie> = if content.starts_with('[') {
        serde_json::from_str(content)?
    } else {
        // Try to find cookies in nested structure
        let obj: serde_json::Value = serde_json::from_str(content)?;
        if let Some(arr) = obj.get("cookies").and_then(|v| v.as_array()) {
            serde_json::from_value(serde_json::Value::Array(arr.clone()))?
        } else {
            return Err(BandcampError::InvalidCookieFormat(
                "Expected array or object with 'cookies' field".to_string(),
            ));
        }
    };

    let result: Vec<_> = cookies
        .into_iter()
        .filter(|c| c.domain.contains("bandcamp.com"))
        .map(|c| (c.name, c.value, c.domain))
        .collect();

    Ok(result)
}

/// Parse Netscape/txt format cookies (Get cookies.txt)
fn parse_netscape_cookies(content: &str) -> Result<Vec<(String, String, String)>, BandcampError> {
    let mut cookies = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Netscape format: domain, flag, path, secure, expiry, name, value
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 7 {
            let domain = parts[0].trim_start_matches('.');
            let name = parts[5];
            let value = parts[6];

            if domain.contains("bandcamp.com") {
                // Check cookie expiration
                let expiry: i64 = parts[4].parse().unwrap_or(0);
                if expiry > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                    if expiry < now {
                        if name == "identity" {
                            return Err(BandcampError::CookiesExpired {
                                cookie: name.to_string(),
                                expired_ago_secs: (now - expiry) as u64,
                            });
                        }
                        tracing::warn!("Cookie '{}' expired, skipping", name);
                        continue;
                    }
                }

                cookies.push((name.to_string(), value.to_string(), domain.to_string()));
            }
        }
    }

    if cookies.is_empty() {
        return Err(BandcampError::InvalidCookieFormat(
            "No Bandcamp cookies found in Netscape format".to_string(),
        ));
    }

    Ok(cookies)
}

/// Build a reqwest cookie jar from parsed cookies
fn build_cookie_jar(cookies: &[(String, String, String)]) -> Result<Jar, BandcampError> {
    // Verify required cookies are present
    validate_cookies(cookies)?;

    let jar = Jar::default();
    let base_url: url::Url = "https://bandcamp.com".parse().unwrap();

    for (name, value, domain) in cookies {
        // Build cookie string
        let cookie_str = format!(
            "{}={}; Domain={}; Path=/",
            name,
            value,
            if domain.starts_with('.') {
                domain.as_str()
            } else {
                domain
            }
        );

        jar.add_cookie_str(&cookie_str, &base_url);
    }

    Ok(jar)
}

/// Check that required cookies are present
fn validate_cookies(cookies: &[(String, String, String)]) -> Result<(), BandcampError> {
    let cookie_names: Vec<&str> = cookies.iter().map(|(name, _, _)| name.as_str()).collect();

    for required in REQUIRED_COOKIES {
        if !cookie_names.contains(required) {
            tracing::warn!("Missing cookie: {}", required);
            // Don't fail here - cookies might still work
        }
    }

    if cookies.is_empty() {
        return Err(BandcampError::InvalidCookieFormat(
            "No cookies found".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_array_format() {
        let json = r#"[
            {"name": "identity", "value": "abc123", "domain": ".bandcamp.com"},
            {"name": "js_id", "value": "xyz789", "domain": ".bandcamp.com"}
        ]"#;

        let cookies = parse_cookies(json).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].0, "identity");
    }

    #[test]
    fn parse_netscape_format() {
        // Use a far-future expiry (year 2040) so cookies aren't considered expired
        let txt = r#"# Netscape HTTP Cookie File
.bandcamp.com	TRUE	/	TRUE	2208988800	identity	abc123
.bandcamp.com	TRUE	/	TRUE	2208988800	client_id	xyz789
"#;

        let cookies = parse_cookies(txt).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].0, "identity");
    }

    #[test]
    fn expired_identity_cookie_returns_error() {
        // Expiry in the past (year 2009)
        let txt = r#"# Netscape HTTP Cookie File
.bandcamp.com	TRUE	/	TRUE	1234567890	identity	abc123
"#;

        let result = parse_cookies(txt);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expired"),
            "Expected expiry error, got: {}",
            err
        );
    }

    #[test]
    fn expired_non_identity_cookie_is_skipped() {
        let txt = r#"# Netscape HTTP Cookie File
.bandcamp.com	TRUE	/	TRUE	2208988800	identity	abc123
.bandcamp.com	TRUE	/	TRUE	1234567890	other_cookie	xyz789
"#;

        let cookies = parse_cookies(txt).unwrap();
        // Only identity should remain, other_cookie was expired and skipped
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].0, "identity");
    }
}
