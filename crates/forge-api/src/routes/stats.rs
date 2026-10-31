//! `GET /v1/stats`: the public numbers of the platform for `/stats`, and when the release and
//! source checks ran last and run next.

use crate::app::AppState;
use crate::error::ApiResult;
use crate::worker::{CHECK_SOURCES_EVERY, POLL_RELEASES_EVERY};
use axum::Json;
use axum::extract::State;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;

/// When a periodic job last finished and when it runs next: the start of its next time slot
/// (`jobs::enqueue_periodic`), or now while one is waiting or running.
async fn check(pool: &PgPool, kind: &str, every: TimeDelta, now: DateTime<Utc>) -> sqlx::Result<Value> {
    let (last, waiting): (Option<DateTime<Utc>>, bool) = sqlx::query_as(
        "SELECT max(done_at), bool_or(done_at IS NULL AND failed_at IS NULL AND run_at <= $2) FROM jobs WHERE kind = $1",
    )
    .bind(kind)
    .bind(now)
    .fetch_one(pool)
    .await
    .map(|(last, waiting): (Option<DateTime<Utc>>, Option<bool>)| (last, waiting.unwrap_or(false)))?;
    let seconds = every.num_seconds().max(1);
    let next_slot = DateTime::from_timestamp((now.timestamp() / seconds + 1) * seconds, 0).unwrap_or(now);
    Ok(json!({ "last": last, "next": if waiting { now } else { next_slot }, "every_minutes": every.num_minutes() }))
}

pub async fn checks(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Value> {
    Ok(json!({
        "releases": check(pool, crate::sync::POLL_RELEASES, POLL_RELEASES_EVERY, now).await?,
        "sources": check(pool, crate::source_check::CHECK_SOURCES, CHECK_SOURCES_EVERY, now).await?,
    }))
}

pub async fn stats(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let now = (state.clock)();
    let since = (now - TimeDelta::days(30)).date_naive();
    let launcher = crate::launcher_stats::totals(&state.pool).await?;
    let current = launcher.current_tag.as_deref().map(|t| t.trim_start_matches('v').to_string());
    let telemetry: Value = sqlx::query_scalar(
        "WITH latest AS (
             SELECT DISTINCT ON (install_id) install_id, launcher_version FROM telemetry_events
             WHERE install_id IS NOT NULL AND day >= $1 ORDER BY install_id, created_at DESC
         )
         SELECT json_build_object(
             'installs', count(*) FILTER (WHERE kind = 'install'),
             'updates', count(*) FILTER (WHERE kind = 'update'),
             'installs_30d', count(*) FILTER (WHERE kind = 'install' AND day >= $1),
             'updates_30d', count(*) FILTER (WHERE kind = 'update' AND day >= $1),
             'active_installations_30d', (SELECT count(*) FROM latest),
             'on_current_launcher_30d', (SELECT count(*) FROM latest WHERE launcher_version = $2))
         FROM telemetry_events",
    )
    .bind(since)
    .bind(&current)
    .fetch_one(&state.pool)
    .await?;
    let top: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object('slug', slug, 'name', name, 'installs', n) ORDER BY n DESC, slug), '[]')
         FROM (SELECT a.slug::text AS slug, a.name, count(*) AS n FROM telemetry_events e JOIN addons a ON a.id = e.addon_id
               WHERE e.kind = 'install' AND e.day >= $1 AND a.deleted_at IS NULL AND a.blocked_at IS NULL
               GROUP BY a.id ORDER BY n DESC, a.slug LIMIT 5) t",
    )
    .bind(since)
    .fetch_one(&state.pool)
    .await?;
    let platform: Value = sqlx::query_scalar(
        "WITH listed AS (SELECT id, owner_id FROM addons WHERE deleted_at IS NULL AND blocked_at IS NULL)
         SELECT json_build_object(
             'addons', (SELECT count(*) FROM listed),
             'verified_addons', (SELECT count(*) FROM listed l JOIN version_trust t ON t.version_id = relevant_version(l.id) WHERE t.trust = 'verified'),
             'creators', (SELECT count(DISTINCT owner_id) FROM listed),
             'game_versions', (SELECT count(*) FROM game_versions),
             'scanned_versions', (SELECT count(*) FROM versions WHERE scan_status = 'done'),
             'blocked_versions', (SELECT count(*) FROM versions WHERE blocked_at IS NOT NULL),
             'github_downloads', (SELECT coalesce(sum(download_count), 0) FROM versions),
             'website_downloads', (SELECT coalesce(sum(count), 0) FROM download_counts WHERE source = 'website'),
             'packs', (SELECT count(*) FROM packs),
             'public_packs', (SELECT count(*) FROM packs WHERE listed),
             'pack_downloads', (SELECT coalesce(sum(count), 0) FROM download_counts WHERE source = 'pack'))",
    )
    .fetch_one(&state.pool)
    .await?;
    let website: Value = sqlx::query_scalar(
        "SELECT json_build_object(
             'views', coalesce(sum(views), 0), 'daily_visitors', coalesce(sum(visitors), 0),
             'views_30d', coalesce(sum(views) FILTER (WHERE day >= $1), 0),
             'daily_visitors_30d', coalesce(sum(visitors) FILTER (WHERE day >= $1), 0))
         FROM web_counts WHERE kind = 'site'",
    )
    .bind(since)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({
        "website": website,
        "launcher": launcher,
        "addons": telemetry,
        "top_addons_30d": top,
        "platform": platform,
        "checks": checks(&state.pool, now).await?,
    })))
}
