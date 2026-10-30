//! GitHub login, run by the API for every client:
//!
//! 1. `GET /v1/auth/github/start?return_to=<client url>` stores `state` + PKCE verifier and sends
//!    the browser to GitHub.
//! 2. `GET /v1/auth/github/callback` trades GitHub's code for the user, upserts them and sends the
//!    browser back to `return_to?code=<one-time code>`.
//! 3. The client posts that code to `POST /v1/auth/exchange` and gets a bearer token.
//!
//! The GitHub user token is used once for `GET /user` and then dropped.

use super::{hash, random_token};
use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const STATE_TTL: Duration = Duration::minutes(10);
const CODE_TTL: Duration = Duration::seconds(60);
pub const SESSION_TTL: Duration = Duration::days(30);

fn bad_request(code: &'static str, message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

pub fn callback_url(state: &AppState) -> String {
    format!("{}/v1/auth/github/callback", state.config.public_api_url)
}

#[derive(Deserialize)]
pub struct StartQuery {
    return_to: String,
}

pub async fn start(State(state): State<AppState>, Query(query): Query<StartQuery>) -> ApiResult<Redirect> {
    if !state.config.auth_return_urls.contains(&query.return_to) {
        return Err(bad_request("invalid_return_to", "return_to is not an allowed client URL"));
    }
    let oauth_state = random_token();
    let verifier = random_token();
    sqlx::query("INSERT INTO oauth_states (state_hash, verifier, return_to, expires_at) VALUES ($1, $2, $3, $4)")
        .bind(hash(&oauth_state))
        .bind(&verifier)
        .bind(&query.return_to)
        .bind((state.clock)() + STATE_TTL)
        .execute(&state.pool)
        .await?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let authorize = Url::parse_with_params(
        &format!("{}/login/oauth/authorize", state.config.github_oauth_url),
        &[
            ("client_id", state.config.github_client_id.as_str()),
            ("redirect_uri", &callback_url(&state)),
            ("state", &oauth_state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("allow_signup", "true"),
        ],
    )
    .map_err(|_| ApiError::internal())?;
    Ok(Redirect::to(authorize.as_str()))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
}

fn back_to(return_to: &str, key: &str, value: &str) -> ApiResult<Redirect> {
    let mut url = Url::parse(return_to).map_err(|_| ApiError::internal())?;
    url.query_pairs_mut().append_pair(key, value);
    Ok(Redirect::to(url.as_str()))
}

pub async fn callback(State(state): State<AppState>, Query(query): Query<CallbackQuery>) -> ApiResult<Redirect> {
    let now = (state.clock)();
    let invalid = || bad_request("invalid_state", "The login link is invalid or expired, start again");
    let oauth_state = query.state.ok_or_else(invalid)?;
    // DELETE ... RETURNING makes every state single-use, even under concurrent callbacks.
    let stored: Option<(String, String, DateTime<Utc>)> =
        sqlx::query_as("DELETE FROM oauth_states WHERE state_hash = $1 RETURNING verifier, return_to, expires_at")
            .bind(hash(&oauth_state))
            .fetch_optional(&state.pool)
            .await?;
    let (verifier, return_to, _) = stored.filter(|(_, _, expires)| *expires > now).ok_or_else(invalid)?;
    let Some(code) = query.code else { return back_to(&return_to, "error", "denied") };

    let github_user = state.github.oauth_user(&code, &verifier, &callback_url(&state)).await.map_err(|e| {
        tracing::warn!(error = %e, "GitHub login failed");
        ApiError::new(StatusCode::BAD_GATEWAY, "github_login_failed", "GitHub login failed, try again")
    })?;
    let role = if state.config.admin_github_ids.contains(&github_user.id) { "admin" } else { "user" };
    let (user_id, banned): (i64, bool) = sqlx::query_as(
        "INSERT INTO users (github_id, login, avatar_url, role, last_login_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (github_id) DO UPDATE
             SET login = EXCLUDED.login, avatar_url = EXCLUDED.avatar_url,
                 role = CASE WHEN EXCLUDED.role = 'admin' THEN 'admin' ELSE coalesce(users.granted_role, 'user') END,
                 last_login_at = EXCLUDED.last_login_at
         RETURNING id, banned_at IS NOT NULL",
    )
    .bind(github_user.id)
    .bind(&github_user.login)
    .bind(&github_user.avatar_url)
    .bind(role)
    .bind(now)
    .fetch_one(&state.pool)
    .await?;
    if banned {
        return back_to(&return_to, "error", "banned");
    }
    let login_code = random_token();
    sqlx::query("INSERT INTO login_codes (code_hash, user_id, return_to, expires_at) VALUES ($1, $2, $3, $4)")
        .bind(hash(&login_code))
        .bind(user_id)
        .bind(&return_to)
        .bind(now + CODE_TTL)
        .execute(&state.pool)
        .await?;
    back_to(&return_to, "code", &login_code)
}

#[derive(Deserialize)]
pub struct ExchangeBody {
    code: String,
    return_to: String,
}

pub async fn exchange(State(state): State<AppState>, Json(body): Json<ExchangeBody>) -> ApiResult<Json<serde_json::Value>> {
    let now = (state.clock)();
    let stored: Option<(i64, String, DateTime<Utc>)> =
        sqlx::query_as("DELETE FROM login_codes WHERE code_hash = $1 RETURNING user_id, return_to, expires_at")
            .bind(hash(&body.code))
            .fetch_optional(&state.pool)
            .await?;
    let (user_id, _, _) = stored
        .filter(|(_, return_to, expires)| *expires > now && *return_to == body.return_to)
        .ok_or_else(|| bad_request("invalid_code", "The login code is invalid or expired"))?;
    let token = random_token();
    let expires_at = now + SESSION_TTL;
    sqlx::query("INSERT INTO sessions (user_id, token_hash, created_at, expires_at) VALUES ($1, $2, $3, $4)")
        .bind(user_id)
        .bind(hash(&token))
        .bind(now)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;
    Ok(Json(serde_json::json!({ "token": token, "expires_at": expires_at })))
}
