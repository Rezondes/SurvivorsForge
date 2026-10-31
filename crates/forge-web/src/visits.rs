//! Page views and visitors of the website, counted here without cookie or JavaScript. A visitor is
//! a keyed hash of their IP whose key changes every day and never leaves this process; the API
//! gets the sums and those hashes once a minute (`flush`) and keeps per-day numbers only.

use crate::app::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{NaiveDate, Utc};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};

/// The counts of this process since the last flush.
pub static VISITS: LazyLock<Visits> = LazyLock::new(Visits::default);

/// (day, kind, value, target)
type Key = (NaiveDate, &'static str, String, String);

#[derive(Default)]
pub struct Visits {
    pending: Mutex<HashMap<Key, (i64, HashSet<String>)>>,
    key: Mutex<Option<(NaiveDate, [u8; 32])>>,
}

/// Crawlers, link previews and scripts: they are no visitors.
pub fn is_bot(user_agent: &str) -> bool {
    const BOTS: &[&str] = &["bot", "crawl", "spider", "slurp", "curl", "wget", "python", "httpclient", "http-client", "go-http", "java/", "okhttp", "preview", "facebookexternalhit", "headless", "monitor", "uptime", "scan"];
    let ua = user_agent.to_ascii_lowercase();
    ua.is_empty() || BOTS.iter().any(|b| ua.contains(b))
}

/// `steam` for the Steam client's browser (Steam Deck), `mobile` for phones, else `desktop`.
pub fn device(user_agent: &str) -> &'static str {
    let ua = user_agent.to_ascii_lowercase();
    if ua.contains("steam") || ua.contains("valve") {
        "steam"
    } else if ["mobile", "android", "iphone", "ipad"].iter().any(|m| ua.contains(m)) {
        "mobile"
    } else {
        "desktop"
    }
}

/// The domain a visitor came from, `(direct)` without one, `None` for this site itself.
pub fn referrer(referer: Option<&str>, own_host: &str) -> Option<String> {
    let Some(referer) = referer.filter(|r| !r.trim().is_empty()) else { return Some("(direct)".into()) };
    let host = reqwest::Url::parse(referer).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_ascii_lowercase()));
    match host {
        Some(h) if h == own_host.trim_start_matches("www.") => None,
        Some(h) => Some(h),
        None => Some("(unknown)".into()),
    }
}

/// Cloudflare's two-letter country code (`XX` and `T1` for unknown and Tor stay as they are).
pub fn country(headers: &HeaderMap) -> String {
    headers
        .get("cf-ipcountry")
        .and_then(|v| v.to_str().ok())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map_or_else(|| "unknown".into(), str::to_ascii_uppercase)
}

/// The addon slug or pack id of an addon or pack page (`/addons/{slug}/...`, `/packs/{id}`).
pub fn target(route: &str, path: &str) -> String {
    if route.starts_with("/addons/{slug}") || route.starts_with("/packs/{id}") {
        path.split('/').nth(2).unwrap_or_default().chars().take(100).collect()
    } else {
        String::new()
    }
}

/// One view, as the rows it counts in.
pub struct View {
    pub route: String,
    pub target: String,
    pub referrer: Option<String>,
    pub country: String,
    pub device: &'static str,
}

/// Sends the counts of the last minute to the API; on an error they are lost, not retried, so a
/// down API never piles them up.
pub async fn flush(state: &AppState) {
    let rows = VISITS.take();
    if rows.is_empty() {
        return;
    }
    if let Err(e) = state.api.post("/v1/internal/visits", None, &serde_json::json!({ "rows": rows })).await {
        tracing::warn!(error = ?e, "cannot send the page views");
    }
}

impl Visits {
    fn sender(&self, day: NaiveDate, ip: IpAddr) -> String {
        let key = {
            let mut today = self.key.lock().unwrap_or_else(|e| e.into_inner());
            match *today {
                Some((d, key)) if d == day => key,
                _ => {
                    let key = rand::random::<[u8; 32]>();
                    *today = Some((day, key));
                    key
                }
            }
        };
        let digest = Sha256::new().chain_update(key).chain_update(ip.to_string()).finalize();
        digest[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn record(&self, day: NaiveDate, ip: IpAddr, view: View) {
        let sender = self.sender(day, ip);
        let mut rows: Vec<(&'static str, String, String)> = vec![
            ("site", String::new(), String::new()),
            ("page", view.route, view.target),
            ("country", view.country, String::new()),
            ("device", view.device.to_string(), String::new()),
        ];
        if let Some(referrer) = view.referrer {
            rows.push(("referrer", referrer, String::new()));
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (kind, value, target) in rows {
            let entry = pending.entry((day, kind, value, target)).or_default();
            entry.0 += 1;
            entry.1.insert(sender.clone());
        }
    }

    /// The rows of `POST /v1/internal/visits`; empties the counts.
    pub fn take(&self) -> Vec<serde_json::Value> {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        pending
            .into_iter()
            .map(|((day, kind, value, target), (views, senders))| {
                serde_json::json!({ "day": day.to_string(), "kind": kind, "value": value, "target": target, "views": views, "senders": senders })
            })
            .collect()
    }
}

/// Counts a page a person saw: a successful HTML answer to a GET, no htmx part of a page, no bot.
pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let counted = request.method() == Method::GET && !request.headers().contains_key("hx-request");
    let route = request.extensions().get::<MatchedPath>().map(|m| m.as_str().to_string());
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let response = next.run(request).await;
    let html = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html"));
    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !(counted && html && response.status() == StatusCode::OK && !is_bot(user_agent)) {
        return response;
    }
    let (Some(route), Ok(ip)) = (route, crate::rate_limit::CLIENT_IP.try_with(|ip| *ip)) else { return response };
    let own_host = reqwest::Url::parse(&state.config.public_web_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let view = View {
        target: target(&route, &path),
        route,
        referrer: referrer(headers.get(header::REFERER).and_then(|v| v.to_str().ok()), &own_host),
        country: country(&headers),
        device: device(user_agent),
    };
    VISITS.record(Utc::now().date_naive(), ip, view);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_not_bots() {
        assert!(!is_bot("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/141.0 Safari/537.36"));
        assert!(is_bot("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"));
        assert!(is_bot("Discordbot/2.0"));
        assert!(is_bot("curl/8.4.0"));
        assert!(is_bot(""));
    }

    #[test]
    fn devices_referrers_countries_targets() {
        assert_eq!(device("Mozilla/5.0 (X11; Linux x86_64) Valve Steam Client"), "steam");
        assert_eq!(device("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Mobile/15E148"), "mobile");
        assert_eq!(device("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"), "desktop");
        assert_eq!(referrer(Some("https://www.reddit.com/r/azerothsurvivors/"), "forge.rezondes.net"), Some("reddit.com".into()));
        assert_eq!(referrer(Some("https://forge.rezondes.net/addons"), "forge.rezondes.net"), None, "moving around the site");
        assert_eq!(referrer(None, "forge.rezondes.net"), Some("(direct)".into()));
        let mut h = HeaderMap::new();
        assert_eq!(country(&h), "unknown");
        h.insert("cf-ipcountry", "de".parse().unwrap());
        assert_eq!(country(&h), "DE");
        assert_eq!(target("/addons/{slug}/versions/{tag}", "/addons/lucky/versions/v1"), "lucky");
        assert_eq!(target("/packs/{id}", "/packs/abc123defg"), "abc123defg");
        assert_eq!(target("/addons", "/addons"), "");
    }

    #[test]
    fn one_visitor_many_views() {
        let visits = Visits::default();
        let day = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
        let view = |route: &str| View { route: route.into(), target: String::new(), referrer: None, country: "DE".into(), device: "desktop" };
        visits.record(day, "203.0.113.7".parse().unwrap(), view("/"));
        visits.record(day, "203.0.113.7".parse().unwrap(), view("/addons"));
        visits.record(day, "203.0.113.8".parse().unwrap(), view("/"));
        let rows = visits.take();
        let site = rows.iter().find(|r| r["kind"] == "site").unwrap();
        assert_eq!((site["views"].as_i64(), site["senders"].as_array().map(Vec::len)), (Some(3), Some(2)));
        assert!(!serde_json::to_string(&rows).unwrap().contains("203.0.113"), "no IP leaves the website");
        assert!(visits.take().is_empty(), "taken once");
    }
}
