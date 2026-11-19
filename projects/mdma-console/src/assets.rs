//! Embedded web assets and the app shell page.
//!
//! Assets are compiled into the binary and served under `/static/<tag>/<path>`, where `<tag>` is a
//! hash of every asset byte. A new build with changed assets therefore has new URLs, so the
//! `immutable` cache header is safe and relative ES-module imports inherit the tag for free.

use crate::AppState;
use askama::Template;
use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::{Arc, LazyLock};

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

struct Asset {
    path: &'static str,
    content_type: &'static str,
    body: &'static str,
}

/// Every embedded asset. Adding a file under `static/` means adding it here; a test fails otherwise.
const ASSETS: &[Asset] = &[
    Asset {
        path: "app.css",
        content_type: CSS,
        body: include_str!("../static/app.css"),
    },
    Asset {
        path: "js/core.js",
        content_type: JS,
        body: include_str!("../static/js/core.js"),
    },
    Asset {
        path: "js/art.js",
        content_type: JS,
        body: include_str!("../static/js/art.js"),
    },
    Asset {
        path: "js/logic.js",
        content_type: JS,
        body: include_str!("../static/js/logic.js"),
    },
    // Feature modules. Each phase owns whole files; each registers its own routes on import.
    Asset {
        path: "js/player.js",
        content_type: JS,
        body: include_str!("../static/js/player.js"),
    },
    Asset {
        path: "js/queue.js",
        content_type: JS,
        body: include_str!("../static/js/queue.js"),
    },
    Asset {
        path: "js/lists.js",
        content_type: JS,
        body: include_str!("../static/js/lists.js"),
    },
    Asset {
        path: "js/narrow.js",
        content_type: JS,
        body: include_str!("../static/js/narrow.js"),
    },
    Asset {
        path: "js/views.js",
        content_type: JS,
        body: include_str!("../static/js/views.js"),
    },
    Asset {
        path: "js/rail.js",
        content_type: JS,
        body: include_str!("../static/js/rail.js"),
    },
    Asset {
        path: "js/collections.js",
        content_type: JS,
        body: include_str!("../static/js/collections.js"),
    },
    Asset {
        path: "js/playlist.js",
        content_type: JS,
        body: include_str!("../static/js/playlist.js"),
    },
    Asset {
        path: "js/bin.js",
        content_type: JS,
        body: include_str!("../static/js/bin.js"),
    },
    Asset {
        path: "js/workbench.js",
        content_type: JS,
        body: include_str!("../static/js/workbench.js"),
    },
    Asset {
        path: "js/timeline.js",
        content_type: JS,
        body: include_str!("../static/js/timeline.js"),
    },
    Asset {
        path: "js/addmusic.js",
        content_type: JS,
        body: include_str!("../static/js/addmusic.js"),
    },
    Asset {
        path: "js/bandcamp.js",
        content_type: JS,
        body: include_str!("../static/js/bandcamp.js"),
    },
    Asset {
        path: "js/updates.js",
        content_type: JS,
        body: include_str!("../static/js/updates.js"),
    },
    Asset {
        path: "js/about.js",
        content_type: JS,
        body: include_str!("../static/js/about.js"),
    },
    Asset {
        path: "js/settings.js",
        content_type: JS,
        body: include_str!("../static/js/settings.js"),
    },
    Asset {
        path: "js/app.js",
        content_type: JS,
        body: include_str!("../static/js/app.js"),
    },
];

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

const FNV_OFFSET: u32 = 0x811c_9dc5;

/// Fold `bytes` into a 32-bit FNV-1a state.
fn fnv1a(mut hash: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Hash of every asset's path, length and body, so renames and moved bytes change the tag too.
fn tag_of(assets: &[Asset]) -> String {
    let mut hash = FNV_OFFSET;
    for a in assets {
        hash = fnv1a(hash, a.path.as_bytes());
        hash = fnv1a(hash, &(a.body.len() as u64).to_le_bytes());
        hash = fnv1a(hash, a.body.as_bytes());
    }
    format!("{hash:08x}")
}

/// 8-hex content hash of all assets; the URL prefix that busts caches.
pub static ASSET_TAG: LazyLock<String> = LazyLock::new(|| tag_of(ASSETS));

fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

async fn serve_asset(Path((tag, path)): Path<(String, String)>) -> Response {
    if tag != *ASSET_TAG {
        return StatusCode::NOT_FOUND.into_response();
    }
    match find(&path) {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (header::CACHE_CONTROL, IMMUTABLE),
            ],
            asset.body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Template)]
#[template(path = "app.html")]
struct AppTemplate {
    version: String,
    node: String,
    asset_tag: String,
}

/// Best-effort name of this node for display, read once.
static NODE_NAME: LazyLock<String> = LazyLock::new(|| {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "mdma".to_string())
});

fn render_shell(node: String) -> Response {
    let template = AppTemplate {
        version: env!("CARGO_PKG_VERSION").to_string(),
        node,
        asset_tag: ASSET_TAG.clone(),
    };
    match template.render() {
        Ok(html) => ([(header::CACHE_CONTROL, "no-cache")], Html(html)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Template render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        }
    }
}

/// The app shell. Never cached: it carries the tag that points at the current assets.
async fn shell() -> Response {
    render_shell(NODE_NAME.clone())
}

/// Routes for the new UI: the shell at `/` (the old UI lives at `/classic`) and the assets.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(shell))
        .route("/static/:tag/*path", get(serve_asset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{get as get_path, get_with_encoding};
    use std::collections::BTreeSet;

    /// Budgets are for what goes over the wire: gzipped bytes.
    const CSS_BUDGET_GZ: usize = 15 * 1024;
    const JS_BUDGET_GZ: usize = 86 * 1024; // a tripwire against unintended growth of what goes over the wire for every view; when a feature needs the room, raise it on purpose to the measured size rounded up to a whole KiB
    /// The only external-looking URL allowed: the SVG namespace, which is an identifier, not a request.
    const SVG_NS: &str = "http://www.w3.org/2000/svg";

    fn header_of(res: &Response, name: header::HeaderName) -> &str {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    async fn body_string(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn shell_html() -> String {
        AppTemplate {
            version: "0.0.0".into(),
            node: "test-node".into(),
            asset_tag: ASSET_TAG.clone(),
        }
        .render()
        .unwrap()
    }

    /// Values of every `attr="..."` in an HTML string.
    fn attr_values(html: &str, attr: &str) -> Vec<String> {
        let needle = format!(" {attr}=\"");
        html.match_indices(&needle)
            .map(|(i, _)| {
                let rest = &html[i + needle.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    }

    /// Specifiers of the static imports in a JS module (`import ... from "x"`, `import "x"`,
    /// and `} from "x"` closing a multi-line import). The house style is double-quoted, static
    /// and relative; anything this scan cannot see is an error rather than a silent miss:
    /// single-quoted imports, `export ... from`, and dynamic `import(`.
    fn scan_imports(js: &str) -> Result<Vec<String>, String> {
        if js.contains("import(") {
            return Err("dynamic import() is not allowed".into());
        }
        let mut out = Vec::new();
        for line in js.lines() {
            let line = line.trim_start();
            if line.starts_with("export ") && line.contains(" from ") {
                return Err(format!("re-export is not allowed: {line}"));
            }
            if !line.starts_with("import ") && !line.starts_with("} from ") {
                continue;
            }
            if line.starts_with("import ") && line.ends_with('{') {
                continue; // opens a multi-line import; the `} from "x"` line names the path
            }
            let spec = line
                .find('"')
                .and_then(|i| line[i + 1..].find('"').map(|j| &line[i + 1..i + 1 + j]));
            match spec {
                Some(s) => out.push(s.to_string()),
                None => return Err(format!("import without a double-quoted path: {line}")),
            }
        }
        Ok(out)
    }

    #[test]
    fn import_scan_rejects_forms_it_cannot_follow() {
        assert_eq!(
            scan_imports("import { a } from \"./a.js\";\nimport \"./b.js\";").unwrap(),
            ["./a.js", "./b.js"]
        );
        assert_eq!(
            scan_imports("import {\n  a,\n} from \"./a.js\";").unwrap(),
            ["./a.js"]
        );
        assert!(scan_imports("import { a } from './a.js';").is_err());
        assert!(scan_imports("export { a } from \"./a.js\";").is_err());
        assert!(scan_imports("export * from \"./a.js\";").is_err());
        assert!(scan_imports("const m = await import(\"./a.js\");").is_err());
    }

    /// Resolve a relative import against the importing asset's path.
    fn resolve(from: &str, spec: &str) -> String {
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
        for seg in spec.split('/') {
            match seg {
                "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        parts.join("/")
    }

    #[test]
    fn asset_tag_is_eight_hex_digits() {
        let tag = &*ASSET_TAG;
        assert_eq!(tag.len(), 8);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()), "tag: {tag}");
    }

    #[test]
    fn asset_tag_changes_with_content() {
        let a = [Asset {
            path: "x",
            content_type: JS,
            body: "one",
        }];
        let b = [Asset {
            path: "x",
            content_type: JS,
            body: "two",
        }];
        assert_ne!(tag_of(&a), tag_of(&b));
        assert_eq!(tag_of(&a), tag_of(&a));
    }

    #[test]
    fn asset_tag_changes_with_path_and_with_where_bytes_sit() {
        let one = |path, body| Asset {
            path,
            content_type: JS,
            body,
        };
        // Renamed file, same bytes.
        assert_ne!(tag_of(&[one("a.js", "x")]), tag_of(&[one("b.js", "x")]));
        // Same concatenated bytes, split differently between files.
        assert_ne!(
            tag_of(&[one("a.js", "xy"), one("b.js", "")]),
            tag_of(&[one("a.js", "x"), one("b.js", "y")])
        );
    }

    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(fnv1a(FNV_OFFSET, b""), 0x811c_9dc5);
        assert_eq!(fnv1a(FNV_OFFSET, b"a"), 0xe40c_292c);
        assert_eq!(fnv1a(FNV_OFFSET, b"foobar"), 0xbf9c_f968);
        assert_eq!(fnv1a(fnv1a(FNV_OFFSET, b"foo"), b"bar"), 0xbf9c_f968);
    }

    #[test]
    fn every_static_file_is_embedded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
        let mut on_disk = BTreeSet::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(&root).unwrap();
                    on_disk.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let embedded: BTreeSet<String> = ASSETS.iter().map(|a| a.path.to_string()).collect();
        assert_eq!(
            on_disk, embedded,
            "static/ and the ASSETS table must list the same files"
        );
    }

    #[test]
    fn shell_refs_resolve_in_asset_table() {
        let html = shell_html();
        let prefix = format!("/static/{}/", *ASSET_TAG);
        let mut refs = attr_values(&html, "src");
        refs.extend(attr_values(&html, "href"));
        let mut checked = 0;
        for r in refs {
            if r.starts_with('#') {
                continue; // in-page: router links and sprite <use>
            }
            let path = r
                .strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("{r} is not under {prefix}"));
            assert!(find(path).is_some(), "{r} is not in the asset table");
            checked += 1;
        }
        assert!(checked >= 2, "expected the stylesheet and the entry module");
    }

    #[test]
    fn js_imports_resolve_in_asset_table() {
        let mut checked = 0;
        for asset in ASSETS.iter().filter(|a| a.path.ends_with(".js")) {
            let specs = scan_imports(asset.body).unwrap_or_else(|e| panic!("{}: {e}", asset.path));
            for spec in specs {
                assert!(
                    spec.starts_with("./") || spec.starts_with("../"),
                    "{}: import {spec} must be relative",
                    asset.path
                );
                let target = resolve(asset.path, &spec);
                assert!(
                    find(&target).is_some(),
                    "{}: import {spec} resolves to {target}, which is not an asset",
                    asset.path
                );
                checked += 1;
            }
        }
        assert!(checked >= 3, "import scan found only {checked} imports");
    }

    #[test]
    fn no_asset_makes_external_requests() {
        let shell = shell_html();
        let everything = ASSETS
            .iter()
            .map(|a| (a.path, a.body))
            .chain(std::iter::once(("app.html", shell.as_str())));
        for (path, body) in everything {
            let scrubbed = body.replace(SVG_NS, "");
            assert!(
                !scrubbed.contains("http://") && !scrubbed.contains("https://"),
                "{path} references an external URL"
            );
        }
    }

    #[test]
    fn size_budgets_hold() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;

        // (raw, gzipped) bytes of the assets with this extension; each file is gzipped on its
        // own, as the server does.
        let sizes = |ext: &str| {
            ASSETS
                .iter()
                .filter(|a| a.path.ends_with(ext))
                .fold((0, 0), |(raw, gz), a| {
                    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
                    enc.write_all(a.body.as_bytes()).unwrap();
                    (raw + a.body.len(), gz + enc.finish().unwrap().len())
                })
        };
        let (css_raw, css_gz) = sizes(".css");
        let (js_raw, js_gz) = sizes(".js");
        println!("asset sizes: css {css_raw} B raw / {css_gz} B gzipped, js {js_raw} B raw / {js_gz} B gzipped");
        assert!(
            css_gz <= CSS_BUDGET_GZ,
            "css is {css_gz} B gzipped, budget {CSS_BUDGET_GZ} B"
        );
        assert!(
            js_gz <= JS_BUDGET_GZ,
            "js is {js_gz} B gzipped, budget {JS_BUDGET_GZ} B"
        );
    }

    #[test]
    fn content_types_match_extensions() {
        for a in ASSETS {
            let want = if a.path.ends_with(".css") {
                CSS
            } else if a.path.ends_with(".js") {
                JS
            } else {
                panic!("{}: no content type known for this extension", a.path)
            };
            assert_eq!(a.content_type, want, "{}", a.path);
        }
    }

    #[test]
    fn theme_script_precedes_stylesheet_and_defaults_to_dark() {
        let html = shell_html();
        let script = html.find("mdma-theme").expect("theme script");
        let css = html.find("app.css").expect("stylesheet link");
        assert!(script < css, "theme must be applied before first paint");
        assert!(html.contains("=== \"light\"") || html.contains("===\"light\""));
        assert!(!html.contains("prefers-color-scheme"));
    }

    #[tokio::test]
    async fn serves_every_asset_with_immutable_cache_and_content_type() {
        for a in ASSETS {
            let res = get_path(&format!("/static/{}/{}", *ASSET_TAG, a.path)).await;
            assert_eq!(res.status(), StatusCode::OK, "{}", a.path);
            assert_eq!(
                header_of(&res, header::CACHE_CONTROL),
                "public, max-age=31536000, immutable",
                "{}",
                a.path
            );
            assert_eq!(header_of(&res, header::CONTENT_TYPE), a.content_type);
            assert_eq!(body_string(res).await, a.body, "{}", a.path);
        }
    }

    #[tokio::test]
    async fn wrong_tag_is_404() {
        let res = get_path("/static/00000000/app.css").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_ne!(
            header_of(&res, header::CACHE_CONTROL),
            IMMUTABLE,
            "a 404 must never be cached as immutable"
        );
    }

    #[tokio::test]
    async fn unknown_asset_under_current_tag_is_404() {
        let res = get_path(&format!("/static/{}/js/nope.js", *ASSET_TAG)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = get_path(&format!("/static/{}/../Cargo.toml", *ASSET_TAG)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn shell_is_html_and_never_cached() {
        let res = get_path("/").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header_of(&res, header::CACHE_CONTROL), "no-cache");
        assert!(header_of(&res, header::CONTENT_TYPE).starts_with("text/html"));
        let body = body_string(res).await;
        assert!(body.contains(&format!("/static/{}/js/app.js", *ASSET_TAG)));
    }

    #[tokio::test]
    async fn assets_are_gzipped_when_asked() {
        let res = get_with_encoding(&format!("/static/{}/app.css", *ASSET_TAG), Some("gzip")).await;
        assert_eq!(header_of(&res, header::CONTENT_ENCODING), "gzip");
    }
}
