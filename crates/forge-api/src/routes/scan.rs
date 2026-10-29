//! `POST /v1/scan`: a creator scans a zip before publishing it. Nothing is stored.

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::scan_job::MAX_DOWNLOAD;
use axum::Json;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use forge_core::archive::analyze_reader;
use serde_json::{Value, json};
use std::io::Cursor;

fn bad(status: StatusCode, code: &'static str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}

/// Scans the zip in the field `file` like a release of an addon. Its own folders are the ones the
/// zip contains; the folders other addons registered count as foreign, except those of addons the
/// user owns or helps with, so an update of their own addon passes.
pub async fn scan(State(state): State<AppState>, user: CurrentUser, mut form: Multipart) -> ApiResult<Json<Value>> {
    let too_large = || bad(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "The zip may be at most 50 MB");
    let mut file = None;
    while let Some(field) = form.next_field().await.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE { too_large() } else { bad(StatusCode::BAD_REQUEST, "bad_upload", "The upload is broken") }
    })? {
        if field.name() == Some("file") {
            let bytes = field.bytes().await.map_err(|_| too_large())?;
            if bytes.len() as u64 > MAX_DOWNLOAD {
                return Err(too_large());
            }
            file = Some(bytes);
        }
    }
    let file = file.filter(|f| !f.is_empty()).ok_or_else(|| bad(StatusCode::UNPROCESSABLE_ENTITY, "no_file", "Send the zip as the field file"))?;
    let foreign: Vec<String> = sqlx::query_scalar(
        "SELECT f.folder::text FROM addon_folders f JOIN addons a ON a.id = f.addon_id
         WHERE f.active AND a.deleted_at IS NULL AND a.owner_id IS DISTINCT FROM $1
           AND NOT EXISTS (SELECT 1 FROM addon_members m WHERE m.addon_id = a.id AND m.user_id = $1)",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let (folders, report) = tokio::task::spawn_blocking(move || {
        let folders: Vec<String> = analyze_reader(Cursor::new(&file[..])).unwrap_or_default().into_iter().map(|a| a.name).collect();
        let own: Vec<String> = folders.iter().filter(|f| !foreign.iter().any(|g| g.eq_ignore_ascii_case(f))).cloned().collect();
        let report = forge_scan::scan(&file, &own, &foreign);
        (folders, report)
    })
    .await
    .map_err(|_| ApiError::internal())?;
    Ok(Json(json!({
        "scanner_version": forge_scan::SCANNER_VERSION,
        "result": if report.flagged() { "flagged" } else { "clean" },
        "folders": folders,
        "findings": report.findings,
    })))
}
