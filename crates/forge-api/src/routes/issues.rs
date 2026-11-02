//! Internal issue tracker for addon bugs, requests and questions.
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::routes::addons::{load_addon, user_permissions, not_blocked};
use axum::{Json, extract::{Path, Query, State}, http::StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

fn invalid(message: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_issue", message)
}
fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", "Only addon maintainers can change issue status")
}
fn valid_text(text: &str, min: usize, max: usize) -> bool {
    let n = text.trim().chars().count();
    n >= min && n <= max
}
#[derive(Deserialize)]
pub struct NewIssue { title: String, body: String, #[serde(default = "default_kind")] kind: String }
fn default_kind() -> String { "bug".to_owned() }
#[derive(Deserialize)]
pub struct NewComment { body: String }
#[derive(Deserialize)]
pub struct ChangeStatus { status: String }

#[derive(Deserialize)]
pub struct IssuePageQuery { pub before: Option<i64> }

pub async fn list(State(state): State<AppState>, Path(slug): Path<String>, Query(page): Query<IssuePageQuery>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let items: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object('id', i.id, 'title', i.title, 'kind', i.kind,
          'status', i.status, 'author', u.login, 'author_id', i.author_id, 'created_at', i.created_at, 'updated_at', i.updated_at)
          ORDER BY i.id DESC), '[]'::json)
         FROM (SELECT * FROM addon_issues WHERE addon_id = $1 AND deleted_at IS NULL AND id < $2 ORDER BY id DESC LIMIT 31) i
         JOIN users u ON u.id = i.author_id"
    ).bind(addon.id).bind(page.before.unwrap_or(i64::MAX)).fetch_one(&state.pool).await?;
    Ok(Json(items))
}
pub async fn create(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>, Json(body): Json<NewIssue>)
    -> ApiResult<(StatusCode, Json<Value>)> {
    if !valid_text(&body.title, 3, 160) || !valid_text(&body.body, 1, 10000) ||
        !matches!(body.kind.as_str(), "bug" | "feature" | "question") {
        return Err(invalid("Title must be 3-160 characters, body 1-10000, kind bug/feature/question"));
    }
    let addon = load_addon(&state.pool, &slug).await?;
    if addon.blocked_at.is_some() { return Err(forbidden()); }
    let now = (state.clock)();
    // Serialize submissions by author: simultaneous double-clicks must not create duplicates.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(user.id).execute(&mut *tx).await?;
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM addon_issues WHERE addon_id=$1 AND author_id=$2
         AND kind=$3 AND lower(btrim(title))=lower($4) AND btrim(body)=$5
         AND deleted_at IS NULL AND created_at > $6)"
    ).bind(addon.id).bind(user.id).bind(&body.kind).bind(body.title.trim())
     .bind(body.body.trim()).bind(now - chrono::Duration::hours(24))
     .fetch_one(&mut *tx).await?;
    if duplicate {
        return Err(ApiError::new(StatusCode::CONFLICT, "duplicate_issue",
            "You already submitted this exact issue in the last 24 hours"));
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM addon_issues WHERE author_id = $1 AND created_at > $2"
    ).bind(user.id).bind(now - chrono::Duration::hours(24))
     .fetch_one(&mut *tx).await?;
    if recent >= 20 { return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "issue_limit", "Daily issue limit reached")); }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO addon_issues (addon_id, author_id, title, body, kind, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$6) RETURNING id"
    ).bind(addon.id).bind(user.id).bind(body.title.trim()).bind(body.body.trim())
     .bind(&body.kind).bind(now).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}
pub async fn show(State(state): State<AppState>, Path((slug, id)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let issue: Option<Value> = sqlx::query_scalar(
        "SELECT json_build_object('id', i.id, 'title', i.title, 'body', i.body, 'kind', i.kind,
         'status', i.status, 'author', u.login, 'author_id', i.author_id, 'created_at', i.created_at, 'updated_at', i.updated_at, 'edited_at', i.edited_at, 'acknowledged_at', i.acknowledged_at,
         'events', (SELECT coalesce(json_agg(json_build_object('action', e.action, 'old_value', e.old_value, 'new_value', e.new_value, 'actor', eu.login, 'created_at', e.created_at) ORDER BY e.created_at, e.id), '[]'::json) FROM addon_issue_events e JOIN users eu ON eu.id=e.actor_id WHERE e.issue_id=i.id), 'comments', (SELECT coalesce(json_agg(json_build_object('id', c.id, 'body', CASE WHEN c.deleted_at IS NULL THEN c.body ELSE '[deleted]' END,
         'author', cu.login, 'author_id', c.author_id, 'created_at', c.created_at, 'edited_at', c.edited_at, 'deleted', c.deleted_at IS NOT NULL) ORDER BY c.created_at, c.id), '[]'::json)
         FROM addon_issue_comments c JOIN users cu ON cu.id = c.author_id WHERE c.issue_id = i.id))
         FROM addon_issues i JOIN users u ON u.id = i.author_id WHERE i.id = $1 AND i.addon_id = $2 AND i.deleted_at IS NULL"
    ).bind(id).bind(addon.id).fetch_optional(&state.pool).await?;
    Ok(Json(issue.ok_or_else(ApiError::not_found)?))
}
pub async fn comment(State(state): State<AppState>, user: CurrentUser, Path((slug, id)): Path<(String, i64)>,
    Json(body): Json<NewComment>) -> ApiResult<(StatusCode, Json<Value>)> {
    if !valid_text(&body.body, 1, 10000) { return Err(invalid("Comment must be 1-10000 characters")); }
    let addon = load_addon(&state.pool, &slug).await?;
    if addon.blocked_at.is_some() { return Err(forbidden()); }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(user.id).execute(&mut *tx).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM addon_issues WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL)"
    ).bind(id).bind(addon.id).fetch_one(&mut *tx).await?;
    if !exists { return Err(ApiError::not_found()); }
    let now = (state.clock)();
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM addon_issue_comments WHERE author_id=$1 AND created_at>$2"
    ).bind(user.id).bind(now - chrono::Duration::hours(24)).fetch_one(&mut *tx).await?;
    if recent >= 100 { return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "comment_limit", "Daily comment limit reached")); }
    let comment_id: i64 = sqlx::query_scalar(
        "INSERT INTO addon_issue_comments(issue_id, author_id, body, created_at) VALUES($1,$2,$3,$4) RETURNING id"
    ).bind(id).bind(user.id).bind(body.body.trim()).bind(now).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id":comment_id}))))
}
pub async fn set_status(State(state): State<AppState>, user: CurrentUser, Path((slug,id)): Path<(String,i64)>,
    Json(body): Json<ChangeStatus>) -> ApiResult<Json<Value>> {
    if !matches!(body.status.as_str(), "open" | "acknowledged" | "in_progress" | "resolved" | "closed") {
        return Err(invalid("Unknown issue status"));
    }
    let addon = load_addon(&state.pool, &slug).await?;
    let rights = user_permissions(&state.pool, &addon, &user).await?;
    if !rights.owner && !rights.admin && !rights.edit_description { return Err(forbidden()); }
    if addon.blocked_at.is_some() { return Err(forbidden()); }
    let mut tx = state.pool.begin().await?;
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT status FROM addon_issues WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL FOR UPDATE"
    ).bind(id).bind(addon.id).fetch_optional(&mut *tx).await?;
    let previous = previous.ok_or_else(ApiError::not_found)?;
    if previous != body.status {
        sqlx::query("UPDATE addon_issues SET status=$3, updated_at=$4, acknowledged_at=CASE WHEN $3 IN ('acknowledged','in_progress','resolved') THEN COALESCE(acknowledged_at,$4) ELSE acknowledged_at END WHERE id=$1 AND addon_id=$2")
            .bind(id).bind(addon.id).bind(&body.status).bind((state.clock)()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO addon_issue_events(issue_id,actor_id,action,old_value,new_value) VALUES($1,$2,'status',$3,$4)")
            .bind(id).bind(user.id).bind(&previous).bind(&body.status).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"id":id,"status":body.status})))
}

#[derive(Deserialize)]
pub struct EditIssue { title: String, body: String, kind: String }
#[derive(Deserialize)]
pub struct EditComment { body: String }

pub async fn edit(State(state): State<AppState>, user: CurrentUser, Path((slug,id)): Path<(String,i64)>,
    Json(body): Json<EditIssue>) -> ApiResult<Json<Value>> {
    if !valid_text(&body.title,3,160) || !valid_text(&body.body,1,10000) ||
        !matches!(body.kind.as_str(),"bug"|"feature"|"question") { return Err(invalid("Invalid issue content")); }
    let addon=load_addon(&state.pool,&slug).await?;
    not_blocked(&addon)?;
    let rights=user_permissions(&state.pool,&addon,&user).await?;
    let privileged=user.can(forge_core::roles::Permission::Moderate, &state.config) || rights.owner;
    let mut tx=state.pool.begin().await?;
    let owner: Option<(i64,Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT author_id,acknowledged_at FROM addon_issues WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL FOR UPDATE")
        .bind(id).bind(addon.id).fetch_optional(&mut *tx).await?;
    let (author,ack)=owner.ok_or_else(ApiError::not_found)?;
    if !(privileged || (author==user.id && ack.is_none())) { return Err(forbidden()); }
    sqlx::query("UPDATE addon_issues SET title=$3,body=$4,kind=$5,edited_at=now(),updated_at=now() WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL")
        .bind(id).bind(addon.id).bind(body.title.trim()).bind(body.body.trim()).bind(&body.kind)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO addon_issue_events(issue_id,actor_id,action) VALUES($1,$2,'issue_edited')")
        .bind(id).bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"edited":true})))
}

pub async fn remove(State(state): State<AppState>, user: CurrentUser, Path((slug,id)): Path<(String,i64)>)
    -> ApiResult<Json<Value>> {
    let addon=load_addon(&state.pool,&slug).await?;
    not_blocked(&addon)?;
    let mut tx=state.pool.begin().await?;
    let owner: Option<(i64,Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT author_id,acknowledged_at FROM addon_issues WHERE id=$1 AND addon_id=$2 AND deleted_at IS NULL FOR UPDATE")
        .bind(id).bind(addon.id).fetch_optional(&mut *tx).await?;
    let (author,ack)=owner.ok_or_else(ApiError::not_found)?;
    if !(user.can(forge_core::roles::Permission::Moderate, &state.config) || (author==user.id && ack.is_none())) { return Err(forbidden()); }
    sqlx::query("UPDATE addon_issues SET deleted_at=now(),updated_at=now() WHERE id=$1")
        .bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO addon_issue_events(issue_id,actor_id,action) VALUES($1,$2,'issue_deleted')")
        .bind(id).bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"deleted":true})))
}

pub async fn edit_comment(State(state): State<AppState>, user: CurrentUser,
    Path((slug,id,comment_id)): Path<(String,i64,i64)>, Json(body): Json<EditComment>) -> ApiResult<Json<Value>> {
    if !valid_text(&body.body,1,10000) { return Err(invalid("Invalid comment")); }
    let addon=load_addon(&state.pool,&slug).await?;
    not_blocked(&addon)?;
    let mut tx=state.pool.begin().await?;
    let author: Option<i64>=sqlx::query_scalar(
        "SELECT c.author_id FROM addon_issue_comments c JOIN addon_issues i ON i.id=c.issue_id WHERE i.addon_id=$1 AND i.id=$2 AND c.id=$3 AND c.deleted_at IS NULL AND i.deleted_at IS NULL FOR UPDATE OF c")
        .bind(addon.id).bind(id).bind(comment_id).fetch_optional(&mut *tx).await?;
    let author=author.ok_or_else(ApiError::not_found)?;
    if author!=user.id && !user.can(forge_core::roles::Permission::Moderate, &state.config) { return Err(forbidden()); }
    let updated=sqlx::query("UPDATE addon_issue_comments SET body=$2,edited_at=now() WHERE id=$1 AND issue_id=$3 AND deleted_at IS NULL")
        .bind(comment_id).bind(body.body.trim()).bind(id).execute(&mut *tx).await?;
    if updated.rows_affected()!=1 { return Err(ApiError::not_found()); }
    sqlx::query("INSERT INTO addon_issue_events(issue_id,actor_id,action,new_value) VALUES($1,$2,'comment_edited',$3)")
        .bind(id).bind(user.id).bind(comment_id.to_string()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":comment_id,"edited":true})))
}

pub async fn remove_comment(State(state): State<AppState>, user: CurrentUser,
    Path((slug,id,comment_id)): Path<(String,i64,i64)>) -> ApiResult<Json<Value>> {
    let addon=load_addon(&state.pool,&slug).await?;
    not_blocked(&addon)?;
    let mut tx=state.pool.begin().await?;
    let author: Option<i64>=sqlx::query_scalar(
        "SELECT c.author_id FROM addon_issue_comments c JOIN addon_issues i ON i.id=c.issue_id WHERE i.addon_id=$1 AND i.id=$2 AND c.id=$3 AND c.deleted_at IS NULL AND i.deleted_at IS NULL FOR UPDATE OF c")
        .bind(addon.id).bind(id).bind(comment_id).fetch_optional(&mut *tx).await?;
    let author=author.ok_or_else(ApiError::not_found)?;
    if author!=user.id && !user.can(forge_core::roles::Permission::Moderate, &state.config) { return Err(forbidden()); }
    let updated=sqlx::query("UPDATE addon_issue_comments SET deleted_at=now(),body='[deleted]' WHERE id=$1 AND issue_id=$2 AND deleted_at IS NULL")
        .bind(comment_id).bind(id).execute(&mut *tx).await?;
    if updated.rows_affected()!=1 { return Err(ApiError::not_found()); }
    sqlx::query("INSERT INTO addon_issue_events(issue_id,actor_id,action,new_value) VALUES($1,$2,'comment_deleted',$3)")
        .bind(id).bind(user.id).bind(comment_id.to_string()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":comment_id,"deleted":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_bounds() {
        assert!(!valid_text("  ",1,100));
        assert!(valid_text("a",1,100));
        assert!(!valid_text("ab",3,160));
        assert!(!valid_text(&"a".repeat(10001),1,10000));
    }
}
