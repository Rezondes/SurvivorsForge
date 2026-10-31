//! Per-IP request limit of the website. The client IP also travels to the API as
//! `X-Forwarded-For`, so the API limits each visitor and not the website as a whole.

use crate::error::error_page;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use forge_core::rate_limit::RateLimiter;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

tokio::task_local! {
    /// The client of the request being handled, for `ApiClient`.
    pub static CLIENT_IP: IpAddr;
}

pub async fn middleware(State(limiter): State<Arc<RateLimiter>>, request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]));
    let forwarded = request.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok());
    let ip = limiter.client_ip(peer, forwarded);
    // Caddy asks before each request to Grafana; a dashboard sends dozens at once.
    let grafana_check = request.uri().path() == "/auth/grafana";
    if !grafana_check && !limiter.allow(ip, Instant::now()) {
        return error_page(StatusCode::TOO_MANY_REQUESTS, "Slow down", "Too many requests, try again in a minute.");
    }
    CLIENT_IP.scope(ip, next.run(request)).await
}
