//! Admin reads and the category list: the review queue, the audit log and user lookup.

use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::StaffUser;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

/// Versions an admin should look at, oldest first: flagged ones, changed files, and new versions
/// of addons whose author already had a verified version. Skipped ones stay off while their trust holds.
pub async fn review_queue(State(state): State<AppState>, StaffUser(_): StaffUser) -> ApiResult<Json<Value>> {
    let list: Value = sqlx::query_scalar(
        "WITH queue AS (
             SELECT v.id, v.tag, v.created_at, v.addon_id, t.trust,
                    (SELECT p.tag FROM versions p WHERE p.addon_id = v.addon_id AND p.published_at < v.published_at
                     ORDER BY p.published_at DESC LIMIT 1) AS previous_tag
             FROM versions v
             JOIN version_trust t ON t.version_id = v.id
             JOIN addons a ON a.id = v.addon_id
             WHERE a.deleted_at IS NULL AND v.deleted_on_github_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM review_skips k WHERE k.version_id = v.id AND k.trust = t.trust)
               AND (t.trust IN ('flagged', 'source_changed')
                    OR (t.trust = 'scanned' AND EXISTS (
                            SELECT 1 FROM verifications w JOIN versions wv ON wv.id = w.version_id WHERE wv.addon_id = a.id))))
         SELECT coalesce(json_agg(json_build_object(
                    'version_id', q.id, 'tag', q.tag, 'trust', q.trust, 'slug', a.slug, 'name', a.name, 'repo', a.repo_full_name,
                    'compare_url', CASE WHEN q.previous_tag IS NULL THEN NULL
                                        ELSE 'https://github.com/' || a.repo_full_name || '/compare/' || q.previous_tag || '...' || q.tag END,
                    'findings', (SELECT coalesce(json_agg(json_build_object(
                                        'rule', f.rule, 'severity', f.severity, 'file', f.file, 'line', f.line, 'message', f.message, 'snippet', f.snippet)
                                        ORDER BY f.severity, f.id), '[]')
                                 FROM findings f WHERE f.scan_id = (SELECT s.id FROM scans s WHERE s.version_id = q.id ORDER BY s.id DESC LIMIT 1)
                                   AND f.severity <> 'info'))
                ORDER BY q.created_at, q.id), '[]')
         FROM queue q JOIN addons a ON a.id = q.addon_id",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct AddonListQuery {
    q: Option<String>,
    page: Option<i64>,
}

pub const ADDON_PAGE: i64 = 50;

/// `GET /v1/admin/addons?q=&page=`: every addon that is not deleted, also blocked ones and ones
/// whose repo cannot be reached, which the public search hides. Searched in name, slug and repo.
pub async fn addons(State(state): State<AppState>, StaffUser(_): StaffUser, Query(q): Query<AddonListQuery>) -> ApiResult<Json<Value>> {
    let page = q.page.unwrap_or(1).max(1);
    // A LIKE pattern of the search text; its own % and _ count literally.
    let pattern = q.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
    });
    let list: Value = sqlx::query_scalar(
        "WITH hits AS (
             SELECT a.* FROM addons a
             WHERE a.deleted_at IS NULL
               AND ($1::text IS NULL OR a.name ILIKE $1 OR a.slug::text ILIKE $1 OR a.repo_full_name ILIKE $1))
         SELECT json_build_object(
             'total', (SELECT count(*) FROM hits), 'page', $2::bigint, 'per_page', $3::bigint,
             'addons', coalesce((SELECT json_agg(json_build_object(
                             'slug', h.slug, 'name', h.name, 'repo', h.repo_full_name, 'owner', u.login,
                             'author', coalesce(u.login::text, h.author_name),
                             'blocked', h.blocked_at IS NOT NULL, 'locked', h.locked_at IS NOT NULL,
                             'latest_tag', v.tag, 'trust', t.trust,
                             'findings', json_build_object('critical', coalesce(t.critical, 0), 'warn', coalesce(t.warn, 0)))
                             ORDER BY lower(h.name), h.id)
                          FROM (SELECT * FROM hits ORDER BY lower(name), id LIMIT $3 OFFSET ($2 - 1) * $3) h
                          LEFT JOIN users u ON u.id = h.owner_id
                          LEFT JOIN versions v ON v.id = relevant_version(h.id)
                          LEFT JOIN version_trust t ON t.version_id = v.id), '[]'))",
    )
    .bind(pattern)
    .bind(page)
    .bind(ADDON_PAGE)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    admin: Option<String>,
    action: Option<String>,
    target_type: Option<String>,
    target_id: Option<String>,
    page: Option<i64>,
}

pub const AUDIT_PAGE: i64 = 50;

pub async fn audit_log(State(state): State<AppState>, StaffUser(_): StaffUser, Query(q): Query<AuditQuery>) -> ApiResult<Json<Value>> {
    let page = q.page.unwrap_or(1).max(1);
    let empty = |v: &Option<String>| v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let page_json: Value = sqlx::query_scalar(&format!(
        "WITH {AUDIT_RESOLVED}, hits AS (
             SELECT * FROM resolved l
             WHERE ($1::text IS NULL OR l.admin_login = $1::citext) AND ($2::text IS NULL OR l.action = $2)
               AND ($3::text IS NULL OR l.target_type = $3) AND ($4::text IS NULL OR l.target_id = $4))
         SELECT json_build_object(
             'total', (SELECT count(*) FROM hits), 'page', $5::bigint, 'per_page', $6::bigint,
             'entries', coalesce((SELECT json_agg({AUDIT_ENTRY} ORDER BY h.id DESC)
                          FROM (SELECT * FROM hits ORDER BY id DESC LIMIT $6 OFFSET ($5 - 1) * $6) h), '[]'))"
    ))
    .bind(empty(&q.admin))
    .bind(empty(&q.action))
    .bind(empty(&q.target_type))
    .bind(empty(&q.target_id))
    .bind(page)
    .bind(AUDIT_PAGE)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(page_json))
}

/// `GET /v1/admin/audit/{id}`: one entry, and the other entries on the same addon (its versions
/// included) or else on the same target, newest first.
pub async fn audit_entry(State(state): State<AppState>, StaffUser(_): StaffUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let entry: Option<Value> = sqlx::query_scalar(&format!(
        "WITH {AUDIT_RESOLVED}
         SELECT jsonb_build_object(
             'history', coalesce((SELECT json_agg({AUDIT_ENTRY} ORDER BY h.id DESC)
                                  FROM (SELECT o.* FROM resolved o
                                        WHERE o.id <> h.id
                                          AND (o.addon_id = h.addon_id OR (o.target_type = h.target_type AND o.target_id = h.target_id))
                                        ORDER BY o.id DESC LIMIT {AUDIT_PAGE}) h), '[]')
         ) || ({AUDIT_ENTRY})::jsonb
         FROM resolved h WHERE h.id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    entry.map(Json).ok_or_else(ApiError::not_found)
}

/// Audit entries with what their target is: the addon (also of a version or a report), the
/// version's tag, the user's login, the game version's name. `label` names it for people.
const AUDIT_RESOLVED: &str = "resolved AS (
    SELECT l.*, u.login::text AS admin_login, coalesce(va.id, aa.id, ra.id) AS addon_id,
           json_build_object(
               'label', CASE l.target_type
                   WHEN 'version' THEN coalesce(va.name || ' ' || v.tag, 'version ' || l.target_id)
                   WHEN 'addon' THEN coalesce(aa.name, l.details->>'slug', 'addon ' || l.target_id)
                   WHEN 'user' THEN coalesce(tu.login::text, 'user ' || l.target_id)
                   WHEN 'report' THEN 'report ' || l.target_id || coalesce(' on ' || ra.name, '')
                   WHEN 'game_version' THEN coalesce(g.name, l.details->>'name', 'game version ' || l.target_id)
                   WHEN 'category' THEN l.target_id
                   WHEN 'comment' THEN 'comment ' || l.target_id || coalesce(' on ' || (l.details->>'addon'), '')
                   ELSE l.target_type || ' ' || l.target_id END,
               'addon_slug', coalesce(va.slug, aa.slug, ra.slug), 'addon_name', coalesce(va.name, aa.name, ra.name),
               'tag', coalesce(v.tag, rv.tag), 'login', tu.login, 'report_reason', r.reason) AS target
    FROM audit_log l
    JOIN users u ON u.id = l.admin_id
    LEFT JOIN versions v ON l.target_type = 'version' AND v.id::text = l.target_id
    LEFT JOIN addons va ON va.id = v.addon_id
    LEFT JOIN addons aa ON l.target_type = 'addon' AND aa.id::text = l.target_id
    LEFT JOIN users tu ON l.target_type = 'user' AND tu.github_id::text = l.target_id
    LEFT JOIN reports r ON l.target_type = 'report' AND r.id::text = l.target_id
    LEFT JOIN versions rv ON r.target_type = 'version' AND rv.id = r.target_id
    LEFT JOIN images ri ON r.target_type = 'image' AND ri.id = r.target_id
    LEFT JOIN addon_comments rc ON r.target_type = 'comment' AND rc.id = r.target_id
    LEFT JOIN addons ra ON ra.id = CASE r.target_type WHEN 'addon' THEN r.target_id WHEN 'version' THEN rv.addon_id WHEN 'image' THEN ri.addon_id
                                                      WHEN 'comment' THEN rc.addon_id END
    LEFT JOIN game_versions g ON l.target_type = 'game_version' AND g.id::text = l.target_id)";

/// One row of `resolved`, as `h`, as JSON.
const AUDIT_ENTRY: &str = "json_build_object(
    'id', h.id, 'admin', h.admin_login, 'action', h.action, 'target_type', h.target_type, 'target_id', h.target_id,
    'target', h.target, 'note', h.note, 'details', h.details, 'created_at', h.created_at)";

/// A platform user by GitHub login, for the ban and owner forms.
pub async fn user(State(state): State<AppState>, StaffUser(_): StaffUser, Path(login): Path<String>) -> ApiResult<Json<Value>> {
    let user: Option<Value> = sqlx::query_scalar(
        "SELECT json_build_object('github_id', u.github_id, 'login', u.login, 'role', u.role, 'banned', u.banned_at IS NOT NULL,
                                  'ban_reason', u.ban_reason, 'addons', (SELECT count(*) FROM addons WHERE owner_id = u.id AND deleted_at IS NULL))
         FROM users u WHERE u.login = $1::citext",
    )
    .bind(login.trim())
    .fetch_optional(&state.pool)
    .await?;
    user.map(Json).ok_or_else(ApiError::not_found)
}

#[derive(Deserialize)]
pub struct CategoryBody {
    name: Option<String>,
    sort_key: Option<i32>,
    /// The top-level category this one sits under; on an update `""` moves it to the top.
    parent: Option<String>,
    #[serde(default)]
    note: String,
}

fn valid_category(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 32 || name.chars().any(char::is_control) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name", "A category name is 1 to 32 characters"));
    }
    Ok(name)
}

/// `parent` may only be a top-level category without addons of its own: one level of
/// subcategories, and addons only in categories without subcategories.
async fn check_parent(state: &AppState, parent: &str, child: Option<&str>) -> ApiResult<()> {
    let unfit = |message: &str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_parent", message.to_string());
    let top: Option<bool> = sqlx::query_scalar("SELECT parent IS NULL FROM categories WHERE name = $1::citext").bind(parent).fetch_optional(&state.pool).await?;
    match top {
        None => return Err(unfit("Unknown parent category")),
        Some(false) => return Err(unfit("Subcategories cannot have subcategories")),
        Some(true) => {}
    }
    if child.is_some_and(|c| c.eq_ignore_ascii_case(parent)) {
        return Err(unfit("A category cannot sit under itself"));
    }
    if let Some(child) = child {
        let has_children: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM categories WHERE parent = $1::citext)").bind(child).fetch_one(&state.pool).await?;
        if has_children {
            return Err(unfit("A category with subcategories stays at the top"));
        }
    }
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addons WHERE category = $1::citext)").bind(parent).fetch_one(&state.pool).await?;
    if used {
        return Err(ApiError::new(StatusCode::CONFLICT, "category_in_use", "Addons use the parent category; move them first"));
    }
    Ok(())
}

pub async fn create_category(State(state): State<AppState>, StaffUser(admin): StaffUser, Json(body): Json<CategoryBody>) -> ApiResult<StatusCode> {
    let name = valid_category(body.name.as_deref().unwrap_or_default())?;
    let parent = body.parent.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if let Some(parent) = parent {
        check_parent(&state, parent, None).await?;
    }
    let mut tx = state.pool.begin().await?;
    let inserted = sqlx::query("INSERT INTO categories (name, sort_key, parent) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(name)
        .bind(body.sort_key.unwrap_or(100))
        .bind(parent)
        .execute(&mut *tx)
        .await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, "name_taken", "This category exists already"));
    }
    let entry = Entry { admin_id: admin.id, action: "category_create", target_type: "category", target_id: name.into(), note: &body.note, details: json!({ "parent": parent }) };
    audit::record(&mut tx, entry, (state.clock)()).await?;
    tx.commit().await?;
    Ok(StatusCode::CREATED)
}

/// Renames or reorders; addons follow a rename through the foreign key.
pub async fn update_category(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(name): Path<String>,
    Json(body): Json<CategoryBody>,
) -> ApiResult<StatusCode> {
    let new_name = match &body.name {
        Some(n) => Some(valid_category(n)?.to_string()),
        None => None,
    };
    let parent = body.parent.as_deref().map(str::trim);
    if let Some(parent) = parent.filter(|p| !p.is_empty()) {
        check_parent(&state, parent, Some(&name)).await?;
    }
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE categories SET name = coalesce($2, name), sort_key = coalesce($3, sort_key),
                parent = CASE WHEN $4::text IS NULL THEN parent ELSE nullif($4, '')::citext END
         WHERE name = $1::citext",
    )
        .bind(&name)
        .bind(&new_name)
        .bind(body.sort_key)
        .bind(parent)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::new(StatusCode::CONFLICT, "name_taken", "This category exists already"),
            _ => e.into(),
        })?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    let details = json!({ "name": new_name, "sort_key": body.sort_key, "parent": parent });
    let entry = Entry { admin_id: admin.id, action: "category_update", target_type: "category", target_id: name, note: &body.note, details };
    audit::record(&mut tx, entry, (state.clock)()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_category(State(state): State<AppState>, StaffUser(admin): StaffUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addons WHERE category = $1::citext)").bind(&name).fetch_one(&state.pool).await?;
    if used {
        return Err(ApiError::new(StatusCode::CONFLICT, "category_in_use", "Addons still use this category"));
    }
    let has_children: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM categories WHERE parent = $1::citext)").bind(&name).fetch_one(&state.pool).await?;
    if has_children {
        return Err(ApiError::new(StatusCode::CONFLICT, "category_has_subcategories", "Delete or move its subcategories first"));
    }
    let mut tx = state.pool.begin().await?;
    let removed = sqlx::query("DELETE FROM categories WHERE name = $1::citext").bind(&name).execute(&mut *tx).await?;
    if removed.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    let entry = Entry { admin_id: admin.id, action: "category_delete", target_type: "category", target_id: name, note: "", details: json!({}) };
    audit::record(&mut tx, entry, (state.clock)()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
