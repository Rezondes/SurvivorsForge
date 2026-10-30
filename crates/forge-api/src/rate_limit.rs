//! Per-IP request limit with a fixed one-minute window.

use crate::error::ApiError;
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

pub use forge_core::rate_limit::RateLimiter;

/// The visitor's address behind Caddy (`TRUSTED_PROXIES`), for handlers that count per visitor.
pub struct VisitorIp(pub IpAddr);

impl axum::extract::FromRequestParts<crate::app::AppState> for VisitorIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut axum::http::request::Parts, state: &crate::app::AppState) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .or_else(|| parts.extensions.get::<MockConnectInfo<SocketAddr>>().map(|c| c.0.ip()))
            .unwrap_or(IpAddr::from([0, 0, 0, 0]));
        let forwarded = parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        Ok(VisitorIp(forge_core::rate_limit::client_ip(&state.config.trusted_proxies, peer, forwarded)))
    }
}

/// The address of the direct sender (Caddy or the website, not the visitor behind them).
pub fn peer_ip(request: &Request) -> IpAddr {
    // Tests set the peer with `MockConnectInfo`.
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| request.extensions().get::<MockConnectInfo<SocketAddr>>().map(|c| c.0.ip()))
        .unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// The limiter and the website's address, whose requests pass uncounted: the website limits each
/// visitor itself, and one of its pages asks the API several times.
pub async fn middleware(State((limiter, website)): State<(Arc<RateLimiter>, Option<IpAddr>)>, request: Request, next: Next) -> Response {
    let peer = peer_ip(&request);
    if Some(peer) == website {
        return next.run(request).await;
    }
    let forwarded = request.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok());
    let ip = limiter.client_ip(peer, forwarded);
    if !limiter.allow(ip, Instant::now()) {
        metrics::counter!("forge_rate_limited_total").increment(1);
        return ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Too many requests, try again in a minute")
            .into_response();
    }
    next.run(request).await
}
