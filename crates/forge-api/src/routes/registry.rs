//! `GET /v1/registry`: every platform addon in the schema of `registry/verified.json`, plus the
//! platform fields the new launcher reads. Also the import and export of that file.

use super::addons::{CreateAddon, insert_addon, public_repo};
use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use forge_core::registry::{DependencyRef, FindingCounts, LatestVersion, Registry, SCHEMA, SourceSpec, VerifiedAddon};
use forge_core::trust::Trust;
use sha2::{Digest, Sha256};

#[derive(sqlx::FromRow)]
struct Row {
    slug: String,
    name: String,
    author: String,
    repo: String,
    summary: String,
    category: String,
    folders: Vec<String>,
    source_type: String,
    asset_pattern: Option<String>,
    verified_author: bool,
    tag: Option<String>,
    asset_url: Option<String>,
    sha256: Option<String>,
    size: Option<i64>,
    trust: Option<String>,
    critical: Option<i32>,
    warn: Option<i32>,
    game_min: Option<String>,
    game_max: Option<String>,
    dep_slugs: Vec<String>,
    dep_required: Vec<bool>,
}

fn parse_trust(s: &str) -> Option<Trust> {
    serde_json::from_value(serde_json::Value::String(s.into())).ok()
}

/// The registry of all addons that are neither deleted nor blocked; `only_verified` keeps the ones
/// whose author had a verified version, which is what `registry/verified.json` holds.
pub async fn build(state: &AppState, only_verified: bool) -> sqlx::Result<Registry> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.slug::text AS slug, a.name, coalesce(u.login::text, a.author_name, '') AS author, a.repo_full_name AS repo,
                a.summary, a.category::text AS category,
                ARRAY(SELECT folder::text FROM addon_folders WHERE addon_id = a.id AND active ORDER BY folder) AS folders,
                a.source_type, a.asset_pattern,
                EXISTS (SELECT 1 FROM verifications w JOIN versions wv ON wv.id = w.version_id WHERE wv.addon_id = a.id) AS verified_author,
                v.tag, v.asset_url, v.sha256, v.asset_size AS size, t.trust, t.critical, t.warn,
                (SELECT name FROM game_versions WHERE id = v.game_min_id) AS game_min,
                (SELECT name FROM game_versions WHERE id = v.game_max_id) AS game_max,
                ARRAY(SELECT b.slug::text FROM addon_dependencies d JOIN addons b ON b.id = d.depends_on_id
                      WHERE d.addon_id = a.id AND b.deleted_at IS NULL ORDER BY b.slug) AS dep_slugs,
                ARRAY(SELECT d.required FROM addon_dependencies d JOIN addons b ON b.id = d.depends_on_id
                      WHERE d.addon_id = a.id AND b.deleted_at IS NULL ORDER BY b.slug) AS dep_required
         FROM addons a
         LEFT JOIN users u ON u.id = a.owner_id
         LEFT JOIN versions v ON v.id = relevant_version(a.id)
         LEFT JOIN version_trust t ON t.version_id = v.id
         WHERE a.deleted_at IS NULL AND a.blocked_at IS NULL
         ORDER BY a.slug",
    )
    .fetch_all(&state.pool)
    .await?;
    let addons = rows
        .into_iter()
        .filter(|r| r.verified_author || !only_verified)
        .map(|r| {
            let trust = r.trust.as_deref().and_then(parse_trust);
            let latest = match (r.tag, r.asset_url, trust) {
                (Some(tag), Some(asset_url), Some(trust)) => Some(LatestVersion {
                    tag,
                    asset_url,
                    sha256: r.sha256,
                    size: r.size,
                    trust,
                    findings: FindingCounts { critical: r.critical.unwrap_or(0) as u32, warn: r.warn.unwrap_or(0) as u32 },
                    game_min: r.game_min,
                    game_max: r.game_max,
                }),
                _ => None,
            };
            let source = match r.source_type.as_str() {
                "tag" => SourceSpec::Tag,
                _ => SourceSpec::Release { asset: r.asset_pattern.unwrap_or_default() },
            };
            VerifiedAddon {
                id: r.slug,
                name: r.name,
                author: r.author,
                repo: r.repo,
                description: r.summary,
                category: r.category,
                folders: r.folders,
                source,
                trust,
                verified_author: Some(r.verified_author),
                latest,
                dependencies: r.dep_slugs.into_iter().zip(r.dep_required).map(|(id, required)| DependencyRef { id, required }).collect(),
            }
        })
        .collect();
    Ok(Registry { schema: SCHEMA, addons })
}

pub async fn registry(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let body = serde_json::to_string(&build(&state, false).await?).map_err(|_| ApiError::internal())?;
    let etag = format!("\"{}\"", &hex::encode(Sha256::digest(body.as_bytes()))[..32]);
    let etag_value = HeaderValue::from_str(&etag).map_err(|_| ApiError::internal())?;
    let cache = (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    if headers.get(header::IF_NONE_MATCH).is_some_and(|v| crate::cache::etag_matches(v, etag.as_bytes())) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag_value), cache]).into_response());
    }
    let json = (header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(([(header::ETAG, etag_value), cache, json], body).into_response())
}

/// What `import_verified` did with one entry.
#[derive(Debug, PartialEq)]
pub enum Imported {
    Created(String),
    Skipped(String, String),
}

/// Adds every entry of a `verified.json` that the platform does not have yet, without owner, like
/// an admin adding it. Running it again changes nothing.
pub async fn import_verified(state: &AppState, text: &str) -> Result<Vec<Imported>, String> {
    let registry = Registry::parse(text).map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    for entry in registry.addons {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addons WHERE slug = $1::citext AND deleted_at IS NULL)")
            .bind(&entry.id)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| e.to_string())?;
        if exists {
            results.push(Imported::Skipped(entry.id, "exists already".into()));
            continue;
        }
        let (source_type, asset_pattern) = match &entry.source {
            SourceSpec::Release { asset } => ("release", Some(asset.clone())),
            SourceSpec::Tag => ("tag", None),
            SourceSpec::Branch => {
                results.push(Imported::Skipped(entry.id, "branch sources are not supported".into()));
                continue;
            }
        };
        let body = CreateAddon {
            slug: entry.id.to_lowercase(),
            name: entry.name.clone(),
            repo: entry.repo.clone(),
            source_type: source_type.into(),
            asset_pattern,
            category: entry.category.clone(),
            summary: entry.description.clone(),
            description_md: String::new(),
            folders: entry.folders.clone(),
            author_name: Some(entry.author.clone()),
        };
        let outcome = async {
            let (repo, installation) = public_repo(state, &body.repo).await?;
            insert_addon(state, &body, None, &repo, installation).await
        }
        .await;
        match outcome {
            Ok(_) => results.push(Imported::Created(entry.id)),
            Err(e) => results.push(Imported::Skipped(entry.id, format!("{}: {}", e.code, e.message))),
        }
    }
    Ok(results)
}

/// The addons of verified authors as `registry/verified.json`, two-space indented like the file.
pub async fn export_verified(state: &AppState) -> sqlx::Result<String> {
    let registry = build(state, true).await?;
    Ok(serde_json::to_string_pretty(&registry).expect("registry serializes") + "\n")
}
