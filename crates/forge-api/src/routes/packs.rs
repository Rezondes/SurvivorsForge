//! `/v1/packs`: addon packs, one zip with several addons for players of the browser version.
//! Any logged-in user creates them and changes their own; anyone with the link sees and downloads
//! them. The worker builds the zip on the first download (`crate::pack_job`).

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::jobs;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::Duration;
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Trust levels a version needs to go into a pack; it also needs a known SHA-256.
pub const PACKABLE: &[&str] = &["scanned", "verified"];
const MAX_NAME: usize = 80;

fn invalid(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

/// The key of a set of versions: packs with the same versions share one zip.
pub fn content_key(version_ids: &[i64]) -> String {
    let mut ids = version_ids.to_vec();
    ids.sort_unstable();
    let joined: Vec<String> = ids.iter().map(i64::to_string).collect();
    hex::encode(Sha256::digest(joined.join(",").as_bytes()))
}

fn public_id() -> String {
    rand::rng().sample_iter(rand::distr::Alphanumeric).take(10).map(char::from).collect()
}

#[derive(Deserialize)]
pub struct NewPack {
    #[serde(default)]
    name: String,
    /// Shown on the public pack list; otherwise only the link leads to it.
    #[serde(default)]
    listed: bool,
    addons: Vec<PackAddon>,
}

#[derive(Deserialize)]
struct PackAddon {
    slug: String,
    /// Without a tag the addon's newest checked version goes in.
    tag: Option<String>,
    /// Follows the newest checked version from now on; ignores `tag`.
    #[serde(default)]
    latest: bool,
}

/// The newest version of addon `a.id` that can go into a pack, with `packable` the parameter that
/// binds `PACKABLE`: releases before pre-releases, none removed on GitHub.
fn newest_packable(packable: &str) -> String {
    format!(
        "(SELECT n.id FROM versions n JOIN version_trust nt ON nt.version_id = n.id
          WHERE n.addon_id = a.id AND n.sha256 IS NOT NULL AND n.deleted_on_github_at IS NULL AND nt.trust = ANY({packable})
          ORDER BY n.prerelease, n.published_at DESC, n.id DESC LIMIT 1)"
    )
}

#[derive(sqlx::FromRow)]
struct Chosen {
    version_id: i64,
    name: String,
    tag: String,
    trust: String,
    sha256: Option<String>,
    size: Option<i64>,
    #[sqlx(skip)]
    latest: bool,
}

fn check_name(name: &str) -> ApiResult<()> {
    if name.chars().count() > MAX_NAME {
        return Err(invalid("invalid_name", "At most 80 characters"));
    }
    Ok(())
}

/// The versions `addons` ask for, each checked and the whole within the pack limits.
async fn choose(state: &AppState, addons: &[PackAddon]) -> ApiResult<Vec<Chosen>> {
    if addons.is_empty() {
        return Err(invalid("empty_pack", "Pick at least one addon"));
    }
    let max = state.config.pack_max_addons;
    if addons.len() as i64 > max {
        return Err(invalid("too_many_addons", format!("A pack holds at most {max} addons")));
    }
    let mut chosen: Vec<Chosen> = Vec::new();
    for addon in addons {
        let tag = addon.tag.as_deref().filter(|_| !addon.latest);
        let row: Option<Chosen> = sqlx::query_as(&format!(
            "SELECT v.id AS version_id, a.name, v.tag, t.trust, v.sha256, v.asset_size AS size
             FROM addons a
             JOIN versions v ON v.addon_id = a.id AND (CASE WHEN $2::text IS NULL THEN v.id = {} ELSE v.tag = $2 END)
             JOIN version_trust t ON t.version_id = v.id
             WHERE a.slug = $1::citext AND a.deleted_at IS NULL",
            newest_packable("$3")
        ))
        .bind(&addon.slug)
        .bind(tag)
        .bind(PACKABLE)
        .fetch_optional(&state.pool)
        .await?;
        let Some(mut row) = row else {
            let has_versions: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM addons a JOIN versions v ON v.addon_id = a.id WHERE a.slug = $1::citext AND a.deleted_at IS NULL)",
            )
                .bind(&addon.slug)
                .fetch_one(&state.pool)
                .await?;
            return Err(match (has_versions, tag) {
                (true, None) => invalid("not_packable", format!("{} has no checked version, so it cannot go into a pack", addon.slug)),
                _ => invalid("unknown_addon", format!("No addon or version {}", addon.slug)),
            });
        };
        if !PACKABLE.contains(&row.trust.as_str()) || row.sha256.is_none() {
            return Err(invalid("not_packable", format!("{} {} is not checked or was flagged, so it cannot go into a pack", row.name, row.tag)));
        }
        if chosen.iter().any(|c| c.version_id == row.version_id || c.name == row.name) {
            return Err(invalid("duplicate_addon", format!("{} is in the list twice", row.name)));
        }
        row.latest = addon.latest;
        chosen.push(row);
    }
    // Release sizes are known before the build; a source zip's size only after it.
    let known: i64 = chosen.iter().filter_map(|c| c.size).sum();
    if known as u64 > state.config.pack_max_bytes {
        return Err(invalid("too_large", format!("The pack would be larger than {} MB", state.config.pack_max_bytes / 1_000_000)));
    }
    Ok(chosen)
}

/// Replaces the versions of `pack` with `chosen` and points it at their content key.
async fn store_versions(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, pack: i64, chosen: &[Chosen]) -> sqlx::Result<()> {
    let ids: Vec<i64> = chosen.iter().map(|c| c.version_id).collect();
    let latest: Vec<bool> = chosen.iter().map(|c| c.latest).collect();
    sqlx::query("DELETE FROM pack_versions WHERE pack_id = $1").bind(pack).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO pack_versions (pack_id, version_id, latest) SELECT $1, unnest($2::bigint[]), unnest($3::bool[])")
        .bind(pack)
        .bind(&ids)
        .bind(&latest)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE packs SET content_key = $2 WHERE id = $1").bind(pack).bind(content_key(&ids)).execute(&mut **tx).await?;
    Ok(())
}

pub async fn create(State(state): State<AppState>, user: CurrentUser, Json(body): Json<NewPack>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = body.name.trim();
    check_name(name)?;
    let now = (state.clock)();
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM packs WHERE created_by = $1 AND created_at > $2")
        .bind(user.id)
        .bind(now - Duration::hours(1))
        .fetch_one(&state.pool)
        .await?;
    if recent >= state.config.packs_per_hour {
        let message = format!("At most {} new packs an hour, try again later", state.config.packs_per_hour);
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "pack_limit", message));
    }

    let chosen = choose(&state, &body.addons).await?;
    let ids: Vec<i64> = chosen.iter().map(|c| c.version_id).collect();
    let id = public_id();
    let mut tx = state.pool.begin().await?;
    let pack: i64 = sqlx::query_scalar(
        "INSERT INTO packs (public_id, name, created_by, content_key, created_at, listed) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(&id)
    .bind(name)
    .bind(user.id)
    .bind(content_key(&ids))
    .bind(now)
    .bind(body.listed)
    .fetch_one(&mut *tx)
    .await?;
    store_versions(&mut tx, pack, &chosen).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

#[derive(Deserialize)]
pub struct PackChange {
    name: Option<String>,
    listed: Option<bool>,
    /// Replaces every addon of the pack.
    addons: Option<Vec<PackAddon>>,
}

/// `PATCH /v1/packs/{id}`: its creator or an admin renames the pack, lists or hides it, or changes
/// its addons and their versions. The link stays the same.
pub async fn update(State(state): State<AppState>, user: CurrentUser, Path(public_id): Path<String>, Json(body): Json<PackChange>) -> ApiResult<Json<Value>> {
    let pack = load(&state, &public_id).await?;
    if pack.created_by != Some(user.id) && !user.is_admin() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "Only the creator of the pack changes it"));
    }
    let name = body.name.as_deref().map(str::trim);
    if let Some(name) = name {
        check_name(name)?;
    }
    let chosen = match &body.addons {
        Some(addons) => Some(choose(&state, addons).await?),
        None => None,
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE packs SET name = coalesce($2, name), listed = coalesce($3, listed) WHERE id = $1")
        .bind(pack.id)
        .bind(name)
        .bind(body.listed)
        .execute(&mut *tx)
        .await?;
    if let Some(chosen) = &chosen {
        store_versions(&mut tx, pack.id, chosen).await?;
    }
    tx.commit().await?;
    show(State(state), Path(public_id)).await
}

#[derive(sqlx::FromRow)]
struct PackRow {
    id: i64,
    created_by: Option<i64>,
    name: String,
    listed: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    creator: Option<String>,
    content_key: String,
}

/// The pack, with the addons that follow the newest version moved up to it.
async fn load(state: &AppState, public_id: &str) -> ApiResult<PackRow> {
    let pack = load_row(state, public_id).await?;
    let moved = sqlx::query(&format!(
        "UPDATE pack_versions pv SET version_id = n.newest
         FROM (SELECT pv.version_id AS old, {} AS newest
               FROM pack_versions pv JOIN versions v ON v.id = pv.version_id JOIN addons a ON a.id = v.addon_id
               WHERE pv.pack_id = $1 AND pv.latest) n
         WHERE pv.pack_id = $1 AND pv.version_id = n.old AND n.newest IS NOT NULL AND n.newest <> n.old",
        newest_packable("$2")
    ))
    .bind(pack.id)
    .bind(PACKABLE)
    .execute(&state.pool)
    .await?;
    if moved.rows_affected() == 0 {
        return Ok(pack);
    }
    let ids: Vec<i64> = sqlx::query_scalar("SELECT version_id FROM pack_versions WHERE pack_id = $1").bind(pack.id).fetch_all(&state.pool).await?;
    sqlx::query("UPDATE packs SET content_key = $2 WHERE id = $1").bind(pack.id).bind(content_key(&ids)).execute(&state.pool).await?;
    load_row(state, public_id).await
}

async fn load_row(state: &AppState, public_id: &str) -> ApiResult<PackRow> {
    sqlx::query_as(
        "SELECT p.id, p.created_by, p.name, p.listed, p.created_at, u.login::text AS creator, p.content_key
         FROM packs p LEFT JOIN users u ON u.id = p.created_by WHERE p.public_id = $1",
    )
    .bind(public_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)
}

#[derive(sqlx::FromRow, serde::Serialize)]
struct PackEntry {
    slug: String,
    name: String,
    tag: String,
    trust: String,
    /// Still installable: not deleted, blocked, flagged or changed on GitHub since.
    available: bool,
    /// Follows the newest checked version.
    latest: bool,
    /// Tags of every version that can go into a pack, newest first: the choices when editing.
    versions: Vec<String>,
    folders: Vec<String>,
}

async fn entries(state: &AppState, pack: i64) -> sqlx::Result<Vec<PackEntry>> {
    sqlx::query_as(
        "SELECT a.slug::text AS slug, a.name, v.tag, t.trust,
                (a.deleted_at IS NULL AND v.sha256 IS NOT NULL AND t.trust = ANY($2)) AS available, pv.latest,
                ARRAY(SELECT o.tag FROM versions o JOIN version_trust ot ON ot.version_id = o.id
                      WHERE o.addon_id = a.id AND o.sha256 IS NOT NULL AND o.deleted_on_github_at IS NULL AND ot.trust = ANY($2)
                      ORDER BY o.prerelease, o.published_at DESC, o.id DESC) AS versions,
                ARRAY(SELECT folder::text FROM addon_folders WHERE addon_id = a.id AND active ORDER BY folder) AS folders
         FROM pack_versions pv
         JOIN versions v ON v.id = pv.version_id
         JOIN addons a ON a.id = v.addon_id
         JOIN version_trust t ON t.version_id = v.id
         WHERE pv.pack_id = $1
         ORDER BY lower(a.name)",
    )
    .bind(pack)
    .bind(PACKABLE)
    .fetch_all(&state.pool)
    .await
}

#[derive(sqlx::FromRow)]
struct FileRow {
    status: String,
    size: Option<i64>,
    error: Option<String>,
    requested_at: chrono::DateTime<chrono::Utc>,
}

async fn file(state: &AppState, key: &str) -> sqlx::Result<Option<FileRow>> {
    sqlx::query_as("SELECT status, size, error, requested_at FROM pack_files WHERE content_key = $1").bind(key).fetch_optional(&state.pool).await
}

/// `GET /v1/packs/{id}`: what is in the pack and whether its zip is ready.
pub async fn show(State(state): State<AppState>, Path(public_id): Path<String>) -> ApiResult<Json<Value>> {
    let pack = load(&state, &public_id).await?;
    let addons = entries(&state, pack.id).await?;
    let file = file(&state, &pack.content_key).await?;
    Ok(Json(json!({
        "id": public_id,
        "name": pack.name,
        "listed": pack.listed,
        "created_at": pack.created_at,
        "created_by": pack.creator,
        "available": addons.iter().all(|a| a.available),
        "addons": addons,
        "file": {
            "status": file.as_ref().map_or("none", |f| f.status.as_str()),
            "size": file.as_ref().and_then(|f| f.size),
            "error": file.as_ref().and_then(|f| f.error.clone()),
        },
    })))
}

const PER_PAGE: i64 = 20;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    page: Option<i64>,
    /// Part of the pack's name or of the name of an addon in it.
    #[serde(default)]
    q: Option<String>,
}

/// `GET /v1/packs?page=`: the listed packs that can be downloaded, newest first.
pub async fn list(State(state): State<AppState>, axum::extract::Query(query): axum::extract::Query<ListQuery>) -> ApiResult<Json<Value>> {
    let page = query.page.unwrap_or(1).max(1);
    // A LIKE pattern of the search text; its own % and _ count literally.
    let pattern = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
    });
    let list: Value = sqlx::query_scalar(
        "WITH open AS (
             SELECT p.id, p.public_id, p.name, p.created_at, u.login::text AS creator
             FROM packs p LEFT JOIN users u ON u.id = p.created_by
             WHERE p.listed AND NOT EXISTS (
                 SELECT 1 FROM pack_versions pv
                 JOIN versions v ON v.id = pv.version_id JOIN addons a ON a.id = v.addon_id
                 JOIN version_trust t ON t.version_id = v.id
                 WHERE pv.pack_id = p.id AND NOT (a.deleted_at IS NULL AND v.sha256 IS NOT NULL AND t.trust = ANY($1)))
               AND ($4::text IS NULL OR p.name ILIKE $4 OR EXISTS (
                   SELECT 1 FROM pack_versions pv JOIN versions v ON v.id = pv.version_id JOIN addons a ON a.id = v.addon_id
                   WHERE pv.pack_id = p.id AND a.name ILIKE $4))
         )
         SELECT json_build_object(
             'total', (SELECT count(*) FROM open), 'page', $2::bigint, 'per_page', $3::bigint,
             'packs', coalesce((SELECT json_agg(item ORDER BY created_at DESC, id DESC) FROM (
                 SELECT o.id, o.created_at, json_build_object(
                     'id', o.public_id, 'name', o.name, 'created_at', o.created_at, 'created_by', o.creator,
                     'addons', (SELECT json_agg(a.name ORDER BY lower(a.name)) FROM pack_versions pv
                                JOIN versions v ON v.id = pv.version_id JOIN addons a ON a.id = v.addon_id WHERE pv.pack_id = o.id)) AS item
                 FROM open o ORDER BY o.created_at DESC, o.id DESC LIMIT $3 OFFSET ($2 - 1) * $3) page), '[]'))",
    )
    .bind(PACKABLE)
    .bind(page)
    .bind(PER_PAGE)
    .bind(pattern)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(list))
}

/// A failed build is tried again after this long, in case GitHub was only down.
const RETRY_FAILED_AFTER: Duration = Duration::hours(1);

/// `GET /v1/packs/{id}/download`: a redirect to the zip when it is built, else `202` while the
/// worker builds it (the client asks again).
pub async fn download(State(state): State<AppState>, Path(public_id): Path<String>) -> ApiResult<Response> {
    let pack = load(&state, &public_id).await?;
    if !entries(&state, pack.id).await?.iter().all(|a| a.available) {
        return Err(ApiError::new(StatusCode::CONFLICT, "pack_unavailable", "An addon of this pack is no longer available"));
    }
    let key = &pack.content_key;
    let now = (state.clock)();
    match file(&state, key).await? {
        Some(f) if f.status == "ready" => {
            sqlx::query("UPDATE pack_files SET last_download_at = $2 WHERE content_key = $1").bind(key).bind(now).execute(&state.pool).await?;
            return Ok(Redirect::to(&format!("{}/{key}.zip", state.config.public_pack_url)).into_response());
        }
        Some(f) if f.status == "building" => {}
        Some(f) if f.status == "failed" && now - f.requested_at < RETRY_FAILED_AFTER => {
            let message = f.error.unwrap_or_else(|| "The pack could not be built".into());
            return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "build_failed", message));
        }
        _ => {
            sqlx::query(
                "INSERT INTO pack_files (content_key, status, requested_at) VALUES ($1, 'building', $2)
                 ON CONFLICT (content_key) DO UPDATE SET status = 'building', error = NULL, requested_at = $2",
            )
            .bind(key)
            .bind(now)
            .execute(&state.pool)
            .await?;
            jobs::enqueue(&state.pool, crate::pack_job::KIND, json!({ "content_key": key }), now, Some(&format!("pack:{key}"))).await?;
        }
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "building" }))).into_response())
}
