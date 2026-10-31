//! Counts of API requests per hour, route, method, status and client. They gather in memory, go to
//! `api_request_stats` once a minute, and also feed the Prometheus metrics on `METRICS_BIND_ADDR`.

use axum::extract::{MatchedPath, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

/// The counts since the last flush.
#[derive(Default)]
pub struct RequestStats {
    pending: Mutex<HashMap<Key, Bucket>>,
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

    /// Writes the gathered counts and starts over; on an error they are kept for the next try.
    pub async fn flush(&self, pool: &PgPool) -> sqlx::Result<()> {
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
    for table in ["api_request_stats", "github_request_stats"] {
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

/// Counts every request, also those the rate limit or the cache answered. Container healthchecks
/// (`/healthz`) are left out: they are no API use.
pub async fn middleware(State(recorder): State<Recorder>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let route = request.extensions().get::<MatchedPath>().map_or("unmatched", MatchedPath::as_str).to_string();
    if route == "/healthz" {
        return next.run(request).await;
    }
    let method = request.method().as_str().to_string();
    let user_agent = request.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    let client = classify(user_agent, crate::rate_limit::peer_ip(&request), recorder.website);
    // GitHub requests this request causes count for its route (`github::usage`).
    let origin = format!("{method} {route}");
    let response = crate::github::usage::ORIGIN.scope(origin, next.run(request)).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16();
    recorder.stats.record((recorder.clock)(), &route, &method, status, client, i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    let labels = [("route", route), ("method", method), ("status", status.to_string()), ("client", client.as_str().to_string())];
    metrics::counter!("forge_http_requests_total", &labels).increment(1);
    metrics::histogram!("forge_http_request_duration_seconds", &labels[..2]).record(elapsed.as_secs_f64());
    response
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
