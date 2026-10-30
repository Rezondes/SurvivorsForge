//! Admin blocks of addons and versions, and bans of users. Every action needs a note and is audited.

use super::reports::note_required;
use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::{AdminUser, StaffUser};
use forge_core::roles::{Permission, Role};
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, Default)]
pub struct Note {
    #[serde(default)]
    note: String,
}

fn note_of(body: Option<Json<Note>>) -> String {
    body.map(|b| b.0.note).unwrap_or_default()
}

async fn finish(state: &AppState, mut tx: sqlx::Transaction<'_, sqlx::Postgres>, entry: Entry<'_>) -> ApiResult<StatusCode> {
    audit::record(&mut tx, entry, (state.clock)()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn block_addon(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(slug): Path<String>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let mut tx = state.pool.begin().await?;
    let id: Option<i64> = sqlx::query_scalar(
        "UPDATE addons SET blocked_at = $2, block_reason = $3, blocked_by_ban = false WHERE slug = $1 AND deleted_at IS NULL RETURNING id",
    )
    .bind(&slug)
    .bind((state.clock)())
    .bind(note)
    .fetch_optional(&mut *tx)
    .await?;
    let id = id.ok_or_else(ApiError::not_found)?;
    finish(&state, tx, Entry { admin_id: admin.id, action: "block_addon", target_type: "addon", target_id: id.to_string(), note, details: json!({ "slug": slug }) }).await
}

pub async fn unblock_addon(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(slug): Path<String>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let mut tx = state.pool.begin().await?;
    let id: Option<i64> = sqlx::query_scalar(
        "UPDATE addons SET blocked_at = NULL, block_reason = NULL, blocked_by_ban = false
         WHERE slug = $1 AND deleted_at IS NULL AND blocked_at IS NOT NULL RETURNING id",
    )
    .bind(&slug)
    .fetch_optional(&mut *tx)
    .await?;
    let id = id.ok_or_else(ApiError::not_found)?;
    finish(&state, tx, Entry { admin_id: admin.id, action: "unblock_addon", target_type: "addon", target_id: id.to_string(), note, details: json!({ "slug": slug }) }).await
}

pub async fn block_version(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(id): Path<i64>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query("UPDATE versions SET blocked_at = $2, block_reason = $3 WHERE id = $1")
        .bind(id)
        .bind((state.clock)())
        .bind(note)
        .execute(&mut *tx)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    finish(&state, tx, Entry { admin_id: admin.id, action: "block_version", target_type: "version", target_id: id.to_string(), note, details: json!({}) }).await
}

pub async fn unblock_version(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(id): Path<i64>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query("UPDATE versions SET blocked_at = NULL, block_reason = NULL WHERE id = $1 AND blocked_at IS NOT NULL")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    finish(&state, tx, Entry { admin_id: admin.id, action: "unblock_version", target_type: "version", target_id: id.to_string(), note, details: json!({}) }).await
}

/// Bans a user: no login, no access, and their own addons are blocked until the ban is lifted.
pub async fn ban(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(github_id): Path<i64>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let target: Option<(i64, String)> = sqlx::query_as("SELECT id, role FROM users WHERE github_id = $1").bind(github_id).fetch_optional(&state.pool).await?;
    let (user_id, role) = target.ok_or_else(ApiError::not_found)?;
    if role == "admin" || state.config.admin_github_ids.contains(&github_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "cannot_ban_admin", "Admins cannot be banned"));
    }
    if role == "moderator" && !admin.can(Permission::ManageRoles, &state.config) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "cannot_ban_staff", "Only an admin can ban a moderator"));
    }
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE users SET banned_at = $2, ban_reason = $3 WHERE id = $1").bind(user_id).bind(now).bind(note).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(user_id).execute(&mut *tx).await?;
    let blocked = sqlx::query(
        "UPDATE addons SET blocked_at = $2, block_reason = 'owner_banned', blocked_by_ban = true
         WHERE owner_id = $1 AND deleted_at IS NULL AND blocked_at IS NULL",
    )
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let details = json!({ "blocked_addons": blocked });
    finish(&state, tx, Entry { admin_id: admin.id, action: "ban_user", target_type: "user", target_id: github_id.to_string(), note, details }).await
}

#[derive(Deserialize)]
pub struct RoleChange {
    role: String,
    #[serde(default)]
    note: String,
}

/// `PUT /v1/admin/users/{github_id}/role`: gives a user the role `user`, `moderator` or `admin`.
/// Admins from `ADMIN_GITHUB_IDS` and the acting admin keep theirs, so nobody locks the site out.
pub async fn set_role(State(state): State<AppState>, AdminUser(admin): AdminUser, Path(github_id): Path<i64>, Json(body): Json<RoleChange>) -> ApiResult<StatusCode> {
    let note = note_required(&body.note)?;
    let role: Role = body.role.parse().map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_role", "role is user, moderator or admin"))?;
    let target: Option<(String, bool)> = sqlx::query_as("SELECT role, banned_at IS NOT NULL FROM users WHERE github_id = $1").bind(github_id).fetch_optional(&state.pool).await?;
    let (from, banned) = target.ok_or_else(ApiError::not_found)?;
    let by_config = state.config.admin_github_ids.contains(&github_id);
    if role != Role::Admin && by_config {
        return Err(ApiError::new(StatusCode::CONFLICT, "admin_by_config", "This admin is set in ADMIN_GITHUB_IDS; remove the ID there"));
    }
    if role != Role::Admin && github_id == admin.github_id {
        return Err(ApiError::new(StatusCode::CONFLICT, "cannot_demote_self", "Another admin has to take your admin rights"));
    }
    if role.is_staff() && banned {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "banned", "Lift the ban first"));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE users SET granted_role = NULLIF($2, 'user'), role = CASE WHEN $3 THEN 'admin' ELSE $2 END WHERE github_id = $1")
        .bind(github_id)
        .bind(role.as_str())
        .bind(by_config)
        .execute(&mut *tx)
        .await?;
    let entry = Entry { admin_id: admin.id, action: "user_role", target_type: "user", target_id: github_id.to_string(), note, details: json!({ "from": from, "to": role.as_str() }) };
    finish(&state, tx, entry).await
}

/// `GET /v1/admin/admins`: every moderator and admin, and whether the configuration or an admin
/// made them one.
pub async fn admins(State(state): State<AppState>, StaffUser(_): StaffUser) -> ApiResult<Json<Value>> {
    let list: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object('github_id', github_id, 'login', login::text, 'role', role,
                    'by_config', github_id = ANY($1), 'granted', granted_role IS NOT NULL) ORDER BY role, lower(login::text)), '[]')
         FROM users WHERE role <> 'user'",
    )
    .bind(&state.config.admin_github_ids)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(list))
}

pub async fn unban(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(github_id): Path<i64>, body: Option<Json<Note>>) -> ApiResult<StatusCode> {
    let note = note_of(body);
    let note = note_required(&note)?;
    let mut tx = state.pool.begin().await?;
    let user_id: Option<i64> = sqlx::query_scalar(
        "UPDATE users SET banned_at = NULL, ban_reason = NULL WHERE github_id = $1 AND banned_at IS NOT NULL RETURNING id",
    )
    .bind(github_id)
    .fetch_optional(&mut *tx)
    .await?;
    let user_id = user_id.ok_or_else(ApiError::not_found)?;
    sqlx::query("UPDATE addons SET blocked_at = NULL, block_reason = NULL, blocked_by_ban = false WHERE owner_id = $1 AND blocked_by_ban")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    finish(&state, tx, Entry { admin_id: admin.id, action: "unban_user", target_type: "user", target_id: github_id.to_string(), note, details: json!({}) }).await
}

#[derive(Deserialize)]
pub struct NewOwner {
    login: String,
    #[serde(default)]
    note: String,
}

/// Gives an addon to another platform user, e.g. its real author after an import.
pub async fn transfer_owner(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(slug): Path<String>, Json(body): Json<NewOwner>) -> ApiResult<StatusCode> {
    let note = note_required(&body.note)?;
    let user: Option<(i64, i64)> = sqlx::query_as("SELECT id, github_id FROM users WHERE login = $1::citext").bind(body.login.trim()).fetch_optional(&state.pool).await?;
    let (user_id, github_id) = user.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "user_not_found", "No platform user with that login"))?;
    let mut tx = state.pool.begin().await?;
    let addon: Option<(i64, Option<i64>)> = sqlx::query_as(
        "UPDATE addons a SET owner_id = $2 FROM (SELECT id, owner_id FROM addons WHERE slug = $1 AND deleted_at IS NULL) old
         WHERE a.id = old.id RETURNING a.id, old.owner_id",
    )
    .bind(&slug)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (addon_id, previous) = addon.ok_or_else(ApiError::not_found)?;
    sqlx::query("DELETE FROM addon_members WHERE addon_id = $1 AND user_id = $2").bind(addon_id).bind(user_id).execute(&mut *tx).await?;
    let details = json!({ "slug": slug, "new_owner": github_id, "previous_owner_user_id": previous });
    finish(&state, tx, Entry { admin_id: admin.id, action: "transfer_owner", target_type: "addon", target_id: addon_id.to_string(), note, details }).await
}
