//! Public addon discussions, distinct from issue-specific comments.
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::routes::addons::{load_addon, user_permissions, not_blocked};
use axum::{Json, extract::{Path, Query, State}, http::StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct Page { pub before: Option<i64> }
#[derive(Deserialize)]
pub struct Body { pub body: String, #[serde(default)] pub parent_id: Option<i64>, #[serde(default)] pub quote_id: Option<i64> }
#[derive(Deserialize)]
pub struct ReportBody { pub reason: String, #[serde(default)] pub details: String }

fn invalid() -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_comment", "Comment must contain 1-10000 characters")
}
fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", "You cannot modify this comment")
}
fn may_edit(author: i64, actor: i64, admin: bool) -> bool {
    author == actor || admin
}
fn may_delete(author: i64, actor: i64, admin: bool, addon_moderator: bool) -> bool {
    may_edit(author, actor, admin) || addon_moderator
}
fn valid(text: &str) -> bool {
    let n = text.trim().chars().count();
    (1..=10000).contains(&n)
}

pub async fn list(State(state): State<AppState>, Path(slug): Path<String>, Query(page): Query<Page>)
    -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let before = page.before.unwrap_or(i64::MAX);
    let comments: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object(
            'id', c.id, 'body', CASE WHEN c.deleted_at IS NULL THEN c.body ELSE '[deleted]' END, 'deleted', c.deleted_at IS NOT NULL, 'author_id', c.author_id, 'author', u.login,
            'avatar_url', u.avatar_url, 'created_at', c.created_at, 'edited_at', c.edited_at, 'parent_id', c.parent_id, 'quote_id', c.quote_id
        ) ORDER BY c.id DESC), '[]'::json)
        FROM (SELECT * FROM addon_comments
              WHERE addon_id=$1 AND id<$2
              ORDER BY id DESC LIMIT 31) c
        JOIN users u ON u.id=c.author_id"
    ).bind(addon.id).bind(before).fetch_one(&state.pool).await?;
    Ok(Json(comments))
}

pub async fn create(State(state): State<AppState>, user: CurrentUser,
    Path(slug): Path<String>, Json(body): Json<Body>)
    -> ApiResult<(StatusCode, Json<Value>)> {
    if !valid(&body.body) { return Err(invalid()); }
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let now = (state.clock)();
    // Serialize writes per author so concurrent requests cannot bypass the daily cap.
    // Transaction-scoped advisory locks release automatically on commit or rollback.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(user.id)
        .execute(&mut *tx).await?;
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM addon_comments WHERE author_id=$1 AND created_at>$2"
    ).bind(user.id).bind(now - chrono::Duration::hours(24)).fetch_one(&mut *tx).await?;
    if recent >= 100 {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "comment_limit", "Daily comment limit reached"));
    }
    if let Some(parent) = body.parent_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM addon_comments WHERE id=$1 AND addon_id=$2)")
            .bind(parent).bind(addon.id).fetch_one(&mut *tx).await?;
        if !exists { return Err(ApiError::not_found()); }
    }
    if let Some(quote) = body.quote_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM addon_comments WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL)")
            .bind(quote).bind(addon.id).fetch_one(&mut *tx).await?;
        if !exists { return Err(ApiError::not_found()); }
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO addon_comments(addon_id,author_id,body,created_at,parent_id,quote_id) VALUES($1,$2,$3,$4,$5,$6) RETURNING id"
    ).bind(addon.id).bind(user.id).bind(body.body.trim()).bind(now).bind(body.parent_id).bind(body.quote_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}

pub async fn edit(State(state): State<AppState>, user: CurrentUser,
    Path((slug,id)): Path<(String,i64)>, Json(body): Json<Body>) -> ApiResult<Json<Value>> {
    if !valid(&body.body) { return Err(invalid()); }
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let author: Option<i64> = sqlx::query_scalar(
        "SELECT author_id FROM addon_comments WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL"
    ).bind(id).bind(addon.id).fetch_optional(&state.pool).await?;
    let author = author.ok_or_else(ApiError::not_found)?;
    if !may_edit(author, user.id, user.can(forge_core::roles::Permission::Moderate, &state.config)) { return Err(forbidden()); }
    let updated = sqlx::query("UPDATE addon_comments SET body=$3,edited_at=$4 WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL AND author_id=$5")
        .bind(id).bind(addon.id).bind(body.body.trim()).bind((state.clock)()).bind(author).execute(&state.pool).await?;
    if updated.rows_affected() == 0 { return Err(ApiError::not_found()); }
    Ok(Json(json!({"id":id,"edited":true})))
}

pub async fn remove(State(state): State<AppState>, user: CurrentUser,
    Path((slug,id)): Path<(String,i64)>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let author: Option<i64> = sqlx::query_scalar(
        "SELECT author_id FROM addon_comments WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL"
    ).bind(id).bind(addon.id).fetch_optional(&state.pool).await?;
    let author = author.ok_or_else(ApiError::not_found)?;
    let rights = user_permissions(&state.pool, &addon, &user).await?;
    if !may_delete(author, user.id, user.can(forge_core::roles::Permission::Moderate, &state.config), rights.owner || rights.admin || rights.edit_description) {
        return Err(forbidden());
    }
    let removed = sqlx::query("UPDATE addon_comments SET deleted_at=$3,deleted_by=$4 WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL")
        .bind(id).bind(addon.id).bind((state.clock)()).bind(user.id).execute(&state.pool).await?;
    if removed.rows_affected() == 0 { return Err(ApiError::not_found()); }
    Ok(Json(json!({"id":id,"deleted":true})))
}


pub async fn report(State(state): State<AppState>, user: CurrentUser,
    Path((slug,id)): Path<(String,i64)>, Json(body): Json<ReportBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    if !matches!(body.reason.as_str(), "spam"|"harassment"|"hate"|"unsafe"|"other")
        || body.details.chars().count() > 2000 {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_report", "Invalid report reason or details"));
    }
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let author: Option<i64> = sqlx::query_scalar(
        "SELECT author_id FROM addon_comments WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL"
    ).bind(id).bind(addon.id).fetch_optional(&state.pool).await?;
    let author = author.ok_or_else(ApiError::not_found)?;
    if author == user.id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "own_comment", "Cannot report your own comment"));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(user.id).execute(&mut *tx).await?;
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM addon_comment_reports WHERE reporter_id=$1 AND created_at > $2"
    ).bind(user.id).bind((state.clock)() - chrono::Duration::days(1))
        .fetch_one(&mut *tx).await?;
    if recent >= 10 {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "report_limit", "At most 10 reports a day"));
    }
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO addon_comment_reports(comment_id,reporter_id,reason,details)
         VALUES($1,$2,$3,$4) ON CONFLICT(comment_id,reporter_id) DO NOTHING RETURNING id"
    ).bind(id).bind(user.id).bind(&body.reason).bind(body.details.trim())
        .fetch_optional(&mut *tx).await?;
    if inserted.is_none() {
        return Err(ApiError::new(StatusCode::CONFLICT, "already_reported", "You already reported this comment"));
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"reported":true}))))
}

#[cfg(test)]
mod tests {
    use super::{valid, may_edit, may_delete};

    #[test]
    fn author_admin_and_moderator_permissions() {
        assert!(may_edit(1, 1, false));
        assert!(may_edit(1, 2, true));
        assert!(!may_edit(1, 2, false));
        assert!(may_delete(1, 1, false, false));
        assert!(may_delete(1, 2, true, false));
        assert!(may_delete(1, 2, false, true));
        assert!(!may_delete(1, 2, false, false));
        // Addon moderators can remove other users comments but cannot edit them.
        assert!(!may_edit(1, 2, false));
    }


    #[test]
    fn rejects_blank_and_whitespace_only_comments() {
        assert!(!valid(""));
        assert!(!valid("  \n\t "));
    }

    #[test]
    fn accepts_unicode_and_boundary_lengths() {
        assert!(valid("Merci pour cet addon ! 🎮"));
        assert!(valid(&"é".repeat(10_000)));
        assert!(!valid(&"é".repeat(10_001)));
    }

    #[test]
    fn counts_unicode_scalars_not_utf8_bytes() {
        assert!(valid(&"界".repeat(10_000)));
        assert!(!valid(&"界".repeat(10_001)));
    }
}
