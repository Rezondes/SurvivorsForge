//! The HTTP router of the API.

use crate::cache::{self, ResponseCache};
use crate::config::Config;
use crate::auth::oauth;
use crate::error::ApiError;
use crate::github::{Clock, GitHubApi};
use crate::routes;
use crate::rate_limit::{self, RateLimiter};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, extract::State, middleware};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub github: Arc<dyn GitHubApi>,
    pub clock: Clock,
}

pub fn router(state: AppState) -> Router {
    let config = state.config.clone();
    let routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/auth/github/start", get(oauth::start))
        .route("/v1/auth/github/callback", get(oauth::callback))
        .route("/v1/auth/exchange", post(oauth::exchange))
        .route("/v1/auth/logout", post(routes::me::logout))
        .route("/v1/me", get(routes::me::me))
        .route("/v1/me/addons", get(routes::creator::my_addons))
        .route("/v1/me/accounts", get(routes::me::accounts))
        .route("/v1/me/repos", get(routes::me::repos))
        .route("/v1/me/repos/{owner}/{repo}/suggestion", get(routes::me::suggestion))
        .route("/v1/addons/{slug}/permissions", get(routes::creator::my_permissions))
        .route("/v1/categories", get(routes::creator::categories))
        .route("/v1/addons", get(routes::search::list).post(routes::addons::create))
        .route(
            "/v1/addons/{slug}",
            get(routes::addons::show).patch(routes::addons::update).delete(routes::addons::delete),
        )
        .route("/v1/addons/{slug}/versions", get(routes::versions::list))
        .route(
            "/v1/addons/{slug}/images",
            post(routes::images::upload).layer(axum::extract::DefaultBodyLimit::max(crate::images::MAX_UPLOAD + 64 * 1024)),
        )
        .route(
            "/v1/addons/{slug}/images/{id}",
            axum::routing::patch(routes::images::update).delete(routes::images::delete),
        )
        .route("/v1/addons/{slug}/icon", axum::routing::delete(routes::images::delete_icon))
        .route("/v1/addons/{slug}/dependencies", axum::routing::put(routes::dependencies::put))
        .route("/v1/addons/{slug}/versions/{tag}/findings", get(routes::versions::findings))
        .route("/v1/addons/{slug}/versions/{tag}", axum::routing::patch(routes::game_versions::set_range))
        .route("/v1/game-versions", get(routes::game_versions::list))
        .route("/v1/admin/game-versions", post(routes::game_versions::create))
        .route(
            "/v1/admin/game-versions/{id}",
            axum::routing::patch(routes::game_versions::update).delete(routes::game_versions::delete),
        )
        .route("/v1/github/webhook", post(routes::webhooks::webhook))
        .route(
            "/v1/admin/versions/{id}/verification",
            post(routes::admin_verify::verify).delete(routes::admin_verify::unverify),
        )
        .route("/v1/admin/versions/{id}/skip", post(routes::admin_verify::skip))
        .route("/v1/admin/review-queue", get(routes::admin_queue::review_queue))
        .route("/v1/admin/audit", get(routes::admin_queue::audit_log))
        .route("/v1/admin/users/{user}", get(routes::admin_queue::user))
        .route("/v1/admin/categories", post(routes::admin_queue::create_category))
        .route(
            "/v1/admin/categories/{name}",
            axum::routing::patch(routes::admin_queue::update_category).delete(routes::admin_queue::delete_category),
        )
        .route("/v1/reports", post(routes::reports::create))
        .route("/v1/admin/reports", get(routes::reports::queue))
        .route("/v1/admin/reports/{id}/resolve", post(routes::reports::resolve))
        .route(
            "/v1/admin/addons/{slug}/block",
            post(routes::admin_moderation::block_addon).delete(routes::admin_moderation::unblock_addon),
        )
        .route(
            "/v1/admin/versions/{id}/block",
            post(routes::admin_moderation::block_version).delete(routes::admin_moderation::unblock_version),
        )
        .route("/v1/admin/users/{user}/ban", post(routes::admin_moderation::ban).delete(routes::admin_moderation::unban))
        .route("/v1/admin/users/{user}/role", axum::routing::put(routes::admin_moderation::set_role))
        .route("/v1/admin/admins", get(routes::admin_moderation::admins))
        .route("/v1/admin/versions/{id}/accept-source", post(routes::admin_verify::accept_source))
        .route("/v1/addons/{slug}/claim", post(routes::addons::claim))
        .route("/v1/admin/addons", post(routes::addons::admin_create))
        .route("/v1/admin/addons/sync", post(routes::refresh::sync_all))
        .route("/v1/addons/{slug}/sync", post(routes::refresh::sync_one))
        .route("/v1/admin/addons/{slug}/owner", post(routes::admin_moderation::transfer_owner))
        .route("/v1/registry", get(routes::registry::registry))
        .route("/v1/packs", get(routes::packs::list).post(routes::packs::create))
        .route("/v1/packs/{id}", get(routes::packs::show))
        .route("/v1/packs/{id}/download", get(routes::packs::download))
        .route("/v1/github/{owner}/{repo}/releases/latest", get(routes::github_meta::latest_release))
        .route("/v1/github/{owner}/{repo}/releases/{tag}", get(routes::github_meta::release))
        .route("/v1/github/{owner}/{repo}/refs", get(routes::github_meta::refs))
        .route("/v1/addons/{slug}/members", get(routes::members::list).post(routes::members::add))
        .route(
            "/v1/addons/{slug}/members/{github_id}",
            axum::routing::patch(routes::members::update).delete(routes::members::remove),
        )
        .fallback(|| async { ApiError::not_found() })
        .with_state(state);
    with_layers(routes, &config)
}

/// The layers every response passes through: panic guard, rate limit, default headers.
pub fn with_layers(routes: Router, config: &Config) -> Router {
    let limiter = Arc::new(RateLimiter::new(config.rate_limit_per_minute, config.trusted_proxies.clone()));
    let cache = Arc::new(ResponseCache::new(Duration::from_secs(config.cache_seconds)));
    routes
        .layer(CatchPanicLayer::custom(|_| ApiError::internal().into_response()))
        .layer(middleware::from_fn_with_state(cache, cache::middleware))
        .layer(middleware::from_fn_with_state((limiter, config.website_ip), rate_limit::middleware))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
}

async fn healthz(State(state): State<AppState>) -> Response {
    let ping = tokio::time::timeout(Duration::from_secs(2), sqlx::query("SELECT 1").execute(&state.pool)).await;
    if matches!(ping, Ok(Ok(_))) {
        Json(serde_json::json!({ "status": "ok", "db": "ok" })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({ "status": "degraded", "db": "down" }))).into_response()
    }
}
