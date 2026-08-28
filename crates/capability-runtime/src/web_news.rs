//! Local, fail-open web headline lookup builtin.
//!
//! Configuration is environment-only: `KRW_WEB_NEWS_API_KEY` and
//! `KRW_WEB_NEWS_API_BASE` (unset disables the lookup). Any failure —
//! missing configuration, timeout, HTTP error, or malformed body — yields the
//! normal empty result `{"items": []}`; the builtin never surfaces an error
//! result and never names the retrieval engine. Logs and diagnostics carry
//! the capability id only.

use std::time::Duration;

use serde_json::{Value, json};

const NEWS_TIMEOUT: Duration = Duration::from_secs(5);
const NEWS_PATH: &str = "/api/v3/stock_news";
/// Neutral credential header. The base endpoint is deployment configuration,
/// so an intermediary can adapt any underlying convention without the agent
/// learning a vendor name.
const NEWS_KEY_HEADER: &str = "x-api-key";
const MAX_NEWS_ITEMS: usize = 10;
const MAX_SUMMARY_BYTES: usize = 1_000;
const MAX_HEADLINE_BYTES: usize = 300;
const MAX_PUBLISHER_BYTES: usize = 120;
const MAX_PUBLISHED_AT_BYTES: usize = 40;
const MAX_URL_BYTES: usize = 500;
/// Hard cap on the response body actually parsed. The contract admits at
/// most ten bounded items; a larger body is treated as a failed lookup.
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// Fail-open lookup entry point used by the capability runtime.
pub(crate) async fn fetch_web_news(arguments: &Value) -> Value {
    let config = effective_config();
    fetch_with_config(arguments, config.as_ref()).await
}

/// Resolve the effective configuration. Tests pin an explicit value so the
/// dispatch path stays deterministic against the ambient developer
/// environment; production always reads the environment.
fn effective_config() -> Option<WebNewsConfig> {
    #[cfg(test)]
    if let Some(pinned) = TEST_CONFIG_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return pinned;
    }
    web_news_config()
}

#[cfg(test)]
thread_local! {
    // The outer Option pins whether a test override is active; the inner
    // one reproduces an unset environment without process-wide mutation.
    #[allow(clippy::option_option)]
    static TEST_CONFIG_OVERRIDE: std::cell::RefCell<Option<Option<WebNewsConfig>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn pin_test_config(config: Option<WebNewsConfig>) {
    TEST_CONFIG_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(config));
}

#[cfg(test)]
pub(crate) fn clear_test_config() {
    TEST_CONFIG_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
}

/// Environment-only configuration. Both variables must be present and
/// non-empty; anything else disables the lookup.
fn web_news_config() -> Option<WebNewsConfig> {
    let key = std::env::var("KRW_WEB_NEWS_API_KEY").ok()?;
    let base = std::env::var("KRW_WEB_NEWS_API_BASE").ok()?;
    if key.trim().is_empty() || base.trim().is_empty() {
        return None;
    }
    Some(WebNewsConfig { key, base })
}

#[derive(Clone)]
pub(crate) struct WebNewsConfig {
    pub(crate) key: String,
    pub(crate) base: String,
}

/// Fetch and normalize. Every failure path returns `{"items": []}`.
pub(crate) async fn fetch_with_config(
    arguments: &Value,
    config: Option<&WebNewsConfig>,
) -> Value {
    let Some(config) = config else {
        return empty_items();
    };
    let Some((ticker, limit)) = request_parameters(arguments) else {
        return empty_items();
    };
    let url = format!(
        "{}{}?tickers={}&limit={}",
        config.base.trim_end_matches('/'),
        NEWS_PATH,
        urlencode(&ticker),
        limit
    );
    // Same one-shot default-provider installation the pooled MCP transport
    // uses: reqwest builds its TLS stack even for plain-HTTP targets.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let Ok(client) = reqwest::Client::builder()
        .timeout(NEWS_TIMEOUT)
        .connect_timeout(NEWS_TIMEOUT)
        // Endpoint routing is a deployment decision; never let a process
        // proxy variable redirect the request (and its credential).
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return empty_items();
    };
    let response = client
        .get(&url)
        .header(NEWS_KEY_HEADER, &config.key)
        .send()
        .await;
    let Ok(response) = response else {
        return empty_items();
    };
    if !response.status().is_success() {
        return empty_items();
    }
    let Ok(body) = response.bytes().await else {
        return empty_items();
    };
    if body.len() > MAX_RESPONSE_BYTES {
        return empty_items();
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => return empty_items(),
    };
    normalize_items(&payload)
}

fn empty_items() -> Value {
    json!({"items": []})
}

fn request_parameters(arguments: &Value) -> Option<(String, u64)> {
    let ticker = arguments.get("ticker")?.as_str()?.to_owned();
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 10);
    if ticker.is_empty() {
        return None;
    }
    Some((ticker, limit))
}

/// Map the raw response array onto the bounded five-field item shape. The
/// mapping is total but lossy: over-long fields are truncated to their
/// contract bounds and items without a headline are dropped.
fn normalize_items(payload: &Value) -> Value {
    let Some(rows) = payload.as_array() else {
        return empty_items();
    };
    let mut items = Vec::new();
    for row in rows.iter().take(MAX_NEWS_ITEMS) {
        let Some(object) = row.as_object() else {
            continue;
        };
        let Some(headline) = bounded_field(object, "title", MAX_HEADLINE_BYTES) else {
            continue;
        };
        let publisher = object
            .get("publisher")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| object.get("site").and_then(Value::as_str))
            .map(|value| truncate_bytes(value, MAX_PUBLISHER_BYTES));
        let Some(publisher) = publisher else {
            continue;
        };
        let Some(published_at) = bounded_field(object, "publishedDate", MAX_PUBLISHED_AT_BYTES)
        else {
            continue;
        };
        let url = object
            .get("link")
            .and_then(Value::as_str)
            .map(|value| truncate_bytes(value, MAX_URL_BYTES));
        let summary = object
            .get("text")
            .and_then(Value::as_str)
            .map(|value| truncate_bytes(value, MAX_SUMMARY_BYTES));
        let mut item = json!({
            "headline": headline,
            "publisher": publisher,
            "published_at": published_at,
        });
        if let Some(url) = url
            && !url.is_empty()
        {
            item["url"] = Value::String(url);
        }
        if let Some(summary) = summary
            && !summary.is_empty()
        {
            item["summary"] = Value::String(summary);
        }
        items.push(item);
    }
    json!({"items": items})
}

fn bounded_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    maximum_bytes: usize,
) -> Option<String> {
    let value = object.get(field)?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(truncate_bytes(value, maximum_bytes))
}

/// Truncate to a byte bound on a UTF-8 character boundary. The typed
/// contract validators measure bytes, so the normalized payload always
/// satisfies them without ever splitting a multi-byte character.
fn truncate_bytes(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Minimal percent-encoding for the ticker query parameter.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(HEX[usize::from(byte) >> 4] as char);
                encoded.push(HEX[usize::from(byte) & 0x0F] as char);
            }
        }
    }
    encoded
}
