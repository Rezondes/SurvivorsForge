//! Release and tag metadata of any public GitHub repo, for the launcher: it asks us instead of
//! GitHub. Read from github.com web endpoints (no hourly budget, unlike the REST API without a
//! token) and cached by `crate::cache`. The zip files themselves stay on GitHub.

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use forge_core::github_web::{Release, parse_assets, parse_latest_location, parse_refs, tags_from_refs};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

const MAX_BODY: usize = 4 * 1024 * 1024;

/// One client for all calls; redirects are read, not followed (`/releases/latest` answers with one).
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("SurvivorsForge-API")
            .build()
            .expect("HTTP client")
    })
}

fn repo_name(owner: &str, repo: &str) -> ApiResult<String> {
    let valid = |p: &str| !p.is_empty() && p.len() <= 100 && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid(owner) || !valid(repo) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_repo", "Not a GitHub repo name (owner/repo)"));
    }
    Ok(format!("{owner}/{repo}"))
}

fn unavailable(detail: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%detail, "github.com request failed");
    ApiError::new(StatusCode::BAD_GATEWAY, "github_unavailable", "GitHub cannot be reached right now")
}

/// GETs `path` on github.com; `None` for a redirect, with its `Location`.
async fn fetch(state: &AppState, path: &str) -> ApiResult<Result<String, Option<String>>> {
    let response = client().get(format!("{}{path}", state.config.github_web_url)).send().await.map_err(unavailable)?;
    crate::github::usage::USAGE.record("web", &response, (state.clock)());
    let status = response.status();
    if status.is_redirection() {
        let location = response.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        return Ok(Err(location));
    }
    // github.com answers 401 (asks for a login) for repos that do not exist or are private.
    if status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", "No public GitHub repo or release by that name"));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "github_rate_limited", "GitHub is busy, try again later"));
    }
    if !status.is_success() {
        return Err(unavailable(status));
    }
    if response.content_length().is_some_and(|l| l > MAX_BODY as u64) {
        return Err(unavailable("answer too large"));
    }
    let bytes = response.bytes().await.map_err(unavailable)?;
    if bytes.len() > MAX_BODY {
        return Err(unavailable("answer too large"));
    }
    Ok(Ok(String::from_utf8_lossy(&bytes).into_owned()))
}

async fn release_of(state: &AppState, repo: &str, tag: &str) -> ApiResult<Release> {
    let encoded: String = tag.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~+".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let html = fetch(state, &format!("/{repo}/releases/expanded_assets/{encoded}")).await?.unwrap_or_default();
    Ok(Release { assets: parse_assets(&html, &state.config.github_web_url, repo, tag), tag: tag.to_string() })
}

/// `GET /v1/github/{owner}/{repo}/releases/latest`: the newest full release, `null` when there is none.
pub async fn latest_release(State(state): State<AppState>, Path((owner, repo)): Path<(String, String)>) -> ApiResult<Json<Option<Release>>> {
    let repo = repo_name(&owner, &repo)?;
    let tag = match fetch(&state, &format!("/{repo}/releases/latest")).await? {
        Err(location) => location.as_deref().and_then(parse_latest_location),
        Ok(_) => None,
    };
    match tag {
        Some(tag) => Ok(Json(Some(release_of(&state, &repo, &tag).await?))),
        None => Ok(Json(None)),
    }
}

/// `GET /v1/github/{owner}/{repo}/releases/{tag}`: the assets of `tag`, also of a pre-release.
pub async fn release(State(state): State<AppState>, Path((owner, repo, tag)): Path<(String, String, String)>) -> ApiResult<Json<Release>> {
    let repo = repo_name(&owner, &repo)?;
    if tag.is_empty() || tag.len() > 255 {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_tag", "Not a tag name"));
    }
    Ok(Json(release_of(&state, &repo, &tag).await?))
}

/// `GET /v1/github/{owner}/{repo}/refs`: default branch, branch heads and tags (newest first).
pub async fn refs(State(state): State<AppState>, Path((owner, repo)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let repo = repo_name(&owner, &repo)?;
    let body = fetch(&state, &format!("/{repo}.git/info/refs?service=git-upload-pack")).await?.map_err(|_| unavailable("redirect"))?;
    let refs = parse_refs(&body);
    let branches: serde_json::Map<String, Value> = refs
        .refs
        .iter()
        .filter_map(|(sha, name)| name.strip_prefix("refs/heads/").map(|b| (b.to_string(), json!(sha))))
        .collect();
    Ok(Json(json!({ "head": refs.head, "branches": branches, "tags": tags_from_refs(&refs.refs) })))
}
