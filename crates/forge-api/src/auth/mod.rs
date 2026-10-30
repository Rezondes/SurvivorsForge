//! Who is calling: opaque bearer tokens issued by the API after a GitHub login.
//!
//! Clients (the website, later the launcher) keep the token; the database only holds its SHA-256.

pub mod oauth;

use crate::app::AppState;
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crate::config::Config;
use chrono::{DateTime, Utc};
use forge_core::roles::{self, Permission, Role};
use sha2::{Digest, Sha256};

/// 32 random bytes, base64url without padding.
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

pub fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize)]
pub struct CurrentUser {
    pub id: i64,
    pub github_id: i64,
    pub login: String,
    pub avatar_url: String,
    pub role: String,
    #[serde(skip)]
    pub banned_at: Option<DateTime<Utc>>,
}

impl CurrentUser {
    /// Unknown names in the database count as `user`; the column check keeps them out anyway.
    pub fn role(&self) -> Role {
        self.role.parse().unwrap_or(Role::User)
    }

    /// Moderators and admins: they may act on every addon, pack and user like an admin.
    pub fn is_staff(&self) -> bool {
        self.role().is_staff()
    }

    pub fn can(&self, permission: Permission, config: &Config) -> bool {
        roles::allows(self.role(), permission, &config.stats_roles)
    }
}

fn unauthorized() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Log in first")
}

/// The bearer token of the request, if any.
pub fn bearer(parts: &Parts) -> Option<&str> {
    parts.headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or_else(unauthorized)?;
        let user: Option<CurrentUser> = sqlx::query_as(
            "SELECT u.id, u.github_id, u.login::text AS login, u.avatar_url, u.role, u.banned_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > $2",
        )
        .bind(hash(token))
        .bind((state.clock)())
        .fetch_optional(&state.pool)
        .await?;
        let user = user.ok_or_else(unauthorized)?;
        if user.banned_at.is_some() {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "banned", "This account is banned"));
        }
        Ok(user)
    }
}

/// The logged-in user, if they have `permission`; everyone else gets `403`.
async fn permitted(parts: &mut Parts, state: &AppState, permission: Permission, message: &'static str) -> Result<CurrentUser, ApiError> {
    let user = CurrentUser::from_request_parts(parts, state).await?;
    if !user.can(permission, &state.config) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", message));
    }
    Ok(user)
}

/// A logged-in moderator or admin: everything under `/admin` except roles.
pub struct StaffUser(pub CurrentUser);

impl FromRequestParts<AppState> for StaffUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        permitted(parts, state, Permission::Moderate, "Moderators and admins only").await.map(StaffUser)
    }
}

/// A logged-in admin, for what moderators may not do: giving roles.
pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        permitted(parts, state, Permission::ManageRoles, "Admins only").await.map(AdminUser)
    }
}
