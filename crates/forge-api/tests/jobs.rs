use chrono::{Duration, Utc};
use forge_api::jobs::{self, LOCK_FOR, claim_next, complete, enqueue, fail};
use forge_api::worker::Worker;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

#[sqlx::test]
async fn concurrent_claims_never_share_a_job(pool: PgPool) {
    let now = Utc::now();
    for _ in 0..20 {
        enqueue(&pool, "test", json!({}), now, None).await.unwrap();
    }
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            while let Some(job) = claim_next(&pool, now).await.unwrap() {
                ids.push(job.id);
            }
            ids
        }));
    }
    let mut all = Vec::new();
    for task in tasks {
        all.extend(task.await.unwrap());
    }
    all.sort();
    let before = all.len();
    all.dedup();
    assert_eq!(before, 20);
    assert_eq!(all.len(), 20);
}

#[sqlx::test]
async fn job_that_keeps_killing_the_worker_is_given_up(pool: PgPool) {
    // A job whose run ends the process never reaches fail(): only its attempts grow.
    let now = Utc::now();
    let id = enqueue(&pool, "test", json!({}), now, None).await.unwrap().unwrap();
    sqlx::query("UPDATE jobs SET max_attempts = 2 WHERE id = $1").bind(id).execute(&pool).await.unwrap();
    assert!(claim_next(&pool, now).await.unwrap().is_some());
    let later = now + LOCK_FOR + Duration::seconds(1);
    assert!(claim_next(&pool, later).await.unwrap().is_some());
    let much_later = later + LOCK_FOR + Duration::seconds(1);
    assert!(claim_next(&pool, much_later).await.unwrap().is_none(), "not claimed a third time");
    let (failed, error): (bool, Option<String>) =
        sqlx::query_as("SELECT failed_at IS NOT NULL, last_error FROM jobs WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(failed);
    assert!(error.unwrap_or_default().contains("gave up"));
}

#[sqlx::test]
async fn failed_job_backs_off_then_gives_up(pool: PgPool) {
    let now = Utc::now();
    let id = enqueue(&pool, "test", json!({}), now, None).await.unwrap().unwrap();
    sqlx::query("UPDATE jobs SET max_attempts = 2 WHERE id = $1").bind(id).execute(&pool).await.unwrap();

    let job = claim_next(&pool, now).await.unwrap().unwrap();
    fail(&pool, &job, "boom 1", now).await.unwrap();
    assert!(claim_next(&pool, now).await.unwrap().is_none(), "backoff keeps it away for now");

    let later = now + Duration::seconds(61);
    let job = claim_next(&pool, later).await.unwrap().expect("runnable after backoff");
    assert_eq!(job.attempts, 2);
    fail(&pool, &job, "boom 2", later).await.unwrap();
    assert!(claim_next(&pool, later + Duration::days(1)).await.unwrap().is_none(), "gave up");

    let (error, failed): (Option<String>, bool) =
        sqlx::query_as("SELECT last_error, failed_at IS NOT NULL FROM jobs WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(error.as_deref(), Some("boom 2"));
    assert!(failed);
}

#[sqlx::test]
async fn expired_lock_is_reclaimed(pool: PgPool) {
    let now = Utc::now();
    enqueue(&pool, "test", json!({}), now, None).await.unwrap();
    let first = claim_next(&pool, now).await.unwrap().unwrap();
    assert!(claim_next(&pool, now + Duration::minutes(5)).await.unwrap().is_none());
    let again = claim_next(&pool, now + LOCK_FOR + Duration::seconds(1)).await.unwrap().unwrap();
    assert_eq!(again.id, first.id);
}

#[sqlx::test]
async fn dedupe_key_prevents_duplicate(pool: PgPool) {
    let now = Utc::now();
    assert!(enqueue(&pool, "scan", json!({}), now, Some("scan:1")).await.unwrap().is_some());
    assert!(enqueue(&pool, "scan", json!({}), now, Some("scan:1")).await.unwrap().is_none());
    let job = claim_next(&pool, now).await.unwrap().unwrap();
    complete(&pool, job.id, now).await.unwrap();
    assert!(enqueue(&pool, "scan", json!({}), now, Some("scan:1")).await.unwrap().is_some(), "done jobs free the key");
}

#[sqlx::test]
async fn periodic_jobs_are_scheduled_once(pool: PgPool) {
    let a = Worker::default().every("poll", Duration::days(1));
    let b = a.clone();
    a.schedule_periodic(&pool).await.unwrap();
    b.schedule_periodic(&pool).await.unwrap();
    a.schedule_periodic(&pool).await.unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM jobs WHERE kind = 'poll'").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);

    let now = Utc::now();
    assert!(jobs::enqueue_periodic(&pool, "poll", Duration::days(1), now + Duration::days(1)).await.unwrap(), "next slot runs again");
}

#[sqlx::test]
async fn prune_deletes_finished_jobs_after_30_days(pool: PgPool) {
    let now = Utc::now();
    let old = now - Duration::days(31);
    for kind in ["old_done", "old_failed", "new_done", "open"] {
        enqueue(&pool, kind, json!({}), old, None).await.unwrap();
    }
    sqlx::query("UPDATE jobs SET done_at = $1 WHERE kind = 'old_done'").bind(old).execute(&pool).await.unwrap();
    sqlx::query("UPDATE jobs SET failed_at = $1 WHERE kind = 'old_failed'").bind(old).execute(&pool).await.unwrap();
    sqlx::query("UPDATE jobs SET done_at = $1 WHERE kind = 'new_done'").bind(now - Duration::days(29)).execute(&pool).await.unwrap();
    jobs::prune(&pool, now).await.unwrap();
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM jobs ORDER BY kind").fetch_all(&pool).await.unwrap();
    assert_eq!(kinds, ["new_done", "open"], "an open job stays however old it is");
}

#[sqlx::test]
async fn shutdown_waits_for_running_job(pool: PgPool) {
    enqueue(&pool, "slow", json!({}), Utc::now(), None).await.unwrap();
    let started = CancellationToken::new();
    let finished = Arc::new(AtomicBool::new(false));
    let worker = {
        let (started, finished) = (started.clone(), finished.clone());
        Worker::default().handle("slow", move |_| {
            let (started, finished) = (started.clone(), finished.clone());
            async move {
                started.cancel();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                finished.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
    };
    let shutdown = CancellationToken::new();
    let run = {
        let (pool, shutdown) = (pool.clone(), shutdown.clone());
        tokio::spawn(async move { worker.run(&pool, shutdown).await })
    };
    started.cancelled().await;
    shutdown.cancel();
    run.await.unwrap();
    assert!(finished.load(Ordering::SeqCst), "the running job finished before shutdown");
    let (done,): (bool,) = sqlx::query_as("SELECT done_at IS NOT NULL FROM jobs").fetch_one(&pool).await.unwrap();
    assert!(done);
}

#[sqlx::test]
async fn unknown_kind_fails_the_job(pool: PgPool) {
    enqueue(&pool, "mystery", json!({}), Utc::now(), None).await.unwrap();
    assert!(Worker::default().run_once(&pool).await.unwrap());
    let (error,): (Option<String>,) = sqlx::query_as("SELECT last_error FROM jobs").fetch_one(&pool).await.unwrap();
    assert!(error.unwrap().contains("mystery"));
}
