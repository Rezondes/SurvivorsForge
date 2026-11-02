//! Owners of an addon no admin has verified yet ask for the check by hand: they post the addon in
//! the Discord channel verified-addons and send the link. The link is not checked here: admins open
//! it, approve, which verifies the version asked about, or reject with a reason.
//!
//! `/v1/addons/{slug}/verification-request` (owner) and `/v1/admin/verification-requests` (staff).

use super::addons::{load_addon, not_blocked, require, user_permissions};
use super::admin_verify::verify_version;
use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::{CurrentUser, StaffUser};
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

/// The channel verified-addons on the Azeroth Survivors Discord.
pub const CHANNEL: &str = "https://discord.com/channels/1557821819032051852/1564402358094405744";

/// The most a link (or whatever the owner sends instead) may hold.
pub const MAX_LINK: usize = 500;

#[derive(Deserialize)]
pub struct Ask {
    #[serde(default)]
    discord_url: String,
}

pub async fn ask(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>, Json(body): Json<Ask>) -> ApiResult<(StatusCode, Json<Value>)> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    require(user_permissions(&state.pool, &addon, &user).await?.owner, "Only the owner asks for a verification")?;
    let url = body.discord_url.trim();
    if url.is_empty() || url.chars().count() > MAX_LINK {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_link", format!("Paste the link to your post in verified-addons (at most {MAX_LINK} characters)")));
    }
    let verified: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM verifications w JOIN versions v ON v.id = w.version_id WHERE v.addon_id = $1)")
        .bind(addon.id)
        .fetch_one(&state.pool)
        .await?;
    if verified {
        return Err(ApiError::new(StatusCode::CONFLICT, "already_verified", "An admin has verified this addon already"));
    }
    // The newest version the scanner checked: the one the admins look at.
    let version: Option<(i64, String)> = sqlx::query_as(
        "SELECT v.id, v.tag FROM versions v JOIN version_trust t ON t.version_id = v.id
         WHERE v.addon_id = $1 AND v.scan_status = 'done' AND v.sha256 IS NOT NULL AND t.trust IN ('scanned', 'flagged')
         ORDER BY v.published_at DESC, v.id DESC LIMIT 1",
    )
    .bind(addon.id)
    .fetch_optional(&state.pool)
    .await?;
    let (version_id, tag) = version.ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "no_checked_version", "No version is checked yet; wait for the scan of a release"))?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO verification_requests (addon_id, version_id, requested_by, discord_url, created_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (addon_id) WHERE status = 'open' DO NOTHING RETURNING id",
    )
    .bind(addon.id)
    .bind(version_id)
    .bind(user.id)
    .bind(url)
    .bind((state.clock)())
    .fetch_optional(&state.pool)
    .await?;
    let id = id.ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "request_open", "A request for this addon is waiting for the admins already"))?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "status": "open", "tag": tag, "discord_url": url }))))
}

/// The newest request of the addon, or `null`.
pub async fn mine(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    require(user_permissions(&state.pool, &addon, &user).await?.owner, "Only the owner sees the verification request")?;
    let latest: Option<Value> = sqlx::query_scalar(
        "SELECT json_build_object('id', r.id, 'status', r.status, 'note', r.note, 'tag', v.tag, 'discord_url', r.discord_url,
                                  'created_at', r.created_at, 'decided_at', r.decided_at)
         FROM verification_requests r JOIN versions v ON v.id = r.version_id
         WHERE r.addon_id = $1 ORDER BY r.created_at DESC, r.id DESC LIMIT 1",
    )
    .bind(addon.id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(Json(latest.unwrap_or(Value::Null)))
}

/// The open requests, oldest first.
pub async fn queue(State(state): State<AppState>, StaffUser(_): StaffUser) -> ApiResult<Json<Value>> {
    Ok(Json(sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object(
                    'id', r.id, 'slug', a.slug, 'name', a.name, 'version_id', v.id, 'tag', v.tag, 'trust', t.trust,
                    'discord_url', r.discord_url, 'requested_by', u.login, 'created_at', r.created_at)
                ORDER BY r.created_at, r.id), '[]')
         FROM verification_requests r
         JOIN addons a ON a.id = r.addon_id
         JOIN versions v ON v.id = r.version_id
         JOIN version_trust t ON t.version_id = v.id
         JOIN users u ON u.id = r.requested_by
         WHERE r.status = 'open'",
    )
    .fetch_one(&state.pool)
    .await?))
}

#[derive(Deserialize, Default)]
pub struct Decision {
    #[serde(default)]
    note: String,
}

pub async fn approve(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(id): Path<i64>, Json(body): Json<Decision>) -> ApiResult<StatusCode> {
    decide(&state, &admin, id, true, body.note.trim()).await
}

pub async fn reject(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(id): Path<i64>, Json(body): Json<Decision>) -> ApiResult<StatusCode> {
    let note = body.note.trim();
    if note.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "note_required", "Tell the creator why"));
    }
    decide(&state, &admin, id, false, note).await
}

async fn decide(state: &AppState, admin: &CurrentUser, id: i64, approve: bool, note: &str) -> ApiResult<StatusCode> {
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    // Locked, so two admins never decide the same request twice.
    let version_id: Option<i64> = sqlx::query_scalar("SELECT version_id FROM verification_requests WHERE id = $1 AND status = 'open' FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let version_id = version_id.ok_or_else(ApiError::not_found)?;
    if approve {
        verify_version(&mut tx, admin.id, version_id, note, now).await?;
    }
    sqlx::query("UPDATE verification_requests SET status = $2, note = $3, decided_by = $4, decided_at = $5 WHERE id = $1")
        .bind(id)
        .bind(if approve { "approved" } else { "rejected" })
        .bind(note)
        .bind(admin.id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let entry = Entry {
        admin_id: admin.id,
        action: if approve { "approve_verification_request" } else { "reject_verification_request" },
        target_type: "verification_request",
        target_id: id.to_string(),
        note,
        details: json!({ "version_id": version_id }),
    };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_is_a_discord_link() {
        assert!(CHANNEL.starts_with("https://discord.com/channels/"));
    }
}
