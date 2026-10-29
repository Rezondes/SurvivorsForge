//! In-memory cache of public answers: anonymous `GET`s of public routes are answered from memory
//! until their time runs out. Any successful write through the API empties the cache, so a visitor
//! sees their own change at once; changes by the worker show after at most one TTL.

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use crate::error::ApiError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// GitHub data does not change with our database and is the most expensive to fetch.
pub const GITHUB_TTL: Duration = Duration::from_secs(600);
const MAX_ENTRIES: usize = 5_000;
const MAX_BODY: usize = 8 * 1024 * 1024;

struct Entry {
    expires: Instant,
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

// ponytail: per-process map, one API instance only; a shared cache when the API scales out.
pub struct ResponseCache {
    /// TTL of answers built from our database; zero turns the cache off.
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
}

impl ResponseCache {
    pub fn new(ttl: Duration) -> ResponseCache {
        ResponseCache { ttl, entries: Mutex::new(HashMap::new()) }
    }

    /// How long the answer for `path` may be kept, `None` for routes that are never cached.
    pub fn ttl_for(&self, path: &str) -> Option<Duration> {
        if self.ttl.is_zero() {
            return None;
        }
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match parts.as_slice() {
            ["v1", "github", ..] => Some(GITHUB_TTL),
            ["v1", "registry"] | ["v1", "addons"] | ["v1", "addons", _] | ["v1", "addons", _, "versions"] => Some(self.ttl),
            ["v1", "addons", _, "versions", _, "findings"] | ["v1", "game-versions"] | ["v1", "categories"] => Some(self.ttl * 5),
            _ => None,
        }
    }

    fn get(&self, key: &str, now: Instant) -> Option<(StatusCode, HeaderMap, Bytes)> {
        let entries = self.entries.lock().unwrap();
        entries.get(key).filter(|e| e.expires > now).map(|e| (e.status, e.headers.clone(), e.body.clone()))
    }

    fn put(&self, key: String, entry: Entry, now: Instant) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, e| e.expires > now);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(key, entry);
    }

    /// Drops everything built from our database; GitHub answers stay.
    pub fn invalidate(&self) {
        self.entries.lock().unwrap().retain(|key, _| key.starts_with("/v1/github/"));
    }
}

/// Whether `If-None-Match` names `etag`, compared weakly: a proxy that compresses the answer
/// (Cloudflare) turns `"x"` into `W/"x"`, and clients send that back.
pub fn etag_matches(if_none_match: &HeaderValue, etag: &[u8]) -> bool {
    let strip = |t: &[u8]| t.strip_prefix(b"W/").unwrap_or(t).to_vec();
    let etag = strip(etag);
    if_none_match.as_bytes().split(|&b| b == b',').map(|t| t.trim_ascii()).any(|t| t == b"*" || strip(t) == etag)
}

pub async fn middleware(State(cache): State<Arc<ResponseCache>>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    if method != Method::GET {
        let response = next.run(request).await;
        if response.status().is_success() && method != Method::HEAD && method != Method::OPTIONS {
            cache.invalidate();
        }
        return response;
    }
    let ttl = cache.ttl_for(request.uri().path());
    let Some(ttl) = ttl.filter(|_| !request.headers().contains_key(header::AUTHORIZATION)) else {
        return next.run(request).await;
    };
    let key = request.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let if_none_match = request.headers().get(header::IF_NONE_MATCH).cloned();
    let now = Instant::now();
    if let Some((status, headers, body)) = cache.get(&key, now) {
        if let (Some(sent), Some(etag)) = (&if_none_match, headers.get(header::ETAG))
            && etag_matches(sent, etag.as_bytes())
        {
            let mut not_modified = StatusCode::NOT_MODIFIED.into_response();
            for name in [header::ETAG, header::CACHE_CONTROL] {
                if let Some(value) = headers.get(&name) {
                    not_modified.headers_mut().insert(name, value.clone());
                }
            }
            return not_modified;
        }
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return response;
    }
    let response = next.run(request).await;
    if response.status() != StatusCode::OK {
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY).await else { return ApiError::internal().into_response() };
    cache.put(key, Entry { expires: now + ttl, status: parts.status, headers: parts.headers.clone(), body: body.clone() }, now);
    Response::from_parts(parts, Body::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_routes_are_cached() {
        let cache = ResponseCache::new(Duration::from_secs(60));
        for path in ["/v1/registry", "/v1/addons", "/v1/addons/lucky", "/v1/addons/lucky/versions", "/v1/categories"] {
            assert!(cache.ttl_for(path).is_some(), "{path}");
        }
        assert_eq!(cache.ttl_for("/v1/github/o/r/refs"), Some(GITHUB_TTL));
        for path in ["/v1/me", "/v1/me/addons", "/v1/addons/lucky/permissions", "/v1/addons/lucky/members", "/v1/admin/audit", "/healthz"] {
            assert_eq!(cache.ttl_for(path), None, "{path}");
        }
        assert_eq!(ResponseCache::new(Duration::ZERO).ttl_for("/v1/registry"), None);
    }

    #[test]
    fn etags_compare_weakly() {
        let matches = |sent: &str| etag_matches(&HeaderValue::from_str(sent).unwrap(), br#""abc""#);
        assert!(matches(r#""abc""#) && matches(r#"W/"abc""#) && matches(r#""x", W/"abc""#) && matches("*"));
        assert!(!matches(r#""abd""#) && !matches(r#"W/"ab""#));
    }

    #[test]
    fn writes_keep_github_answers() {
        let cache = ResponseCache::new(Duration::from_secs(60));
        let now = Instant::now();
        let entry = || Entry { expires: now + Duration::from_secs(60), status: StatusCode::OK, headers: HeaderMap::new(), body: Bytes::new() };
        cache.put("/v1/addons".into(), entry(), now);
        cache.put("/v1/github/o/r/refs".into(), entry(), now);
        cache.invalidate();
        assert!(cache.get("/v1/addons", now).is_none());
        assert!(cache.get("/v1/github/o/r/refs", now).is_some());
        assert!(cache.get("/v1/github/o/r/refs", now + Duration::from_secs(61)).is_none(), "expired");
    }
}
