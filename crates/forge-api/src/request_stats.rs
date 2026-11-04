//! Counts of API requests per hour, route, method, status and client. They gather in memory, go to
//! `api_request_stats` once a minute, and also feed the Prometheus metrics on `METRICS_BIND_ADDR`.
//! Launchers that send their installation ID are also noted per hour with their version and
//! platform (`launcher_active`), for the active launchers in Grafana. Other clients of the public API
//! are counted per hour by their User-Agent's first word and country (`public_api_stats`).

use axum::body::HttpBody;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, Method};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::Instrument;

/// How long the hourly buckets stay in the database.
pub const KEEP_DAYS: i64 = 90;
/// The periodic job that deletes older buckets.
pub const PRUNE: &str = "prune_request_stats";

/// Who sent a request: the launcher, the website or anyone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Client {
    Launcher,
    Website,
    Other,
}

impl Client {
    pub fn as_str(self) -> &'static str {
        match self {
            Client::Launcher => "launcher",
            Client::Website => "website",
            Client::Other => "other",
        }
    }
}

/// The version and platform a launcher names in its User-Agent, `SurvivorsForge/<version> (<os>)`.
/// Launchers before 0.6.4 name no platform, and anything but the three known ones reads `unknown`;
/// an odd version is not stored at all, so no client fills the table with labels of its own.
pub fn launcher_build(user_agent: &str) -> Option<(String, &'static str)> {
    let rest = user_agent.strip_prefix("SurvivorsForge/")?;
    let (version, platform) = rest.split_once(' ').map_or((rest, ""), |(v, p)| (v, p.trim()));
    let version_ok = !version.is_empty() && version.len() <= 32 && version.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b));
    let os = match platform.trim_start_matches('(').trim_end_matches(')') {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        _ => "unknown",
    };
    version_ok.then(|| (version.to_string(), os))
}

/// The routes of the public API for other launchers (docs/public-api.md).
pub const PUBLIC_ROUTES: [&str; 4] = ["/v1/registry", "/v1/addons/{slug}", "/v1/addons/{slug}/versions", "/v1/addons/{slug}/versions/{tag}/findings"];

/// Who calls the public API: the first word of the User-Agent (`MyLauncher/1.2`), as its rules ask;
/// `none` without one and `unknown` for an odd one, so no client fills the table with labels.
pub fn public_client(user_agent: Option<&str>) -> String {
    let Some(first) = user_agent.and_then(|ua| ua.split_whitespace().next()) else { return "none".into() };
    let fine = first.len() <= 48 && first.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+/".contains(&b));
    if fine { first.to_string() } else { "unknown".into() }
}

/// The country Cloudflare reports (`CF-IPCountry`, two letters), else `unknown`; never the address.
pub fn country(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("cf-ipcountry")
        .and_then(|v| v.to_str().ok())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map_or_else(|| "unknown".into(), str::to_ascii_uppercase)
}

/// The launcher sends `SurvivorsForge/<version>`; the website calls from `WEBSITE_IP`.
pub fn classify(user_agent: Option<&str>, peer: IpAddr, website: Option<IpAddr>) -> Client {
    if user_agent.is_some_and(|ua| ua.starts_with("SurvivorsForge/")) {
        Client::Launcher
    } else if Some(peer) == website {
        Client::Website
    } else {
        Client::Other
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    hour: DateTime<Utc>,
    route: String,
    method: String,
    status: u16,
    client: Client,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Bucket {
    count: i64,
    total_ms: i64,
    max_ms: i64,
}

/// A launcher seen in an hour.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Active {
    hour: DateTime<Utc>,
    install_id: String,
    version: String,
    os: &'static str,
}

/// An hour's public API requests of one client from one country on one route.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PublicKey {
    hour: DateTime<Utc>,
    route: String,
    client: String,
    country: String,
}

/// The counts, the active launchers and the public API use since the last flush.
#[derive(Default)]
pub struct RequestStats {
    pending: Mutex<HashMap<Key, Bucket>>,
    active: Mutex<HashSet<Active>>,
    public: Mutex<HashMap<PublicKey, i64>>,
}

impl RequestStats {
    pub fn record(&self, at: DateTime<Utc>, route: &str, method: &str, status: u16, client: Client, millis: i64) {
        let hour = at.duration_trunc(TimeDelta::hours(1)).unwrap_or(at);
        let key = Key { hour, route: route.to_string(), method: method.to_string(), status, client };
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = pending.entry(key).or_default();
        bucket.count += 1;
        bucket.total_ms += millis;
        bucket.max_ms = bucket.max_ms.max(millis);
    }

    /// Notes a launcher that asked something in the hour of `at`.
    pub fn record_active(&self, at: DateTime<Utc>, install_id: String, version: String, os: &'static str) {
        let hour = at.duration_trunc(TimeDelta::hours(1)).unwrap_or(at);
        self.active.lock().unwrap_or_else(|e| e.into_inner()).insert(Active { hour, install_id, version, os });
    }

    /// Counts a request of another client to a public route in the hour of `at`.
    pub fn record_public(&self, at: DateTime<Utc>, route: &str, client: String, country: String) {
        let hour = at.duration_trunc(TimeDelta::hours(1)).unwrap_or(at);
        *self.public.lock().unwrap_or_else(|e| e.into_inner()).entry(PublicKey { hour, route: route.to_string(), client, country }).or_default() += 1;
    }

    /// Writes the gathered counts and starts over; on an error they are kept for the next try.
    pub async fn flush(&self, pool: &PgPool) -> sqlx::Result<()> {
        let public = std::mem::take(&mut *self.public.lock().unwrap_or_else(|e| e.into_inner()));
        if !public.is_empty()
            && let Err(e) = write_public(pool, &public).await
        {
            let mut kept = self.public.lock().unwrap_or_else(|e| e.into_inner());
            for (key, count) in public {
                *kept.entry(key).or_default() += count;
            }
            return Err(e);
        }
        let active = std::mem::take(&mut *self.active.lock().unwrap_or_else(|e| e.into_inner()));
        if !active.is_empty()
            && let Err(e) = write_active(pool, &active).await
        {
            self.active.lock().unwrap_or_else(|e| e.into_inner()).extend(active);
            return Err(e);
        }
        let taken = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        if taken.is_empty() {
            return Ok(());
        }
        let result = write(pool, &taken).await;
        if result.is_err() {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            for (key, b) in taken {
                let bucket = pending.entry(key).or_default();
                bucket.count += b.count;
                bucket.total_ms += b.total_ms;
                bucket.max_ms = bucket.max_ms.max(b.max_ms);
            }
        }
        result
    }
}

async fn write_public(pool: &PgPool, counts: &HashMap<PublicKey, i64>) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for (k, count) in counts {
        sqlx::query(
            "INSERT INTO public_api_stats (hour, route, client, country, count) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (hour, route, client, country) DO UPDATE SET count = public_api_stats.count + EXCLUDED.count",
        )
        .bind(k.hour)
        .bind(&k.route)
        .bind(&k.client)
        .bind(&k.country)
        .bind(count)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// One row per launcher and hour; a later request in the hour names the newer version.
async fn write_active(pool: &PgPool, active: &HashSet<Active>) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for a in active {
        sqlx::query(
            "INSERT INTO launcher_active (hour, install_id, version, os) VALUES ($1, $2, $3, $4)
             ON CONFLICT (hour, install_id) DO UPDATE SET version = EXCLUDED.version, os = EXCLUDED.os",
        )
        .bind(a.hour)
        .bind(&a.install_id)
        .bind(&a.version)
        .bind(a.os)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

async fn write(pool: &PgPool, buckets: &HashMap<Key, Bucket>) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for (k, b) in buckets {
        sqlx::query(
            "INSERT INTO api_request_stats (hour, route, method, status, client, count, total_ms, max_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (hour, route, method, status, client) DO UPDATE
                 SET count = api_request_stats.count + EXCLUDED.count,
                     total_ms = api_request_stats.total_ms + EXCLUDED.total_ms,
                     max_ms = greatest(api_request_stats.max_ms, EXCLUDED.max_ms)",
        )
        .bind(k.hour)
        .bind(&k.route)
        .bind(&k.method)
        .bind(i16::try_from(k.status).unwrap_or(i16::MAX))
        .bind(k.client.as_str())
        .bind(b.count)
        .bind(b.total_ms)
        .bind(b.max_ms)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Deletes buckets of API and GitHub requests older than `KEEP_DAYS`. The job `prune_request_stats`.
pub async fn prune(pool: &PgPool, now: DateTime<Utc>) -> Result<(), String> {
    for table in ["api_request_stats", "github_request_stats", "public_api_stats"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE hour < $1"))
            .bind(now - TimeDelta::days(KEEP_DAYS))
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The state of the middleware: the counts, the clock and the website's address.
#[derive(Clone)]
pub struct Recorder {
    pub stats: Arc<RequestStats>,
    pub clock: crate::github::Clock,
    pub website: Option<IpAddr>,
}

/// The method as a label and stats key. An extension method is any token the client picks, so
/// it counts as `other`; otherwise every new one would stay in memory as a label for good.
fn method_label(method: &Method) -> &str {
    let standard = matches!(*method, Method::GET | Method::HEAD | Method::POST | Method::PUT | Method::PATCH | Method::DELETE | Method::OPTIONS | Method::TRACE | Method::CONNECT);
    if standard { method.as_str() } else { "other" }
}

/// Counts every request, also those the rate limit or the cache answered. Container healthchecks
/// (`/healthz`) are left out: they are no API use.
pub async fn middleware(State(recorder): State<Recorder>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let route = request.extensions().get::<MatchedPath>().map_or("unmatched", MatchedPath::as_str).to_string();
    if route == "/healthz" {
        return next.run(request).await;
    }
    let method = method_label(request.method()).to_string();
    let user_agent = request.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    let client = classify(user_agent, crate::rate_limit::peer_ip(&request), recorder.website);
    if client == Client::Other && PUBLIC_ROUTES.contains(&route.as_str()) {
        recorder.stats.record_public((recorder.clock)(), &route, public_client(user_agent), country(request.headers()));
    }
    // One span per request (OpenTelemetry, `forge_trace`), continuing the website's trace.
    let span = tracing::info_span!("request", method = %method, route = %route, client = client.as_str(), install_id = tracing::field::Empty, status = tracing::field::Empty, trace_id = tracing::field::Empty);
    if let Some(id) = crate::routes::telemetry::install_id(request.headers()) {
        span.record("install_id", id.as_str());
        if let Some((version, os)) = user_agent.filter(|_| client == Client::Launcher).and_then(launcher_build) {
            recorder.stats.record_active((recorder.clock)(), id, version, os);
        }
    }
    forge_trace::continue_trace(&span, request.headers());
    // GitHub requests this request causes count for its route (`github::usage`).
    let origin = format!("{method} {route}");
    let response = crate::github::usage::ORIGIN.scope(origin, next.run(request)).instrument(span.clone()).await;
    span.record("status", response.status().as_u16());
    let elapsed = started.elapsed();
    let status = response.status().as_u16();
    recorder.stats.record((recorder.clock)(), &route, &method, status, client, i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    let labels = [("route", route), ("method", method), ("status", status.to_string()), ("client", client.as_str().to_string())];
    metrics::counter!("forge_http_requests_total", &labels).increment(1);
    metrics::histogram!("forge_http_request_duration_seconds", &labels[..2]).record(elapsed.as_secs_f64());
    if let Some(bytes) = response_bytes(&response).filter(|_| labels[1].1 != "HEAD") {
        metrics::counter!("forge_http_response_bytes_total", &labels[..1]).increment(bytes);
    }
    response
}

/// The size of the answer's body before any proxy compresses it: `Content-Length` or the size the
/// body knows up front. Streamed bodies (pack and image files) have none and are not counted.
fn response_bytes(response: &Response) -> Option<u64> {
    let header = response.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse().ok());
    header.or_else(|| response.body().size_hint().exact())
}

/// Installs the Prometheus recorder of this process; `None` if one is installed already.
pub fn install_prometheus() -> Option<PrometheusHandle> {
    const SECONDS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];
    PrometheusBuilder::new()
        .set_buckets_for_metric(metrics_exporter_prometheus::Matcher::Suffix("_seconds".into()), SECONDS)
        .ok()?
        .install_recorder()
        .ok()
}

/// The router of `METRICS_BIND_ADDR`: only `GET /metrics`, never reachable through Caddy.
pub fn metrics_router(handle: PrometheusHandle) -> axum::Router {
    axum::Router::new().route("/metrics", axum::routing::get(move || std::future::ready(handle.render())))
}

/// Serves `/metrics` until the process ends; errors are logged, the API keeps running.
pub fn spawn_metrics_server(addr: std::net::SocketAddr) {
    let Some(handle) = install_prometheus() else { return };
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "metrics listening");
                if let Err(e) = axum::serve(listener, metrics_router(handle)).await {
                    tracing::error!(error = %e, "metrics server stopped");
                }
            }
            Err(e) => tracing::error!(error = %e, %addr, "cannot listen for metrics"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_client() {
        let website: IpAddr = "10.0.0.11".parse().unwrap();
        let other: IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(classify(Some("SurvivorsForge/0.4.3"), other, Some(website)), Client::Launcher);
        assert_eq!(classify(Some("reqwest"), website, Some(website)), Client::Website);
        assert_eq!(classify(Some("curl/8.0"), other, Some(website)), Client::Other);
        assert_eq!(classify(None, other, None), Client::Other);
    }

    #[test]
    fn reads_version_and_platform_from_the_launcher() {
        assert_eq!(launcher_build("SurvivorsForge/0.6.4 (windows)"), Some(("0.6.4".into(), "windows")));
        assert_eq!(launcher_build("SurvivorsForge/0.7.0-rc.1 (macos)"), Some(("0.7.0-rc.1".into(), "macos")));
        assert_eq!(launcher_build("SurvivorsForge/0.6.3"), Some(("0.6.3".into(), "unknown")), "before the platform was sent");
        assert_eq!(launcher_build("SurvivorsForge/0.6.4 (amiga)"), Some(("0.6.4".into(), "unknown")));
        assert_eq!(launcher_build("SurvivorsForge/<script>"), None);
        assert_eq!(launcher_build(&format!("SurvivorsForge/{}", "9".repeat(40))), None);
        assert_eq!(launcher_build("curl/8.0"), None);
    }

    #[test]
    fn names_public_clients_by_their_user_agent() {
        assert_eq!(public_client(Some("MyLauncher/1.2 (+https://github.com/me/mylauncher)")), "MyLauncher/1.2");
        assert_eq!(public_client(None), "none");
        assert_eq!(public_client(Some("<script>alert(1)</script>")), "unknown");
        assert_eq!(public_client(Some(&"a".repeat(60))), "unknown");
    }

    #[test]
    fn labels_extension_methods_as_other() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::DELETE), "DELETE");
        assert_eq!(method_label(&Method::from_bytes("X".repeat(4096).as_bytes()).unwrap()), "other");
    }

    #[test]
    fn records_per_hour_and_keeps_the_maximum() {
        let stats = RequestStats::default();
        let at: DateTime<Utc> = "2026-10-31T10:15:00Z".parse().unwrap();
        stats.record(at, "/v1/addons", "GET", 200, Client::Other, 10);
        stats.record(at + TimeDelta::minutes(30), "/v1/addons", "GET", 200, Client::Other, 30);
        stats.record(at + TimeDelta::hours(1), "/v1/addons", "GET", 200, Client::Other, 5);
        let pending = stats.pending.lock().unwrap();
        let hour: DateTime<Utc> = "2026-10-31T10:00:00Z".parse().unwrap();
        let first = pending.iter().find(|(k, _)| k.hour == hour).map(|(_, b)| *b);
        assert_eq!(first, Some(Bucket { count: 2, total_ms: 40, max_ms: 30 }));
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn measures_known_response_sizes() {
        assert_eq!(response_bytes(&Response::new(axum::body::Body::from("hello"))), Some(5));
        assert_eq!(response_bytes(&Response::new(axum::body::Body::empty())), Some(0));
        let mut sized = Response::new(axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(tokio::io::empty())));
        assert_eq!(response_bytes(&sized), None, "a stream has no size up front");
        sized.headers_mut().insert(header::CONTENT_LENGTH, "42".parse().unwrap());
        assert_eq!(response_bytes(&sized), Some(42));
    }

    #[test]
    fn renders_prometheus_text() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("forge_http_requests_total", "route" => "/v1/addons/{slug}", "client" => "launcher").increment(2);
        });
        let text = handle.render();
        assert!(text.contains(r#"forge_http_requests_total{route="/v1/addons/{slug}",client="launcher"} 2"#), "{text}");
    }
}
