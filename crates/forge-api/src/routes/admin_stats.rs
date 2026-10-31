//! `GET /v1/admin/stats?range=24h|7d|30d`: the detailed numbers of `/admin/stats` for everyone
//! whose role is in `ADMIN_STATS_ROLES`.

use crate::app::AppState;
use crate::auth::StatsViewer;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use chrono::TimeDelta;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Range {
    #[serde(default)]
    range: Option<String>,
}

fn span(range: Option<&str>) -> ApiResult<TimeDelta> {
    match range.unwrap_or("24h") {
        "24h" => Ok(TimeDelta::hours(24)),
        "7d" => Ok(TimeDelta::days(7)),
        "30d" => Ok(TimeDelta::days(30)),
        _ => Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_range", "range is 24h, 7d or 30d")),
    }
}

pub async fn stats(State(state): State<AppState>, StatsViewer(_): StatsViewer, Query(q): Query<Range>) -> ApiResult<Json<Value>> {
    let now = (state.clock)();
    let since = now - span(q.range.as_deref())?;
    let pool = &state.pool;
    let requests: Value = sqlx::query_scalar(
        "WITH r AS (SELECT * FROM api_request_stats WHERE hour >= date_trunc('hour', $1::timestamptz))
         SELECT json_build_object(
             'total', (SELECT coalesce(sum(count), 0) FROM r),
             'rate_limited', (SELECT coalesce(sum(count), 0) FROM r WHERE status = 429),
             'per_hour', (SELECT coalesce(json_agg(json_build_object('hour', hour, 'count', n) ORDER BY hour), '[]')
                          FROM (SELECT hour, sum(count) AS n FROM r GROUP BY hour) h),
             'routes', (SELECT coalesce(json_agg(json_build_object('route', route, 'method', method, 'count', n, 'avg_ms', avg_ms, 'max_ms', max_ms) ORDER BY n DESC, route), '[]')
                        FROM (SELECT route, method, sum(count) AS n, (sum(total_ms) / greatest(sum(count), 1))::bigint AS avg_ms, max(max_ms) AS max_ms
                              FROM r GROUP BY route, method ORDER BY n DESC, route LIMIT 20) t),
             'statuses', (SELECT coalesce(json_agg(json_build_object('status', status, 'count', n) ORDER BY status), '[]')
                          FROM (SELECT status, sum(count) AS n FROM r GROUP BY status) s),
             'clients', (SELECT coalesce(json_agg(json_build_object('client', client, 'count', n) ORDER BY n DESC, client), '[]')
                         FROM (SELECT client, sum(count) AS n FROM r GROUP BY client) c))",
    )
    .bind(since)
    .fetch_one(pool)
    .await?;
    let jobs: Value = sqlx::query_scalar(
        "SELECT coalesce(json_agg(json_build_object('kind', kind, 'open', open, 'running', running, 'done', done, 'failed', failed, 'last_error', last_error) ORDER BY kind), '[]')
         FROM (SELECT kind,
                      count(*) FILTER (WHERE done_at IS NULL AND failed_at IS NULL AND (locked_until IS NULL OR locked_until < $2)) AS open,
                      count(*) FILTER (WHERE done_at IS NULL AND failed_at IS NULL AND locked_until >= $2) AS running,
                      count(*) FILTER (WHERE done_at >= $1) AS done,
                      count(*) FILTER (WHERE failed_at >= $1) AS failed,
                      (SELECT j.last_error FROM jobs j WHERE j.kind = g.kind AND j.last_error IS NOT NULL ORDER BY j.id DESC LIMIT 1) AS last_error
               FROM jobs g GROUP BY kind) k",
    )
    .bind(since)
    .bind(now)
    .fetch_one(pool)
    .await?;
    let scans: Value = sqlx::query_scalar(
        "SELECT json_build_object(
             'scans', (SELECT count(*) FROM scans WHERE finished_at >= $1),
             'flagged', (SELECT count(*) FROM scans WHERE finished_at >= $1 AND result = 'flagged'),
             'errors', (SELECT count(*) FROM scans WHERE finished_at >= $1 AND result = 'error'),
             'findings', (SELECT count(*) FROM findings f JOIN scans s ON s.id = f.scan_id WHERE s.finished_at >= $1),
             'blocked_versions', (SELECT count(*) FROM versions WHERE blocked_at >= $1))",
    )
    .bind(since)
    .fetch_one(pool)
    .await?;
    let telemetry: Value = sqlx::query_scalar(
        "WITH e AS (SELECT * FROM telemetry_events WHERE created_at >= $1)
         SELECT json_build_object(
             'kinds', (SELECT coalesce(json_agg(json_build_object('kind', kind, 'count', n) ORDER BY kind), '[]')
                       FROM (SELECT kind, count(*) AS n FROM e GROUP BY kind) k),
             'failing_addons', (SELECT coalesce(json_agg(json_build_object('slug', slug, 'failed', failed, 'total', total) ORDER BY failed DESC, slug), '[]')
                                FROM (SELECT a.slug::text AS slug, count(*) FILTER (WHERE e.error_code IS NOT NULL) AS failed, count(*) AS total
                                      FROM e JOIN addons a ON a.id = e.addon_id GROUP BY a.slug
                                      HAVING count(*) FILTER (WHERE e.error_code IS NOT NULL) > 0
                                      ORDER BY failed DESC, a.slug LIMIT 10) f),
             'error_codes', (SELECT coalesce(json_agg(json_build_object('code', error_code, 'count', n) ORDER BY n DESC, error_code), '[]')
                             FROM (SELECT error_code, count(*) AS n FROM e WHERE error_code IS NOT NULL GROUP BY error_code) c),
             'launcher_versions', (SELECT coalesce(json_agg(json_build_object('version', launcher_version, 'count', n) ORDER BY n DESC, launcher_version), '[]')
                                   FROM (SELECT launcher_version, count(*) AS n FROM e GROUP BY launcher_version) v))",
    )
    .bind(since)
    .fetch_one(pool)
    .await?;
    let github = match state.github.rate_limit().await {
        Ok(limit) => json!(limit),
        Err(e) => json!({ "error": e.to_string() }),
    };
    Ok(Json(json!({ "since": since, "requests": requests, "jobs": jobs, "scans": scans, "telemetry": telemetry, "github": github })))
}
