//! `/v1/game-versions` (public list) and `/v1/admin/game-versions` (admins keep the list).

use super::addons::{load_addon, not_blocked, require, user_permissions};
use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::{StaffUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let list: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object('id', id, 'name', name, 'released_at', released_at, 'sort_key', sort_key)
                                  ORDER BY sort_key, id), '[]') FROM game_versions",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(list))
}

/// The id of the game version called `name`.
pub async fn game_version_id(pool: &PgPool, name: &str) -> ApiResult<i32> {
    sqlx::query_scalar("SELECT id FROM game_versions WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_game_version", format!("No game version {name}")))
}

fn validate_name(name: &str) -> ApiResult<()> {
    let ok = !name.trim().is_empty() && name.len() <= 32 && !name.chars().any(char::is_control);
    if ok { Ok(()) } else { Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name", "A game version name is 1 to 32 characters")) }
}

#[derive(Deserialize)]
pub struct GameVersionBody {
    name: Option<String>,
    released_at: Option<NaiveDate>,
    sort_key: Option<i32>,
    #[serde(default)]
    note: String,
}

fn map_unique(e: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(db) = &e
        && db.is_unique_violation()
    {
        return ApiError::new(StatusCode::CONFLICT, "name_taken", "This game version exists already");
    }
    e.into()
}

pub async fn create(State(state): State<AppState>, StaffUser(admin): StaffUser, Json(body): Json<GameVersionBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = body.name.unwrap_or_default();
    validate_name(&name)?;
    let sort_key = body.sort_key.ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_sort_key", "sort_key is required"))?;
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let id: i32 = sqlx::query_scalar("INSERT INTO game_versions (name, released_at, sort_key) VALUES ($1, $2, $3) RETURNING id")
        .bind(name.trim())
        .bind(body.released_at)
        .bind(sort_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
    let details = json!({ "name": name.trim(), "sort_key": sort_key });
    let entry = Entry { admin_id: admin.id, action: "game_version_create", target_type: "game_version", target_id: id.to_string(), note: &body.note, details };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name.trim(), "released_at": body.released_at, "sort_key": sort_key }))))
}

pub async fn update(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(id): Path<i32>,
    Json(body): Json<GameVersionBody>,
) -> ApiResult<StatusCode> {
    if let Some(name) = &body.name {
        validate_name(name)?;
    }
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE game_versions SET name = coalesce($2, name), released_at = coalesce($3, released_at), sort_key = coalesce($4, sort_key)
         WHERE id = $1",
    )
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.released_at)
    .bind(body.sort_key)
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    let details = json!({ "name": body.name, "sort_key": body.sort_key, "released_at": body.released_at });
    let entry = Entry { admin_id: admin.id, action: "game_version_update", target_type: "game_version", target_id: id.to_string(), note: &body.note, details };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(id): Path<i32>) -> ApiResult<StatusCode> {
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM versions WHERE game_min_id = $1 OR game_max_id = $1)")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if used {
        return Err(ApiError::new(StatusCode::CONFLICT, "game_version_in_use", "Addon versions still name this game version"));
    }
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let name: Option<String> = sqlx::query_scalar("DELETE FROM game_versions WHERE id = $1 RETURNING name").bind(id).fetch_optional(&mut *tx).await?;
    let name = name.ok_or_else(ApiError::not_found)?;
    let entry = Entry { admin_id: admin.id, action: "game_version_delete", target_type: "game_version", target_id: id.to_string(), note: "", details: json!({ "name": name }) };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CompatBody {
    game_min: Option<String>,
    game_max: Option<String>,
}

/// `PATCH /v1/addons/{slug}/versions/{tag}`: the owner states which game versions it runs on.
pub async fn set_range(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((slug, tag)): Path<(String, String)>,
    Json(body): Json<CompatBody>,
) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    require(user_permissions(&state.pool, &addon, &user).await?.owner, "Only the owner sets compatibility")?;
    let min = match &body.game_min {
        Some(name) => Some(game_version_id(&state.pool, name).await?),
        None => None,
    };
    let max = match &body.game_max {
        Some(name) => Some(game_version_id(&state.pool, name).await?),
        None => None,
    };
    if let (Some(min), Some(max)) = (min, max) {
        let ordered: bool = sqlx::query_scalar("SELECT (SELECT sort_key FROM game_versions WHERE id = $1) <= (SELECT sort_key FROM game_versions WHERE id = $2)")
            .bind(min)
            .bind(max)
            .fetch_one(&state.pool)
            .await?;
        if !ordered {
            return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_game_range", "The last game version lies before the first"));
        }
    }
    let changed = sqlx::query("UPDATE versions SET game_min_id = $3, game_max_id = $4 WHERE addon_id = $1 AND tag = $2")
        .bind(addon.id)
        .bind(&tag)
        .bind(min)
        .bind(max)
        .execute(&state.pool)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "tag": tag, "game_min": body.game_min, "game_max": body.game_max })))
}
