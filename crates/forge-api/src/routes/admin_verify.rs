//! `/v1/admin/versions/{id}/verification`: admins approve exactly one file of a version.

use crate::app::AppState;
use crate::audit::{self, Entry};
use crate::auth::StaffUser;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, Default)]
pub struct VerifyBody {
    #[serde(default)]
    note: String,
}

pub async fn verify(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(version_id): Path<i64>,
    Json(body): Json<VerifyBody>,
) -> ApiResult<Json<Value>> {
    let version: Option<(Option<String>, String, String)> = sqlx::query_as(
        "SELECT v.sha256, v.scan_status, t.trust FROM versions v JOIN version_trust t ON t.version_id = v.id WHERE v.id = $1",
    )
    .bind(version_id)
    .fetch_optional(&state.pool)
    .await?;
    let (sha256, scan_status, trust) = version.ok_or_else(ApiError::not_found)?;
    let Some(sha256) = sha256.filter(|_| scan_status == "done") else {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_scanned", "Only scanned versions can be verified"));
    };
    let note = body.note.trim();
    if trust == "flagged" && note.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "note_required", "Explain why a flagged version is fine"));
    }
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO verifications (version_id, sha256, admin_id, note, created_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (version_id) DO UPDATE SET sha256 = EXCLUDED.sha256, admin_id = EXCLUDED.admin_id,
             note = EXCLUDED.note, created_at = EXCLUDED.created_at",
    )
    .bind(version_id)
    .bind(&sha256)
    .bind(admin.id)
    .bind(note)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let entry = Entry {
        admin_id: admin.id,
        action: "verify",
        target_type: "version",
        target_id: version_id.to_string(),
        note,
        details: json!({ "sha256": sha256, "trust_before": trust }),
    };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(Json(json!({ "version_id": version_id, "sha256": sha256, "trust": "verified" })))
}

pub async fn unverify(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(version_id): Path<i64>,
    body: Option<Json<VerifyBody>>,
) -> ApiResult<StatusCode> {
    let note = body.map(|b| b.0.note).unwrap_or_default();
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    let removed: Option<String> = sqlx::query_scalar("DELETE FROM verifications WHERE version_id = $1 RETURNING sha256")
        .bind(version_id)
        .fetch_optional(&mut *tx)
        .await?;
    let sha256 = removed.ok_or_else(ApiError::not_found)?;
    let entry = Entry {
        admin_id: admin.id,
        action: "unverify",
        target_type: "version",
        target_id: version_id.to_string(),
        note: note.trim(),
        details: json!({ "sha256": sha256 }),
    };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Takes a version off the review queue without verifying it; it keeps its trust.
pub async fn skip(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(version_id): Path<i64>,
    body: Option<Json<VerifyBody>>,
) -> ApiResult<StatusCode> {
    let note = body.map(|b| b.0.note).unwrap_or_default();
    let note = note.trim();
    let trust: Option<String> = sqlx::query_scalar("SELECT trust FROM version_trust WHERE version_id = $1").bind(version_id).fetch_optional(&state.pool).await?;
    let trust = trust.ok_or_else(ApiError::not_found)?;
    let now = (state.clock)();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO review_skips (version_id, trust, admin_id, note, created_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (version_id) DO UPDATE SET trust = EXCLUDED.trust, admin_id = EXCLUDED.admin_id,
             note = EXCLUDED.note, created_at = EXCLUDED.created_at",
    )
    .bind(version_id)
    .bind(&trust)
    .bind(admin.id)
    .bind(note)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let entry = Entry { admin_id: admin.id, action: "skip_version", target_type: "version", target_id: version_id.to_string(), note, details: json!({ "trust": trust }) };
    audit::record(&mut tx, entry, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The file on GitHub changed after the scan; the admin accepts the new one.
pub async fn accept_source(
    State(state): State<AppState>,
    StaffUser(admin): StaffUser,
    Path(version_id): Path<i64>,
    body: Option<Json<VerifyBody>>,
) -> ApiResult<StatusCode> {
    let note = body.map(|b| b.0.note).unwrap_or_default();
    if crate::source_check::accept_source(&state, admin.id, version_id, note.trim()).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
}
