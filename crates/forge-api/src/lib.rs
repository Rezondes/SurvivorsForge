//! Backend API and worker of the SurvivorsForge platform.

pub mod addons;
pub mod app;
pub mod audit;
pub mod auth;
pub mod cache;
pub mod config;
pub mod error;
pub mod github;
pub mod images;
pub mod jobs;
pub mod launcher_stats;
pub mod pack_job;
pub mod rate_limit;
pub mod request_stats;
pub mod routes;
pub mod scan_job;
pub mod sender;
pub mod source_check;
pub mod suggest;
pub mod sync;
pub mod worker;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
