//! `POST /v1/internal/visits`: the website's page views of the last minute, already summed up.
//! Only the website may send them (`WEBSITE_IP`); it never sends an IP, only daily keyed hashes.

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use crate::rate_limit::FromWebsite;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use serde::Deserialize;

/// The daily job that deletes old dedupe rows.
pub const PRUNE: &str = "prune_web_seen";

#[derive(Deserialize)]
pub struct Row {
    day: NaiveDate,
    kind: String,
    value: String,
    #[serde(default)]
    target: String,
    views: i64,
    /// Hex hashes of the visitors behind the views.
    #[serde(default)]
    senders: Vec<String>,
}

#[derive(Deserialize)]
pub struct Batch {
    rows: Vec<Row>,
}

fn invalid(what: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_visits", format!("Invalid {what}"))
}

pub async fn record(State(state): State<AppState>, _: FromWebsite, Json(batch): Json<Batch>) -> ApiResult<StatusCode> {
    let mut tx = state.pool.begin().await?;
    for row in &batch.rows {
        if !matches!(row.kind.as_str(), "site" | "page" | "referrer" | "country" | "device") {
            return Err(invalid("kind"));
        }
        if row.value.len() > 200 || row.target.len() > 100 || row.views < 0 {
            return Err(invalid("row"));
        }
        let mut visitors = 0i64;
        for sender in &row.senders {
            let sender = (0..sender.len()).step_by(2).map(|i| u8::from_str_radix(sender.get(i..i + 2).unwrap_or("x"), 16)).collect::<Result<Vec<u8>, _>>().map_err(|_| invalid("sender"))?;
            let new: Option<i32> = sqlx::query_scalar(
                "INSERT INTO web_seen (day, sender, kind, value, target) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING RETURNING 1",
            )
            .bind(row.day)
            .bind(sender)
            .bind(&row.kind)
            .bind(&row.value)
            .bind(&row.target)
            .fetch_optional(&mut *tx)
            .await?;
            visitors += i64::from(new.is_some());
        }
        sqlx::query(
            "INSERT INTO web_counts (day, kind, value, target, views, visitors) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (day, kind, value, target) DO UPDATE
                 SET views = web_counts.views + EXCLUDED.views, visitors = web_counts.visitors + EXCLUDED.visitors",
        )
        .bind(row.day)
        .bind(&row.kind)
        .bind(&row.value)
        .bind(&row.target)
        .bind(row.views)
        .bind(visitors)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn prune(pool: &sqlx::PgPool, now: DateTime<Utc>) -> Result<(), String> {
    sqlx::query("DELETE FROM web_seen WHERE day < $1")
        .bind((now - TimeDelta::days(2)).date_naive())
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
