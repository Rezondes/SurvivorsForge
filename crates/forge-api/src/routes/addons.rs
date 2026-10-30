//! `/v1/addons`: create, read, change and delete addons.

use crate::addons::{Permissions, permissions, validate_description, validate_folders, validate_name, validate_slug, validate_source, validate_summary};
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::github::GitHubError;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AddonRow {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub owner_id: Option<i64>,
    pub repo_id: i64,
    pub repo_full_name: String,
    /// `None` while nobody installed the GitHub App on the repo.
    pub installation_id: Option<i64>,
    pub source_type: String,
    pub asset_pattern: Option<String>,
    pub category: String,
    pub summary: String,
    pub description_md: String,
    pub blocked_at: Option<DateTime<Utc>>,
    pub block_reason: Option<String>,
}

const ADDON_COLUMNS: &str = "id, slug::text AS slug, name, owner_id, repo_id, repo_full_name, installation_id, source_type,
    asset_pattern, category::text AS category, summary, description_md, blocked_at, block_reason";

/// The addon behind `slug`, unless it is deleted.
pub async fn load_addon(pool: &PgPool, slug: &str) -> ApiResult<AddonRow> {
    sqlx::query_as(&format!("SELECT {ADDON_COLUMNS} FROM addons WHERE slug = $1 AND deleted_at IS NULL"))
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(ApiError::not_found)
}

fn forbidden(message: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", message)
}

fn unprocessable(code: &'static str, message: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

pub fn github_error(e: GitHubError) -> ApiError {
    tracing::warn!(error = %e, "GitHub request failed");
    ApiError::new(StatusCode::BAD_GATEWAY, "github_error", "GitHub could not be reached, try again later")
}

/// Unique violations of the addon tables as the conflicts they stand for.
fn map_unique(e: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(db) = &e {
        match db.constraint() {
            Some("addons_slug") => return conflict("slug_taken", "This slug is taken"),
            Some("addons_repo") => return conflict("repo_taken", "This repo already has an addon"),
            Some("addon_folders_owner") => return conflict("folder_taken", "A folder belongs to another addon"),
            _ => {}
        }
    }
    e.into()
}

/// Proves the repo is the user's: a repo of the account GitHub logged them in as is theirs (the
/// owner's id counts, not a name that may change hands); any other needs the App on the repo and
/// admin rights there. The installation, if there is one, keeps updates instant.
async fn verify_repo(state: &AppState, user: &CurrentUser, full_name: &str) -> ApiResult<(crate::github::types::Repo, Option<i64>)> {
    let (repo, installation) = public_repo(state, full_name).await?;
    if repo.owner.id == user.github_id {
        return Ok((repo, installation));
    }
    let installation = installation.ok_or_else(|| unprocessable("app_not_installed", "Install the SurvivorsForge GitHub App on this repo first"))?;
    let permission = state.github.user_permission(installation, repo.id, &user.login).await.map_err(github_error)?;
    if permission != "admin" {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not_repo_admin", "You need admin rights on this repo"));
    }
    Ok((repo, Some(installation)))
}

/// Addons go into a category without subcategories (Warrior, not Classes).
async fn check_category(pool: &PgPool, category: &str) -> ApiResult<()> {
    let has_children: Option<bool> = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM categories c WHERE c.parent = categories.name) FROM categories WHERE name = $1::citext",
    )
    .bind(category)
    .fetch_optional(pool)
    .await?;
    match has_children {
        None => Err(unprocessable("unknown_category", "Unknown category")),
        Some(true) => Err(unprocessable("category_has_subcategories", "Pick one of its subcategories")),
        Some(false) => Ok(()),
    }
}

/// Names the first folder that already belongs to another addon.
async fn check_folders(pool: &PgPool, folders: &[String], own_addon: Option<i64>) -> ApiResult<()> {
    let taken: Option<(String, String)> = sqlx::query_as(
        "SELECT f.folder::text, a.name FROM addon_folders f JOIN addons a ON a.id = f.addon_id
         WHERE f.active AND f.folder = ANY($1::citext[]) AND f.addon_id IS DISTINCT FROM $2 LIMIT 1",
    )
    .bind(folders)
    .bind(own_addon)
    .fetch_optional(pool)
    .await?;
    match taken {
        Some((folder, owner)) => Err(conflict("folder_taken", format!("The folder {folder} belongs to the addon {owner}"))),
        None => Ok(()),
    }
}

async fn write_folders(tx: &mut Transaction<'_, Postgres>, addon_id: i64, folders: &[String]) -> ApiResult<()> {
    sqlx::query("DELETE FROM addon_folders WHERE addon_id = $1").bind(addon_id).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO addon_folders (addon_id, folder) SELECT $1, unnest($2::citext[])")
        .bind(addon_id)
        .bind(folders)
        .execute(&mut **tx)
        .await
        .map_err(map_unique)?;
    Ok(())
}

/// The public JSON of an addon. `description_html` is the only form clients may render as HTML.
pub async fn addon_view(state: &AppState, id: i64) -> ApiResult<Value> {
    let mut view: Value = sqlx::query_scalar(
        "SELECT json_build_object(
             'slug', a.slug, 'name', a.name, 'summary', a.summary, 'description_md', a.description_md,
             'category', a.category, 'repo', a.repo_full_name, 'source_type', a.source_type,
             'asset_pattern', a.asset_pattern,
             'folders', (SELECT coalesce(json_agg(f.folder ORDER BY f.folder), '[]') FROM addon_folders f WHERE f.addon_id = a.id),
             'owner', CASE WHEN u.id IS NULL THEN NULL
                           ELSE json_build_object('github_id', u.github_id, 'login', u.login, 'avatar_url', u.avatar_url) END,
             'author', coalesce(u.login::text, a.author_name),
             'claimed', a.owner_id IS NOT NULL,
             'contributors', (SELECT coalesce(json_agg(json_build_object('login', mu.login, 'avatar_url', mu.avatar_url) ORDER BY mu.login), '[]')
                              FROM addon_members m JOIN users mu ON mu.id = m.user_id WHERE m.addon_id = a.id),
             'blocked', a.blocked_at IS NOT NULL,
             'trust', t.trust,
             'latest_tag', (SELECT tag FROM versions WHERE id = t.version_id),
             'findings', CASE WHEN t.version_id IS NULL THEN NULL
                              ELSE json_build_object('critical', t.critical, 'warn', t.warn, 'info', t.info) END,
             'dependencies', (SELECT coalesce(json_agg(json_build_object(
                                   'slug', b.slug, 'name', b.name, 'required', d.required,
                                   'trust', (SELECT trust FROM version_trust WHERE version_id = relevant_version(b.id)),
                                   'available', b.deleted_at IS NULL AND b.blocked_at IS NULL)
                               ORDER BY d.required DESC, b.slug), '[]')
                              FROM addon_dependencies d JOIN addons b ON b.id = d.depends_on_id WHERE d.addon_id = a.id),
             'images', (SELECT coalesce(json_agg(json_build_object(
                             'id', i.id, 'url', $2 || '/' || i.file, 'width', i.width, 'height', i.height, 'caption', i.caption,
                             'position', i.position) ORDER BY i.position, i.id), '[]')
                        FROM images i WHERE i.addon_id = a.id AND i.kind = 'screenshot'),
             'icon_url', (SELECT $2 || '/' || i.file FROM images i WHERE i.addon_id = a.id AND i.kind = 'icon'),
             'verified_author', EXISTS (SELECT 1 FROM verifications w JOIN versions wv ON wv.id = w.version_id WHERE wv.addon_id = a.id),
             'created_at', a.created_at, 'updated_at', a.updated_at)
         FROM addons a LEFT JOIN users u ON u.id = a.owner_id
         LEFT JOIN version_trust t ON t.version_id = relevant_version(a.id)
         WHERE a.id = $1",
    )
    .bind(id)
    .bind(&state.config.public_image_url)
    .fetch_one(&state.pool)
    .await?;
    let html = forge_core::markdown::render(view["description_md"].as_str().unwrap_or_default(), &state.config.public_image_url);
    view["description_html"] = Value::String(html);
    view["install_order"] = json!(super::dependencies::required_order(&state.pool, id).await?);
    Ok(view)
}

#[derive(Deserialize, Clone)]
pub struct CreateAddon {
    pub slug: String,
    pub name: String,
    pub repo: String,
    pub source_type: String,
    pub asset_pattern: Option<String>,
    pub category: String,
    #[serde(default)]
    pub summary: String,
    /// The long description, Markdown.
    #[serde(default)]
    pub description_md: String,
    pub folders: Vec<String>,
    /// Admin additions only: the author's name while nobody owns the addon.
    #[serde(default)]
    pub author_name: Option<String>,
}

/// Validates and stores a new addon, then queues its first sync. The repo checks are the caller's.
pub async fn insert_addon(
    state: &AppState,
    body: &CreateAddon,
    owner: Option<i64>,
    repo: &crate::github::types::Repo,
    installation: Option<i64>,
) -> ApiResult<i64> {
    validate_slug(&body.slug)?;
    validate_name(&body.name)?;
    validate_summary(&body.summary)?;
    validate_description(&body.description_md)?;
    let pattern = body.asset_pattern.clone().filter(|_| body.source_type == "release");
    validate_source(&body.source_type, pattern.as_deref())?;
    validate_folders(&body.folders)?;
    check_category(&state.pool, &body.category).await?;
    let taken: Option<String> = sqlx::query_scalar("SELECT slug::text FROM addons WHERE repo_id = $1 AND deleted_at IS NULL")
        .bind(repo.id)
        .fetch_optional(&state.pool)
        .await?;
    if taken.is_some() {
        return Err(conflict("repo_taken", "This repo already has an addon"));
    }
    check_folders(&state.pool, &body.folders, None).await?;

    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO addons (slug, name, owner_id, repo_id, repo_full_name, installation_id, source_type, asset_pattern,
                             category, summary, author_name, created_at, updated_at, description_md)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12, $13) RETURNING id",
    )
    .bind(&body.slug)
    .bind(body.name.trim())
    .bind(owner)
    .bind(repo.id)
    .bind(&repo.full_name)
    .bind(installation)
    .bind(&body.source_type)
    .bind(&pattern)
    .bind(&body.category)
    .bind(&body.summary)
    .bind(body.author_name.as_deref().map(str::trim).filter(|a| !a.is_empty() && owner.is_none()))
    .bind(now)
    .bind(&body.description_md)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    write_folders(&mut tx, id, &body.folders).await?;
    tx.commit().await?;
    crate::sync::enqueue_sync(state, id, now).await?;
    Ok(id)
}

pub async fn create(State(state): State<AppState>, user: CurrentUser, Json(body): Json<CreateAddon>) -> ApiResult<(StatusCode, Json<Value>)> {
    validate_slug(&body.slug)?;
    let (repo, installation) = verify_repo(&state, &user, &body.repo).await?;
    let id = insert_addon(&state, &body, Some(user.id), &repo, installation).await?;
    Ok((StatusCode::CREATED, Json(addon_view(&state, id).await?)))
}

/// The public repo `owner/name` for an addon an admin adds: through the App when it is installed
/// there, else without a token.
pub async fn public_repo(state: &AppState, full_name: &str) -> ApiResult<(crate::github::types::Repo, Option<i64>)> {
    let full_name = full_name.trim();
    let valid = full_name.split('/').count() == 2 && full_name.split('/').all(|p| !p.is_empty()) && full_name.len() <= 140;
    if !valid {
        return Err(unprocessable("invalid_repo", "The repo must be owner/name"));
    }
    let installation = state.github.installation_for_repo(full_name).await.map_err(github_error)?;
    let repo = match state.github.repo_by_name(installation, full_name).await {
        Ok(repo) => repo,
        Err(crate::github::GitHubError::NotFound) => return Err(unprocessable("repo_not_found", "There is no public repo with that name")),
        Err(e) => return Err(github_error(e)),
    };
    if repo.private {
        return Err(unprocessable("repo_private", "Only public repos can be published"));
    }
    Ok((repo, installation))
}

/// `POST /v1/admin/addons`: an admin adds an addon of someone else's repo without the GitHub App
/// checks, like an entry of the old verified list. Nobody owns it until its author claims it.
pub async fn admin_create(
    State(state): State<AppState>,
    crate::auth::StaffUser(admin): crate::auth::StaffUser,
    Json(body): Json<CreateAddon>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    validate_slug(&body.slug)?;
    let (repo, installation) = public_repo(&state, &body.repo).await?;
    let id = insert_addon(&state, &body, None, &repo, installation).await?;
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let entry = crate::audit::Entry {
        admin_id: admin.id,
        action: "admin_create_addon",
        target_type: "addon",
        target_id: id.to_string(),
        note: "",
        details: json!({ "slug": body.slug, "repo": repo.full_name }),
    };
    crate::audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(addon_view(&state, id).await?)))
}

pub async fn show(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<super::versions::GameQuery>,
) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    if addon.blocked_at.is_some() {
        // A blocked addon shows nothing but the fact and the reason.
        return Ok(Json(json!({ "slug": addon.slug, "name": addon.name, "blocked": true, "block_reason": addon.block_reason })));
    }
    let mut view = addon_view(&state, addon.id).await?;
    if let Some(game) = super::versions::game_of(&state, &query).await? {
        let compatible: Option<bool> = sqlx::query_scalar("SELECT game_compatible(relevant_version($1), $2)")
            .bind(addon.id)
            .bind(game)
            .fetch_one(&state.pool)
            .await?;
        view["compatible"] = json!(compatible);
    }
    Ok(Json(view))
}

#[derive(Deserialize, Default)]
pub struct PatchAddon {
    name: Option<String>,
    summary: Option<String>,
    description_md: Option<String>,
    category: Option<String>,
    repo: Option<String>,
    source_type: Option<String>,
    asset_pattern: Option<String>,
    folders: Option<Vec<String>>,
}

/// What `user` may do with `addon`.
pub async fn user_permissions(pool: &PgPool, addon: &AddonRow, user: &CurrentUser) -> ApiResult<Permissions> {
    let member: Option<(bool, bool)> =
        sqlx::query_as("SELECT can_edit_description, can_edit_images FROM addon_members WHERE addon_id = $1 AND user_id = $2")
            .bind(addon.id)
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    Ok(permissions(user.id, user.is_staff(), addon.owner_id, member))
}

/// Blocked addons stay as they are until an admin lifts the block.
pub fn not_blocked(addon: &AddonRow) -> ApiResult<()> {
    if addon.blocked_at.is_some() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "addon_blocked", "This addon is blocked"));
    }
    Ok(())
}

/// `403` unless `allowed`.
pub fn require(allowed: bool, message: &str) -> ApiResult<()> {
    if allowed { Ok(()) } else { Err(forbidden(message)) }
}

/// Owner fields: everything except summary and description.
fn touches_owner_fields(body: &PatchAddon) -> bool {
    body.name.is_some()
        || body.category.is_some()
        || body.repo.is_some()
        || body.source_type.is_some()
        || body.asset_pattern.is_some()
        || body.folders.is_some()
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(slug): Path<String>,
    Json(body): Json<PatchAddon>,
) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let can = user_permissions(&state.pool, &addon, &user).await?;
    require(can.owner || !touches_owner_fields(&body), "Only the owner can change this")?;
    require(can.edit_description || (body.summary.is_none() && body.description_md.is_none()), "You may not edit the description")?;

    if let Some(name) = &body.name {
        validate_name(name)?;
    }
    if let Some(summary) = &body.summary {
        validate_summary(summary)?;
    }
    if let Some(description) = &body.description_md {
        validate_description(description)?;
    }
    if let Some(category) = &body.category {
        check_category(&state.pool, category).await?;
    }
    let source_type = body.source_type.clone().unwrap_or(addon.source_type.clone());
    let pattern = if source_type == "release" { body.asset_pattern.clone().or(addon.asset_pattern.clone()) } else { None };
    validate_source(&source_type, pattern.as_deref())?;
    if let Some(folders) = &body.folders {
        validate_folders(folders)?;
        check_folders(&state.pool, folders, Some(addon.id)).await?;
    }
    let (repo_id, repo_full_name, installation) = match &body.repo {
        Some(repo) => {
            let (repo, installation) = verify_repo(&state, &user, repo).await?;
            (repo.id, repo.full_name, installation)
        }
        None => (addon.repo_id, addon.repo_full_name.clone(), addon.installation_id),
    };

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE addons SET name = $2, summary = $3, description_md = $4, category = $5, repo_id = $6, repo_full_name = $7,
                installation_id = $8, source_type = $9, asset_pattern = $10, updated_at = $11
         WHERE id = $1",
    )
    .bind(addon.id)
    .bind(body.name.as_deref().map(str::trim).unwrap_or(&addon.name))
    .bind(body.summary.as_deref().unwrap_or(&addon.summary))
    .bind(body.description_md.as_deref().unwrap_or(&addon.description_md))
    .bind(body.category.as_deref().unwrap_or(&addon.category))
    .bind(repo_id)
    .bind(&repo_full_name)
    .bind(installation)
    .bind(&source_type)
    .bind(&pattern)
    .bind((state.clock)())
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    if let Some(folders) = &body.folders {
        write_folders(&mut tx, addon.id, folders).await?;
    }
    tx.commit().await?;
    Ok(Json(addon_view(&state, addon.id).await?))
}

/// Soft delete: the addon disappears, its slug, repo and folders are free again.
pub async fn delete(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>) -> ApiResult<StatusCode> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    require(user_permissions(&state.pool, &addon, &user).await?.owner, "Only the owner can delete this addon")?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE addons SET deleted_at = $2 WHERE id = $1").bind(addon.id).bind((state.clock)()).execute(&mut *tx).await?;
    sqlx::query("UPDATE addon_folders SET active = false WHERE addon_id = $1").bind(addon.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/addons/{slug}/claim`: the author of an imported addon takes it over by proving admin
/// rights on its repo, the same check as for publishing.
pub async fn claim(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    if addon.owner_id.is_some() {
        return Err(conflict("already_claimed", "This addon has an owner"));
    }
    let (repo, installation) = verify_repo(&state, &user, &addon.repo_full_name).await?;
    if repo.id != addon.repo_id {
        return Err(conflict("repo_mismatch", "The repo behind this name is not the addon's repo"));
    }
    let claimed = sqlx::query(
        "UPDATE addons SET owner_id = $2, installation_id = $3, repo_full_name = $4, updated_at = $5 WHERE id = $1 AND owner_id IS NULL",
    )
    .bind(addon.id)
    .bind(user.id)
    .bind(installation)
    .bind(&repo.full_name)
    .bind((state.clock)())
    .execute(&state.pool)
    .await?;
    if claimed.rows_affected() == 0 {
        return Err(conflict("already_claimed", "This addon has an owner"));
    }
    Ok(Json(addon_view(&state, addon.id).await?))
}
