//! Shared HTML helpers for Bandcamp pages

use crate::error::BandcampError;
use scraper::{Html, Selector};

/// Extract the decoded `data-blob` attribute of the element with the given id.
///
/// Bandcamp embeds page data as HTML-escaped JSON in `data-blob` attributes
/// (e.g. `#pagedata`, `#PlaylistPage`). The HTML parser decodes the entities,
/// so the returned string is plain JSON.
pub(crate) fn extract_data_blob(html: &str, element_id: &str) -> Result<String, BandcampError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse(&format!("#{}", element_id))
        .map_err(|e| BandcampError::HtmlParse(format!("Failed to create selector: {:?}", e)))?;

    let element = document.select(&selector).next().ok_or_else(|| {
        BandcampError::HtmlParse(format!("Could not find #{} element", element_id))
    })?;

    element
        .value()
        .attr("data-blob")
        .map(str::to_string)
        .ok_or_else(|| BandcampError::HtmlParse("Missing data-blob attribute".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_unescapes_blob() {
        let html = r#"<div id="x" data-blob="{&quot;a&quot;: &quot;b &amp; c&quot;}"></div>"#;
        assert_eq!(extract_data_blob(html, "x").unwrap(), r#"{"a": "b & c"}"#);
    }

    #[test]
    fn missing_element_errors() {
        let err = extract_data_blob("<html></html>", "nope").unwrap_err();
        assert!(err.to_string().contains("#nope"));
    }

    #[test]
    fn missing_attribute_errors() {
        let err = extract_data_blob(r#"<div id="x"></div>"#, "x").unwrap_err();
        assert!(err.to_string().contains("data-blob"));
    }
}
