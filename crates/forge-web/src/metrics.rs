//! Prometheus metrics of the website on `WEB_METRICS_BIND_ADDR`: requests by route template and
//! status, and their duration. Caddy never forwards that address.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use std::time::Instant;

/// Counts every page request except the container healthcheck.
pub async fn middleware(request: Request, next: Next) -> Response {
    let route = request.extensions().get::<MatchedPath>().map_or("unmatched", MatchedPath::as_str).to_string();
    if route == "/healthz" {
        return next.run(request).await;
    }
    let started = Instant::now();
    let response = next.run(request).await;
    let labels = [("route", route), ("status", response.status().as_u16().to_string())];
    metrics::counter!("forge_web_requests_total", &labels).increment(1);
    metrics::histogram!("forge_web_request_duration_seconds", &labels[..1]).record(started.elapsed().as_secs_f64());
    response
}

/// Installs the recorder and serves `/metrics` until the process ends; errors are only logged.
pub fn spawn_server(addr: std::net::SocketAddr) {
    const SECONDS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
    let Some(handle) = PrometheusBuilder::new().set_buckets_for_metric(Matcher::Suffix("_seconds".into()), SECONDS).ok().and_then(|b| b.install_recorder().ok()) else {
        return;
    };
    tokio::spawn(async move {
        let app = axum::Router::new().route("/metrics", axum::routing::get(move || std::future::ready(handle.render())));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!(error = %e, "metrics server stopped");
                }
            }
            Err(e) => tracing::error!(error = %e, %addr, "cannot listen for metrics"),
        }
    });
}
