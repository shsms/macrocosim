//! Embedded SPA assets + `/api/logs` log-tap backfill.

use super::super::state::Assets;
use axum::{
    Json,
    body::Body,
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

pub(in crate::ui) async fn index(headers: HeaderMap) -> Response {
    serve_embedded("index.html", &headers)
}

pub(in crate::ui) async fn asset(Path(path): Path<String>, headers: HeaderMap) -> Response {
    serve_embedded(&path, &headers)
}

/// Serve one embedded asset: `Cache-Control: no-cache` plus an
/// `ETag` over the file's content hash, and a 304 when the request
/// echoes that ETag back.
///
/// Both halves are needed. Without `no-cache` a tab left open across
/// an upgrade keeps running pre-upgrade JS against the new server
/// and 404s on endpoints that moved. Without the validator
/// `no-cache` would mean a full 200 re-download on every single
/// request, since the browser has nothing to revalidate WITH — so an
/// unchanged asset costs one small round trip instead, and a changed
/// one is fetched fresh.
fn serve_embedded(path: &str, headers: &HeaderMap) -> Response {
    let no_cache = (header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let Some(content) = Assets::get(path) else {
        return (
            StatusCode::NOT_FOUND,
            [no_cache],
            format!("asset not found: {path}"),
        )
            .into_response();
    };
    // The content hash rust-embed already keeps, so the tag changes
    // exactly when the bytes do — no build stamp, no mtime (which a
    // fresh checkout would move without changing anything).
    let etag = etag_for(&content.metadata.sha256_hash());
    let hit = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| if_none_match_hits(v, &etag));
    // Hex in quotes: always valid header text.
    let etag = HeaderValue::from_str(&etag).expect("a quoted hex etag is a valid header value");
    if hit {
        // 304 carries no body, and keeps the validator + the
        // directive so the next request revalidates the same way.
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag), no_cache]).into_response();
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(mime_for(path)),
            ),
            (header::ETAG, etag),
            no_cache,
        ],
        Body::from(content.data.into_owned()),
    )
        .into_response()
}

/// A strong ETag for a content hash: the hash as lowercase hex,
/// wrapped in the quotes the header syntax requires.
fn etag_for(hash: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut tag = String::with_capacity(hash.len() * 2 + 2);
    tag.push('"');
    for byte in hash {
        write!(tag, "{byte:02x}").unwrap();
    }
    tag.push('"');
    tag
}

/// Does an `If-None-Match` header value match `etag`? The header is
/// a comma-separated list, `*` matches anything the server holds,
/// and a proxy may have weakened a tag to `W/"…"` on the way
/// through — a weak comparison is what this header calls for, so the
/// prefix is stripped rather than rejected.
fn if_none_match_hits(header_value: &str, etag: &str) -> bool {
    header_value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Backfill recent log lines from the LogTap ring buffer. Returns
/// an empty list when the binary didn't initialise a tap (test path).
pub(in crate::ui) async fn logs_backfill() -> Json<Vec<crate::ui_log::LogEvent>> {
    Json(
        crate::ui_log::LOG_TAP
            .get()
            .map(|t| t.snapshot())
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `no-cache` only means "revalidate" if there is something to
    /// revalidate WITH. Every asset response carries an ETag over its
    /// content hash, and a request echoing that ETag back gets a 304
    /// with no body — so an unchanged asset costs a round trip, not a
    /// re-download, while a changed one is fetched fresh.
    #[test]
    fn an_etag_round_trip_yields_a_304() {
        let first = serve_embedded("index.html", &HeaderMap::new());
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let etag = first
            .headers()
            .get(header::ETAG)
            .expect("every asset carries a validator")
            .clone();

        let mut echoed = HeaderMap::new();
        echoed.insert(header::IF_NONE_MATCH, etag.clone());
        let second = serve_embedded("index.html", &echoed);
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(second.headers().get(header::ETAG), Some(&etag));
        assert_eq!(
            second.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache",
            "the 304 keeps telling the browser to revalidate next time"
        );

        // A validator for some other version re-downloads.
        let mut stale = HeaderMap::new();
        stale.insert(header::IF_NONE_MATCH, HeaderValue::from_static("\"old\""));
        assert_eq!(
            serve_embedded("index.html", &stale).status(),
            StatusCode::OK
        );

        // A miss is a 404 with no validator, never a 304.
        let missing = serve_embedded("no-such-asset.js", &HeaderMap::new());
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(missing.headers().get(header::ETAG).is_none());
    }

    /// `If-None-Match` is a list, and a proxy may have weakened the
    /// validator on the way through.
    #[test]
    fn if_none_match_accepts_lists_weak_tags_and_star() {
        assert!(if_none_match_hits("\"a\", \"b\"", "\"b\""));
        assert!(if_none_match_hits("W/\"b\"", "\"b\""));
        assert!(if_none_match_hits("*", "\"anything\""));
        assert!(!if_none_match_hits("\"a\", \"c\"", "\"b\""));
        assert!(!if_none_match_hits("", "\"b\""));
    }
}
