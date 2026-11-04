//! Background jobs in Postgres. `claim_next` uses `FOR UPDATE SKIP LOCKED`, so any number of
//! workers can pull from the same table without taking the same job.
//!
//! The current time is always a parameter, so tests never wait.

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

/// How long a claimed job stays locked. A worker that dies mid-job loses it after this.
pub const LOCK_FOR: Duration = Duration::minutes(10);
/// Days a done or failed job stays for the statistics.
pub const KEEP_DAYS: i64 = 30;
/// The daily job that deletes finished jobs older than `KEEP_DAYS`.
pub const PRUNE: &str = "prune_jobs";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// Queues a job. `None` when an open job with the same `dedupe_key` exists already.
pub async fn enqueue(
    pool: &PgPool,
    kind: &str,
    payload: serde_json::Value,
    run_at: DateTime<Utc>,
    dedupe_key: Option<&str>,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "INSERT INTO jobs (kind, payload, run_at, dedupe_key) VALUES ($1, $2, $3, $4)
         ON CONFLICT (dedupe_key) WHERE done_at IS NULL AND failed_at IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .bind(dedupe_key)
    .fetch_optional(pool)
    .await
}

/// Queues the run of a periodic job for the time slot `now` falls into, once across all workers.
pub async fn enqueue_periodic(pool: &PgPool, kind: &str, every: Duration, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let slot = now.timestamp() / every.num_seconds().max(1);
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO jobs (kind, run_at, periodic_key) VALUES ($1, $2, $3)
         ON CONFLICT (periodic_key) DO NOTHING RETURNING id",
    )
    .bind(kind)
    .bind(now)
    .bind(format!("{kind}:{slot}"))
    .fetch_optional(pool)
    .await?;
    Ok(inserted.is_some())
}

/// Takes the oldest runnable job and locks it for `LOCK_FOR`. Counts the attempt.
///
/// A job whose run ends the process (a crash, an abort) never reaches `fail`: only its attempts
/// grow. Once they reach `max_attempts` and the lock ran out, it is given up here instead of being
/// claimed again and again.
pub async fn claim_next(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Option<Job>> {
    sqlx::query(
        "UPDATE jobs SET failed_at = $1, locked_until = NULL,
                last_error = 'gave up after ' || attempts || ' attempts that did not finish'
         WHERE done_at IS NULL AND failed_at IS NULL AND attempts >= max_attempts
           AND (locked_until IS NULL OR locked_until < $1)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query_as(
        "UPDATE jobs SET locked_until = $2, attempts = attempts + 1
         WHERE id = (
             SELECT id FROM jobs
             WHERE done_at IS NULL AND failed_at IS NULL AND run_at <= $1
               AND (locked_until IS NULL OR locked_until < $1)
             ORDER BY run_at, id
             FOR UPDATE SKIP LOCKED
             LIMIT 1)
         RETURNING id, kind, payload, attempts, max_attempts",
    )
    .bind(now)
    .bind(now + LOCK_FOR)
    .fetch_optional(pool)
    .await
}

pub async fn complete(pool: &PgPool, id: i64, now: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET done_at = $2, locked_until = NULL WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Deletes jobs that were done or failed more than `KEEP_DAYS` ago. Open jobs stay. A periodic
/// job's slot is long past by then, so its key cannot run again.
pub async fn prune(pool: &PgPool, now: DateTime<Utc>) -> Result<(), String> {
    sqlx::query("DELETE FROM jobs WHERE coalesce(done_at, failed_at) < $1")
        .bind(now - Duration::days(KEEP_DAYS))
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 30 s doubled per attempt, at most an hour.
pub fn backoff(attempts: i32) -> Duration {
    let seconds = 30i64.saturating_mul(1i64 << attempts.clamp(0, 20));
    Duration::seconds(seconds.min(3600))
}

/// Records the error. Plans another try after `backoff`, or gives up after `max_attempts`.
pub async fn fail(pool: &PgPool, job: &Job, error: &str, now: DateTime<Utc>) -> sqlx::Result<()> {
    if job.attempts >= job.max_attempts {
        sqlx::query("UPDATE jobs SET failed_at = $2, last_error = $3, locked_until = NULL WHERE id = $1")
            .bind(job.id)
            .bind(now)
            .bind(error)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("UPDATE jobs SET run_at = $2, last_error = $3, locked_until = NULL WHERE id = $1")
            .bind(job.id)
            .bind(now + backoff(job.attempts))
            .bind(error)
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_an_hour() {
        assert_eq!(backoff(0), Duration::seconds(30));
        assert_eq!(backoff(1), Duration::seconds(60));
        assert_eq!(backoff(3), Duration::seconds(240));
        assert_eq!(backoff(10), Duration::seconds(3600));
        assert_eq!(backoff(1000), Duration::seconds(3600));
    }
}
