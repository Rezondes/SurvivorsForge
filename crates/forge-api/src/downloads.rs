//! Downloads through the website and of packs, counted once per visitor and day without storing
//! an IP (`crate::sender`).

use crate::app::AppState;
use chrono::{DateTime, TimeDelta, Utc};
use std::net::IpAddr;

/// The daily job that deletes old dedupe rows.
pub const PRUNE: &str = "prune_downloads";

#[derive(Debug, Clone, Copy)]
pub enum Source {
    Website,
    Pack,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Website => "website",
            Source::Pack => "pack",
        }
    }
}

/// Counts one download of `target_id` by the visitor at `ip`, unless they downloaded it today.
pub async fn count(state: &AppState, source: Source, target_id: i64, ip: IpAddr) -> sqlx::Result<()> {
    let day = (state.clock)().date_naive();
    let mut tx = state.pool.begin().await?;
    let first: Option<i32> = sqlx::query_scalar(
        "INSERT INTO download_seen (day, sender, source, target_id) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING 1",
    )
    .bind(day)
    .bind(state.sender_key.hash(day, ip))
    .bind(source.as_str())
    .bind(target_id)
    .fetch_optional(&mut *tx)
    .await?;
    if first.is_some() {
        sqlx::query(
            "INSERT INTO download_counts (day, source, target_id, count) VALUES ($1, $2, $3, 1)
             ON CONFLICT (day, source, target_id) DO UPDATE SET count = download_counts.count + 1",
        )
        .bind(day)
        .bind(source.as_str())
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn prune(pool: &sqlx::PgPool, now: DateTime<Utc>) -> Result<(), String> {
    sqlx::query("DELETE FROM download_seen WHERE day < $1")
        .bind((now - TimeDelta::days(2)).date_naive())
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
