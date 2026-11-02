//! GitHub login, run by the API for every client:
//!
//! 1. `GET /v1/auth/github/start?return_to=<client url>` stores `state` + PKCE verifier and sends
//!    the browser to GitHub.
//! 2. `GET /v1/auth/github/callback` trades GitHub's code for the user, upserts them and sends the
//!    browser back to `return_to?code=<one-time code>`.
//! 3. The client posts that code to `POST /v1/auth/exchange` and gets a bearer token.
//!
//! The GitHub user token reads the user and goes to the client with the bearer token, if it asks
//! (`github_token`); the database never holds it.

use super::{CurrentUser, hash, random_token};
use crate::app::AppState;
use crate::github::types::UserToken;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
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

    let (github_user, github_token) = state.github.oauth_user(&code, &verifier, &callback_url(&state)).await.map_err(|e| {
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
    keep_github_token(hash(&login_code), github_token, now + CODE_TTL, now);
    back_to(&return_to, "code", &login_code)
}

/// The GitHub tokens of logins whose code the client has not exchanged yet, by code hash. They
/// never touch the database: the website keeps them in its encrypted cookie.
// ponytail: per-process, one API instance only; keep them next to `login_codes` when the API scales out.
type Pending = HashMap<Vec<u8>, (UserToken, DateTime<Utc>)>;
static PENDING_GITHUB_TOKENS: LazyLock<Mutex<Pending>> = LazyLock::new(Default::default);

fn keep_github_token(code_hash: Vec<u8>, token: UserToken, expires_at: DateTime<Utc>, now: DateTime<Utc>) {
    let mut pending = PENDING_GITHUB_TOKENS.lock().unwrap_or_else(|e| e.into_inner());
    pending.retain(|_, (_, expires)| *expires > now);
    pending.insert(code_hash, (token, expires_at));
}

/// Takes the token of a login code out; an expired one is gone either way.
fn take_github_token(code_hash: &[u8], now: DateTime<Utc>) -> Option<UserToken> {
    let mut pending = PENDING_GITHUB_TOKENS.lock().unwrap_or_else(|e| e.into_inner());
    pending.remove(code_hash).filter(|(_, expires)| *expires > now).map(|(token, _)| token)
}

#[derive(Deserialize)]
pub struct ExchangeBody {
    code: String,
    return_to: String,
    /// The website asks for the user's GitHub token, to post issues as them.
    #[serde(default)]
    github_token: bool,
}

pub async fn exchange(State(state): State<AppState>, Json(body): Json<ExchangeBody>) -> ApiResult<Json<serde_json::Value>> {
    let now = (state.clock)();
    let code_hash = hash(&body.code);
    let github_token = take_github_token(&code_hash, now);
    let stored: Option<(i64, String, DateTime<Utc>)> =
        sqlx::query_as("DELETE FROM login_codes WHERE code_hash = $1 RETURNING user_id, return_to, expires_at")
            .bind(&code_hash)
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
    let mut answer = serde_json::json!({ "token": token, "expires_at": expires_at });
    if let Some(github) = github_token.filter(|_| body.github_token) {
        answer["github_token"] = serde_json::to_value(github).map_err(|_| ApiError::internal())?;
    }
    Ok(Json(answer))
}

#[derive(Deserialize)]
pub struct RefreshBody {
    refresh_token: String,
}

/// `POST /v1/auth/github/refresh`: a new GitHub token for the logged-in user's expired one. Only
/// the API knows the App's client secret this takes.
pub async fn refresh_github_token(State(state): State<AppState>, user: CurrentUser, Json(body): Json<RefreshBody>) -> ApiResult<Json<UserToken>> {
    let reauth = || ApiError::new(StatusCode::FORBIDDEN, "github_reauth", "Log in again to post on GitHub");
    let token = state.github.refresh_user_token(&body.refresh_token).await.map_err(|e| {
        tracing::info!(error = %e, "GitHub token refresh failed");
        reauth()
    })?;
    // A refresh token of someone else gives their account nothing here.
    let owner = state.github.token_user(&token.access_token).await.map_err(|_| reauth())?;
    if owner.id != user.github_id {
        return Err(reauth());
    }
    Ok(Json(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(n: u8) -> UserToken {
        UserToken { access_token: format!("gho-{n}"), expires_at: None, refresh_token: None, refresh_token_expires_at: None }
    }

    #[test]
    fn github_tokens_wait_only_as_long_as_their_code() {
        let now = Utc::now();
        keep_github_token(vec![200, 1], token(1), now + CODE_TTL, now);
        assert_eq!(take_github_token(&[200, 1], now + Duration::seconds(10)), Some(token(1)));
        assert_eq!(take_github_token(&[200, 1], now + Duration::seconds(10)), None, "handed out once");
        keep_github_token(vec![200, 2], token(2), now + CODE_TTL, now);
        assert_eq!(take_github_token(&[200, 2], now + CODE_TTL + Duration::seconds(1)), None, "gone with the code");
        keep_github_token(vec![200, 3], token(3), now + CODE_TTL, now);
        keep_github_token(vec![200, 4], token(4), now + Duration::hours(1), now + Duration::minutes(2));
        assert!(!PENDING_GITHUB_TOKENS.lock().unwrap().contains_key(&vec![200, 3]), "a new login clears the expired ones");
    }
}
