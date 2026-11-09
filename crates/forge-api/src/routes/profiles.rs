//! `/v1/me/profiles`: the launcher's profiles of the logged-in user, for another computer of theirs.

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

/// At most this many profiles per user, each at most this big.
pub const MAX_PROFILES: i64 = 20;
pub const MAX_BYTES: usize = 64 * 1024;
const NAME_MAX: usize = 40;

/// `GET /v1/me/profiles`: the user's profiles, oldest name first.
pub async fn list(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, Value, i64)> =
        sqlx::query_as("SELECT name, data, updated_at FROM account_profiles WHERE user_id = $1 ORDER BY lower(name)")
            .bind(user.id)
            .fetch_all(&state.pool)
            .await?;
    let profiles: Vec<Value> = rows.into_iter().map(|(name, data, updated_at)| json!({ "name": name, "data": data, "updated_at": updated_at })).collect();
    Ok(Json(json!({ "profiles": profiles })))
}

#[derive(Deserialize)]
pub struct Put {
    data: Value,
    updated_at: i64,
    /// The `updated_at` the launcher last had from here: a newer one here means another computer
    /// changed it meanwhile, and the write is refused.
    #[serde(default)]
    base_updated_at: Option<i64>,
}

fn valid_name(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_name", format!("A profile name has 1 to {NAME_MAX} characters")));
    }
    Ok(name)
}

/// `PUT /v1/me/profiles/{name}`: adds the profile or replaces the one of that name.
pub async fn put(State(state): State<AppState>, user: CurrentUser, Path(name): Path<String>, Json(body): Json<Put>) -> ApiResult<Json<Value>> {
    let name = valid_name(&name)?;
    if serde_json::to_vec(&body.data).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_BYTES {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "profile_too_big", "A profile has at most 64 KiB"));
    }
    let mut tx = state.pool.begin().await?;
    // One writer per user at a time, so two computers cannot both pass the count.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('account_profiles'), $1::int)").bind(user.id as i32).execute(&mut *tx).await?;
    let current: Option<i64> = sqlx::query_scalar("SELECT updated_at FROM account_profiles WHERE user_id = $1 AND lower(name) = lower($2)")
        .bind(user.id)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
    match current {
        Some(here) if body.base_updated_at.is_some_and(|base| here > base) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "profile_changed", "Another computer changed this profile meanwhile"));
        }
        Some(_) => {
            sqlx::query("UPDATE account_profiles SET name = $3, data = $4, updated_at = $5 WHERE user_id = $1 AND lower(name) = lower($2)")
                .bind(user.id)
                .bind(name)
                .bind(name)
                .bind(&body.data)
                .bind(body.updated_at)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM account_profiles WHERE user_id = $1").bind(user.id).fetch_one(&mut *tx).await?;
            if count >= MAX_PROFILES {
                return Err(ApiError::new(StatusCode::CONFLICT, "too_many_profiles", format!("At most {MAX_PROFILES} profiles per account")));
            }
            sqlx::query("INSERT INTO account_profiles (user_id, name, data, updated_at) VALUES ($1, $2, $3, $4)")
                .bind(user.id)
                .bind(name)
                .bind(&body.data)
                .bind(body.updated_at)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(json!({ "name": name, "updated_at": body.updated_at })))
}

/// `DELETE /v1/me/profiles/{name}`: gone, or never there.
pub async fn delete(State(state): State<AppState>, user: CurrentUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM account_profiles WHERE user_id = $1 AND lower(name) = lower($2)").bind(user.id).bind(name.trim()).execute(&state.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}
