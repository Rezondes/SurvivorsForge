//! `POST /v1/telemetry/events`: anonymous install and update events of the launcher.

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use crate::rate_limit::VisitorIp;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Most events in one request.
pub const MAX_EVENTS: usize = 50;
/// Days an event keeps its install ID.
pub const KEEP_INSTALL_ID_DAYS: i64 = 90;
/// The daily job that forgets old install IDs and dedupe rows.
pub const PRUNE: &str = "prune_telemetry";
/// The header with the launcher's random installation ID, sent only when the user allows it.
pub const INSTALL_HEADER: &str = "x-survivorsforge-install";

#[derive(Deserialize)]
pub struct Batch {
    events: Vec<Event>,
}

#[derive(Deserialize)]
pub struct Event {
    kind: String,
    addon: String,
    version: String,
    launcher_version: String,
    os: String,
    #[serde(default)]
    error_code: Option<String>,
}

fn failed(kind: &str) -> Option<bool> {
    match kind {
        "install" | "update" => Some(false),
        "install_failed" | "update_failed" => Some(true),
        _ => None,
    }
}

fn short(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_whitespace)
}

/// A fixed code like `download_failed`: no free text, so no paths or names can leak in.
fn error_code_ok(code: &str) -> bool {
    !code.is_empty() && code.len() <= 40 && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Event {
    fn valid(&self) -> bool {
        let code_fits = match (failed(&self.kind), &self.error_code) {
            (Some(true), Some(code)) => error_code_ok(code),
            (Some(false), None) => true,
            _ => false,
        };
        code_fits && short(&self.version, 64) && short(&self.launcher_version, 32) && matches!(self.os.as_str(), "windows" | "linux")
    }
}

/// A canonical UUID (`8-4-4-4-12` hex digits), lowercased; anything else counts as no ID.
pub fn install_id(headers: &HeaderMap) -> Option<String> {
    let id = headers.get(INSTALL_HEADER)?.to_str().ok()?.trim().to_ascii_lowercase();
    let groups: Vec<&str> = id.split('-').collect();
    let shape = groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12]);
    (shape && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))).then_some(id)
}

pub async fn events(State(state): State<AppState>, VisitorIp(ip): VisitorIp, headers: HeaderMap, Json(batch): Json<Batch>) -> ApiResult<(StatusCode, Json<Value>)> {
    if batch.events.len() > MAX_EVENTS {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "too_many_events", "At most 50 events per request"));
    }
    let now = (state.clock)();
    let day = now.date_naive();
    let install = install_id(&headers);
    let sender = match &install {
        Some(id) => id.as_bytes().to_vec(),
        None => state.sender_key.hash(day, ip),
    };
    let slugs: Vec<&str> = batch.events.iter().map(|e| e.addon.as_str()).collect();
    let addons: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>("SELECT slug::text, id FROM addons WHERE slug = ANY($1::text[]) AND deleted_at IS NULL")
        .bind(&slugs)
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect();
    let (mut accepted, mut rejected) = (0, 0);
    let mut tx = state.pool.begin().await?;
    for event in &batch.events {
        let Some(addon_id) = addons.get(&event.addon).filter(|_| event.valid()) else {
            rejected += 1;
            continue;
        };
        accepted += 1;
        let first: Option<i32> = sqlx::query_scalar(
            "INSERT INTO telemetry_seen (day, sender, addon_id, version, kind) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING RETURNING 1",
        )
        .bind(day)
        .bind(&sender)
        .bind(addon_id)
        .bind(&event.version)
        .bind(&event.kind)
        .fetch_optional(&mut *tx)
        .await?;
        if first.is_none() {
            continue;
        }
        sqlx::query(
            "INSERT INTO telemetry_events (day, kind, addon_id, version, launcher_version, os, error_code, install_id, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::uuid, $9)",
        )
        .bind(day)
        .bind(&event.kind)
        .bind(addon_id)
        .bind(&event.version)
        .bind(&event.launcher_version)
        .bind(&event.os)
        .bind(&event.error_code)
        .bind(&install)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    metrics::counter!("forge_telemetry_events_total").increment(accepted);
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": accepted, "rejected": rejected }))))
}

/// Forgets install IDs after `KEEP_INSTALL_ID_DAYS` and dedupe rows after two days.
pub async fn prune(pool: &sqlx::PgPool, now: DateTime<Utc>) -> Result<(), String> {
    let run = async {
        sqlx::query("UPDATE telemetry_events SET install_id = NULL WHERE install_id IS NOT NULL AND created_at < $1")
            .bind(now - TimeDelta::days(KEEP_INSTALL_ID_DAYS))
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM telemetry_seen WHERE day < $1").bind((now - TimeDelta::days(2)).date_naive()).execute(pool).await?;
        Ok::<_, sqlx::Error>(())
    };
    run.await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_ids_must_be_uuids() {
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(INSTALL_HEADER, v.parse().unwrap());
            install_id(&h)
        };
        assert_eq!(with("3F2504E0-4F89-41D3-9A0C-0305E82C3301"), Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301".into()));
        assert_eq!(with("not-a-uuid"), None);
        assert_eq!(with("3f2504e0-4f89-41d3-9a0c-0305e82c330g"), None);
        assert_eq!(install_id(&HeaderMap::new()), None);
    }

    #[test]
    fn error_codes_are_fixed_words() {
        assert!(error_code_ok("download_failed"));
        assert!(!error_code_ok("C:\\Users\\me\\x"));
        assert!(!error_code_ok("Download failed"));
        assert!(!error_code_ok(""));
    }
}
