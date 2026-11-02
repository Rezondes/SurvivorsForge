//! `/v1/addons/{slug}/comments`: the public discussion under an addon, apart from the GitHub issues
//! of its repo. Replies point at their parent. Deleting only marks a comment, so a thread keeps its
//! shape; deleting someone else's goes into the audit log. Comments are reported through
//! `/v1/reports`. They are off until the owner turns them on (`comments_enabled`).

use super::addons::{AddonRow, load_addon, not_blocked, user_permissions};
use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::Duration;
use forge_core::roles::Permission;
use serde::Deserialize;
use serde_json::{Value, json};

/// Comments on one page, newest first; the API sends one more to show that older ones exist.
pub const PER_PAGE: i64 = 30;
pub const MAX_BODY: usize = 5_000;
pub const MAX_PER_HOUR: i64 = 20;

#[derive(Deserialize)]
pub struct Page {
    /// Comments older than this id.
    pub before: Option<i64>,
}

#[derive(Deserialize)]
pub struct NewComment {
    body: String,
    #[serde(default)]
    parent_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct Changes {
    body: String,
}

/// The trimmed text, or `422 invalid_comment`.
fn text(body: &str) -> ApiResult<&str> {
    let body = body.trim();
    if (1..=MAX_BODY).contains(&body.chars().count()) {
        Ok(body)
    } else {
        Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_comment", "A comment has 1 to 5000 characters"))
    }
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", "You cannot change this comment")
}

/// Not blocked, and the owner turned the comments on.
fn open(addon: &AddonRow) -> ApiResult<()> {
    not_blocked(addon)?;
    if addon.comments_enabled {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "comments_off", "The creator has not turned on comments for this addon"))
    }
}

/// The author of a comment of this addon that is not deleted.
async fn author_of(state: &AppState, addon_id: i64, id: i64) -> ApiResult<i64> {
    let author: Option<i64> = sqlx::query_scalar("SELECT author_id FROM addon_comments WHERE id = $1 AND addon_id = $2 AND deleted_at IS NULL")
        .bind(id)
        .bind(addon_id)
        .fetch_optional(&state.pool)
        .await?;
    author.ok_or_else(ApiError::not_found)
}

pub async fn list(State(state): State<AppState>, Path(slug): Path<String>, Query(page): Query<Page>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    open(&addon)?;
    let comments: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object(
                    'id', c.id, 'deleted', c.deleted_at IS NOT NULL,
                    'body', CASE WHEN c.deleted_at IS NULL THEN c.body ELSE '' END,
                    'author_id', c.author_id, 'author', u.login, 'avatar_url', u.avatar_url,
                    'created_at', c.created_at, 'edited_at', c.edited_at, 'parent_id', c.parent_id)
                ORDER BY c.id DESC), '[]')
         FROM (SELECT * FROM addon_comments WHERE addon_id = $1 AND id < $2 ORDER BY id DESC LIMIT $3) c
         JOIN users u ON u.id = c.author_id",
    )
    .bind(addon.id)
    .bind(page.before.unwrap_or(i64::MAX))
    .bind(PER_PAGE + 1)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(comments))
}

pub async fn create(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>, Json(body): Json<NewComment>) -> ApiResult<(StatusCode, Json<Value>)> {
    let text = text(&body.body)?;
    let addon = load_addon(&state.pool, &slug).await?;
    open(&addon)?;
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    // One write per author at a time, so a burst of requests cannot pass the limit.
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(user.id).execute(&mut *tx).await?;
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM addon_comments WHERE author_id = $1 AND created_at > $2")
        .bind(user.id)
        .bind(now - Duration::hours(1))
        .fetch_one(&mut *tx)
        .await?;
    if recent >= MAX_PER_HOUR {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "comment_limit", "At most 20 comments an hour, try again later"));
    }
    // A reply may also go under a deleted comment, but not under one of another addon.
    if let Some(parent) = body.parent_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addon_comments WHERE id = $1 AND addon_id = $2)")
            .bind(parent)
            .bind(addon.id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(ApiError::not_found());
        }
    }
    let id: i64 = sqlx::query_scalar("INSERT INTO addon_comments (addon_id, author_id, body, parent_id, created_at) VALUES ($1, $2, $3, $4, $5) RETURNING id")
        .bind(addon.id)
        .bind(user.id)
        .bind(text)
        .bind(body.parent_id)
        .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// Only the author changes the text: nobody puts words in someone else's mouth.
pub async fn edit(State(state): State<AppState>, user: CurrentUser, Path((slug, id)): Path<(String, i64)>, Json(body): Json<Changes>) -> ApiResult<StatusCode> {
    let text = text(&body.body)?;
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    if author_of(&state, addon.id, id).await? != user.id {
        return Err(forbidden());
    }
    sqlx::query("UPDATE addon_comments SET body = $2, edited_at = $3 WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .bind(text)
        .bind((state.clock)())
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The author, moderators, and the addon's owner and editors delete a comment; deleting someone
/// else's lands in the audit log with its text.
pub async fn remove(State(state): State<AppState>, user: CurrentUser, Path((slug, id)): Path<(String, i64)>) -> ApiResult<StatusCode> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let author = author_of(&state, addon.id, id).await?;
    if author != user.id && !user.can(Permission::Moderate, &state.config) {
        let rights = user_permissions(&state.pool, &addon, &user).await?;
        if !(rights.owner || rights.admin || rights.edit_description) {
            return Err(forbidden());
        }
    }
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let body: Option<String> = sqlx::query_scalar("UPDATE addon_comments SET deleted_at = $2, deleted_by = $3 WHERE id = $1 AND deleted_at IS NULL RETURNING body")
        .bind(id)
        .bind(now)
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await?;
    let body = body.ok_or_else(ApiError::not_found)?;
    if author != user.id {
        let details = json!({ "addon": addon.slug, "author_id": author, "body": body });
        let entry = Entry { admin_id: user.id, action: "delete_comment", target_type: "comment", target_id: id.to_string(), note: "", details };
        audit::record(&mut tx, entry, now).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn a_comment_counts_characters_not_bytes_and_ignores_outer_space() {
        assert!(text("").is_err());
        assert!(text("  \n\t ").is_err());
        assert_eq!(text("  Merci ! 🎮 ").unwrap(), "Merci ! 🎮");
        assert!(text(&"界".repeat(5_000)).is_ok());
        assert!(text(&"界".repeat(5_001)).is_err());
    }
}
