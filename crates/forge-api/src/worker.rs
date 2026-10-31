//! `forge-api worker`: takes jobs from the queue and runs the handler of their kind.

use crate::app::AppState;
use crate::jobs::{self, Job};
use crate::sync;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
pub type Handler = Arc<dyn Fn(Job) -> HandlerFuture + Send + Sync>;

/// How long the worker sleeps when the queue is empty.
const IDLE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone, Default)]
pub struct Worker {
    handlers: HashMap<&'static str, Handler>,
    periodic: Vec<(&'static str, Duration)>,
}

/// How often the releases of every addon are polled (`sync::POLL_RELEASES`).
pub const POLL_RELEASES_EVERY: Duration = Duration::minutes(15);
/// How often stored versions are compared with GitHub (`source_check::CHECK_SOURCES`).
pub const CHECK_SOURCES_EVERY: Duration = Duration::hours(6);

/// The worker of the platform: every job kind and every periodic job.
pub fn platform(state: AppState) -> Worker {
    let sync_state = state.clone();
    let poll_state = state.clone();
    let scan_state = state.clone();
    let source_state = state.clone();
    let pack_state = state.clone();
    let stats_state = state.clone();
    let launcher_state = state.clone();
    let telemetry_state = state.clone();
    let downloads_state = state.clone();
    let visits_state = state.clone();
    let prune_state = state;
    Worker::default()
        .handle(sync::SYNC_ADDON, move |job| {
            let state = sync_state.clone();
            async move {
                let addon_id = job.payload["addon_id"].as_i64().ok_or("payload without addon_id")?;
                sync::sync_addon(&state, addon_id).await
            }
        })
        .handle(sync::POLL_RELEASES, move |_| {
            let state = poll_state.clone();
            async move { sync::poll_releases(&state).await }
        })
        .handle(sync::SCAN_VERSION, move |job| {
            let state = scan_state.clone();
            async move { crate::scan_job::run(&state, &job).await }
        })
        .handle(crate::source_check::CHECK_SOURCES, move |_| {
            let state = source_state.clone();
            async move { crate::source_check::check_sources(&state, state.config.source_check_batch).await }
        })
        .handle(crate::pack_job::KIND, move |job| {
            let state = pack_state.clone();
            async move { crate::pack_job::build(&state, &job.payload).await }
        })
        .handle(crate::pack_job::PRUNE, move |_| {
            let state = prune_state.clone();
            async move { crate::pack_job::prune(&state).await }
        })
        .handle(crate::request_stats::PRUNE, move |_| {
            let state = stats_state.clone();
            async move { crate::request_stats::prune(&state.pool, (state.clock)()).await }
        })
        .handle(crate::launcher_stats::POLL, move |_| {
            let state = launcher_state.clone();
            async move { crate::launcher_stats::poll(&state).await }
        })
        .handle(crate::routes::telemetry::PRUNE, move |_| {
            let state = telemetry_state.clone();
            async move { crate::routes::telemetry::prune(&state.pool, (state.clock)()).await }
        })
        .handle(crate::downloads::PRUNE, move |_| {
            let state = downloads_state.clone();
            async move { crate::downloads::prune(&state.pool, (state.clock)()).await }
        })
        .handle(crate::routes::visits::PRUNE, move |_| {
            let state = visits_state.clone();
            async move { crate::routes::visits::prune(&state.pool, (state.clock)()).await }
        })
        .every(sync::POLL_RELEASES, POLL_RELEASES_EVERY)
        .every(crate::pack_job::PRUNE, Duration::hours(1))
        .every(crate::source_check::CHECK_SOURCES, CHECK_SOURCES_EVERY)
        .every(crate::request_stats::PRUNE, Duration::days(1))
        .every(crate::launcher_stats::POLL, Duration::hours(1))
        .every(crate::routes::telemetry::PRUNE, Duration::days(1))
        .every(crate::downloads::PRUNE, Duration::days(1))
        .every(crate::routes::visits::PRUNE, Duration::days(1))
}

impl Worker {
    pub fn handle<F, Fut>(mut self, kind: &'static str, handler: F) -> Worker
    where
        F: Fn(Job) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.handlers.insert(kind, Arc::new(move |job| Box::pin(handler(job))));
        self
    }

    /// Runs a job of `kind` once per `every`, no matter how many workers run.
    pub fn every(mut self, kind: &'static str, every: Duration) -> Worker {
        self.periodic.push((kind, every));
        self
    }

    pub async fn schedule_periodic(&self, pool: &PgPool) -> sqlx::Result<()> {
        for (kind, every) in &self.periodic {
            jobs::enqueue_periodic(pool, kind, *every, Utc::now()).await?;
        }
        Ok(())
    }

    /// Runs one job if there is one. `false` when the queue is empty.
    pub async fn run_once(&self, pool: &PgPool) -> sqlx::Result<bool> {
        let Some(job) = jobs::claim_next(pool, Utc::now()).await? else { return Ok(false) };
        let started = std::time::Instant::now();
        let origin = format!("job {}", job.kind);
        let span = tracing::info_span!("job", kind = %job.kind, id = job.id, trace_id = tracing::field::Empty);
        forge_trace::record_trace_id(&span);
        let result = match self.handlers.get(job.kind.as_str()) {
            Some(handler) => tracing::Instrument::instrument(crate::github::usage::ORIGIN.scope(origin, handler(job.clone())), span).await,
            None => Err(format!("no handler for job kind {}", job.kind)),
        };
        let outcome = if result.is_ok() { "done" } else { "failed" };
        metrics::counter!("forge_jobs_total", "kind" => job.kind.clone(), "outcome" => outcome).increment(1);
        metrics::histogram!("forge_job_duration_seconds", "kind" => job.kind.clone()).record(started.elapsed().as_secs_f64());
        match result {
            Ok(()) => jobs::complete(pool, job.id, Utc::now()).await?,
            Err(e) => {
                tracing::warn!(job = job.id, kind = %job.kind, error = %e, "job failed");
                jobs::fail(pool, &job, &e, Utc::now()).await?;
            }
        }
        Ok(true)
    }

    /// Works until `shutdown` is cancelled. A running job always finishes first.
    pub async fn run(&self, pool: &PgPool, shutdown: CancellationToken) {
        while !shutdown.is_cancelled() {
            if let Err(e) = self.schedule_periodic(pool).await {
                tracing::error!(error = %e, "cannot schedule periodic jobs");
            }
            let worked = self.run_once(pool).await.unwrap_or_else(|e| {
                tracing::error!(error = %e, "cannot take a job");
                false
            });
            if !worked {
                tokio::select! {
                    _ = shutdown.cancelled() => {}
                    _ = tokio::time::sleep(IDLE) => {}
                }
            }
        }
    }
}
