//! `/v1/addons/{slug}/versions`: the version history, newest first, including versions gone from GitHub.

use super::addons::load_addon;
use super::game_versions::game_version_id;
use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct GameQuery {
    pub game: Option<String>,
}

/// The id of `?game=`, if given.
pub async fn game_of(state: &AppState, query: &GameQuery) -> ApiResult<Option<i32>> {
    match &query.game {
        Some(name) => Ok(Some(game_version_id(&state.pool, name).await?)),
        None => Ok(None),
    }
}

/// `POST /v1/addons/{slug}/downloads`: the website counts a download it sends to GitHub.
pub async fn count_download(
    State(state): State<AppState>,
    _: crate::rate_limit::FromWebsite,
    crate::rate_limit::VisitorIp(ip): crate::rate_limit::VisitorIp,
    Path(slug): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let addon = load_addon(&state.pool, &slug).await?;
    crate::downloads::count(&state, crate::downloads::Source::Website, addon.id, ip).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list(State(state): State<AppState>, Path(slug): Path<String>, Query(query): Query<GameQuery>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    let game = game_of(&state, &query).await?;
    let versions: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object(
                    'id', v.id, 'tag', tag, 'name', name, 'prerelease', prerelease, 'published_at', published_at,
                    'asset_name', asset_name, 'asset_url', asset_url, 'size', asset_size, 'sha256', sha256,
                    'downloads', download_count, 'scan_status', scan_status, 'source', source_status,
                    'deleted_on_github', deleted_on_github_at IS NOT NULL,
                    'trust', t.trust, 'findings', json_build_object('critical', t.critical, 'warn', t.warn, 'info', t.info),
                    'game_min', (SELECT name FROM game_versions WHERE id = v.game_min_id),
                    'game_max', (SELECT name FROM game_versions WHERE id = v.game_max_id),
                    'compatible', CASE WHEN $2::int IS NULL THEN NULL ELSE game_compatible(v.id, $2) END)
                ORDER BY published_at DESC, v.id DESC), '[]')
         FROM versions v JOIN version_trust t ON t.version_id = v.id WHERE v.addon_id = $1",
    )
    .bind(addon.id)
    .bind(game)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(newest_first(versions)))
}

/// By date, and by version number where dates are the same: tags have no date of their own, so all
/// tags of one sync share it, and the row order follows GitHub's listing, not the version.
fn newest_first(mut versions: Value) -> Value {
    if let Some(list) = versions.as_array_mut() {
        let date = |v: &Value| v["published_at"].as_str().unwrap_or_default().to_string();
        let key = |v: &Value| forge_core::github_web::version_key(v["tag"].as_str().unwrap_or_default());
        list.sort_by(|a, b| date(b).cmp(&date(a)).then_with(|| key(b).cmp(&key(a))));
    }
    versions
}

/// The latest scan of a version with all its findings.
pub async fn findings(State(state): State<AppState>, Path((slug, tag)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let addon = load_addon(&state.pool, &slug).await?;
    let report: Option<Value> = sqlx::query_scalar(
        "SELECT json_build_object(
             'tag', v.tag, 'scan_status', v.scan_status,
             'scan', (SELECT json_build_object(
                          'result', s.result, 'scanner_version', s.scanner_version, 'sha256', s.sha256,
                          'finished_at', s.finished_at,
                          'findings', (SELECT coalesce(json_agg(json_build_object(
                                              'rule', f.rule, 'severity', f.severity, 'file', f.file, 'line', f.line,
                                              'message', f.message, 'snippet', f.snippet)
                                          ORDER BY CASE f.severity WHEN 'critical' THEN 0 WHEN 'warn' THEN 1 WHEN 'notice' THEN 2 ELSE 3 END, f.id), '[]')
                                       FROM findings f WHERE f.scan_id = s.id))
                      FROM scans s WHERE s.version_id = v.id ORDER BY s.id DESC LIMIT 1))
         FROM versions v WHERE v.addon_id = $1 AND v.tag = $2",
    )
    .bind(addon.id)
    .bind(&tag)
    .fetch_optional(&state.pool)
    .await?;
    report.map(Json).ok_or_else(ApiError::not_found)
}
