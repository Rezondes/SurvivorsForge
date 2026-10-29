//! Update checks by hand: an admin checks every addon, a creator forces one of theirs.

use super::addons::{load_addon, not_blocked, require, user_permissions};
use crate::app::AppState;
use crate::auth::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::{jobs, sync};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};

/// How long a user waits between two forced update checks.
pub const REFRESH_EVERY: Duration = Duration::minutes(5);

/// `POST /v1/admin/addons/sync`: queues the check of every addon now, in the background.
pub async fn sync_all(State(state): State<AppState>, AdminUser(_): AdminUser) -> ApiResult<StatusCode> {
    jobs::enqueue(&state.pool, sync::POLL_RELEASES, serde_json::json!({}), (state.clock)(), Some(sync::POLL_RELEASES)).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /v1/addons/{slug}/sync`: the owner or a contributor checks the addon for updates now.
/// Once per user every `REFRESH_EVERY`, across all their addons.
pub async fn sync_one(State(state): State<AppState>, user: CurrentUser, Path(slug): Path<String>) -> ApiResult<StatusCode> {
    let addon = load_addon(&state.pool, &slug).await?;
    not_blocked(&addon)?;
    let p = user_permissions(&state.pool, &addon, &user).await?;
    let member: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addon_members WHERE addon_id = $1 AND user_id = $2)")
        .bind(addon.id)
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    require(p.owner || p.admin || member, "Only the owner and contributors check an addon for updates")?;
    let now = (state.clock)();
    let last: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
        "UPDATE users SET refresh_requested_at = $2
         WHERE id = $1 AND (refresh_requested_at IS NULL OR refresh_requested_at <= $3)
         RETURNING refresh_requested_at",
    )
    .bind(user.id)
    .bind(now)
    .bind(now - REFRESH_EVERY)
    .fetch_optional(&state.pool)
    .await?;
    if last.is_none() {
        let since: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT refresh_requested_at FROM users WHERE id = $1").bind(user.id).fetch_one(&state.pool).await?;
        let wait = since.map(|t| t + REFRESH_EVERY - now).unwrap_or(REFRESH_EVERY);
        let minutes = (wait.num_seconds() + 59) / 60;
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "refresh_too_soon",
            format!("You can force the next update check in {minutes} minute{}", if minutes == 1 { "" } else { "s" }),
        ));
    }
    sync::enqueue_sync(&state, addon.id, now).await?;
    Ok(StatusCode::ACCEPTED)
}
