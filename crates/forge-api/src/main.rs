use forge_api::MIGRATOR;
use forge_api::app::{AppState, router};
use forge_api::config::Config;
use forge_api::github::app_auth::load_private_key;
use forge_api::github::{GitHubClient, OAuthApp};
use forge_api::worker;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: forge-api <serve | worker | migrate | rescan --all | import-verified <file> | export-verified [file]>";

#[tokio::main]
async fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_default();
    let _trace = forge_trace::init(if command == "worker" { "worker" } else { "api" });
    if command == "healthcheck" {
        return healthcheck(std::env::args().nth(2).unwrap_or_default()).await;
    }
    if !matches!(command.as_str(), "serve" | "worker" | "migrate" | "rescan" | "import-verified" | "export-verified") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let pool = match PgPoolOptions::new().max_connections(10).connect(&config.database_url).await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("cannot connect to the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match command.as_str() {
        "migrate" => MIGRATOR.run(&pool).await.map_err(|e| e.to_string()),
        "worker" => work(config, pool).await,
        "rescan" if std::env::args().nth(2).as_deref() == Some("--all") => match build_state(config, pool) {
            Ok(state) => forge_api::scan_job::rescan_outdated(&state)
                .await
                .map(|n| println!("queued {n} scans"))
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        },
        "rescan" => Err(USAGE.to_string()),
        "import-verified" | "export-verified" => registry_file(&command, config, pool).await,
        _ => serve(config, pool).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The shared state of `serve` and `worker`, with the real GitHub client.
fn build_state(config: Config, pool: sqlx::PgPool) -> Result<AppState, String> {
    let key = load_private_key(config.github_app_private_key.as_deref(), config.github_app_private_key_file.as_deref())?;
    let oauth = OAuthApp {
        oauth_url: config.github_oauth_url.clone(),
        client_id: config.github_client_id.clone(),
        client_secret: config.github_client_secret.clone(),
    };
    let github = GitHubClient::new(&config.github_api_url, &config.github_app_id, key, Arc::new(chrono::Utc::now)).with_oauth(oauth).with_read_token(config.github_read_token.clone());
    Ok(AppState { pool, config: Arc::new(config), github: Arc::new(github), clock: Arc::new(chrono::Utc::now), request_stats: Default::default(), sender_key: Default::default() })
}

/// `import-verified <file>` adds the entries of a verified.json; `export-verified [file]` writes one.
async fn registry_file(command: &str, config: Config, pool: sqlx::PgPool) -> Result<(), String> {
    use forge_api::routes::registry::{Imported, export_verified, import_verified};
    let path = std::env::args().nth(2);
    let state = build_state(config, pool)?;
    if command == "export-verified" {
        let json = export_verified(&state).await.map_err(|e| e.to_string())?;
        return match path {
            Some(path) => std::fs::write(&path, json).map_err(|e| format!("cannot write {path}: {e}")),
            None => {
                print!("{json}");
                Ok(())
            }
        };
    }
    let path = path.ok_or_else(|| USAGE.to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
    for result in import_verified(&state, &text).await? {
        match result {
            Imported::Created(id) => println!("created  {id}"),
            Imported::Skipped(id, why) => println!("skipped  {id}: {why}"),
        }
    }
    Ok(())
}

async fn work(config: Config, pool: sqlx::PgPool) -> Result<(), String> {
    let state = build_state(config, pool.clone())?;
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        token.cancel();
    });
    forge_api::request_stats::spawn_metrics_server(state.config.metrics_bind_addr);
    tracing::info!("forge-api worker started");
    let flusher = {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                every.tick().await;
                if let Err(e) = forge_api::github::usage::USAGE.flush(&pool).await {
                    tracing::warn!(error = %e, "cannot write GitHub usage");
                }
            }
        })
    };
    worker::platform(state).run(&pool, shutdown).await;
    flusher.abort();
    if let Err(e) = forge_api::github::usage::USAGE.flush(&pool).await {
        tracing::warn!(error = %e, "cannot write the last GitHub usage");
    }
    Ok(())
}

async fn serve(config: Config, pool: sqlx::PgPool) -> Result<(), String> {
    if config.migrate_on_start {
        MIGRATOR.run(&pool).await.map_err(|e| e.to_string())?;
    }
    if let Some(password) = &config.grafana_db_password {
        forge_api::grafana::set_reader_password(&pool, password).await.map_err(|e| format!("cannot set the grafana_reader password: {e}"))?;
    }
    let bind_addr = config.bind_addr;
    forge_api::request_stats::spawn_metrics_server(config.metrics_bind_addr);
    let state = build_state(config, pool.clone())?;
    let stats = state.request_stats.clone();
    let app = router(state);
    let flusher = {
        let (stats, pool) = (stats.clone(), pool.clone());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                every.tick().await;
                if let Err(e) = stats.flush(&pool).await {
                    tracing::warn!(error = %e, "cannot write request stats");
                }
                if let Err(e) = forge_api::github::usage::USAGE.flush(&pool).await {
                    tracing::warn!(error = %e, "cannot write GitHub usage");
                }
            }
        })
    };
    let listener = tokio::net::TcpListener::bind(bind_addr).await.map_err(|e| e.to_string())?;
    tracing::info!(addr = %bind_addr, "forge-api listening");
    let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string());
    flusher.abort();
    if let Err(e) = stats.flush(&pool).await {
        tracing::warn!(error = %e, "cannot write the last request stats");
    }
    if let Err(e) = forge_api::github::usage::USAGE.flush(&pool).await {
        tracing::warn!(error = %e, "cannot write the last GitHub usage");
    }
    served
}

/// `healthcheck <url>` asks a running server, `healthcheck db` the database; for container
/// healthchecks, since the image has no shell or curl.
async fn healthcheck(target: String) -> ExitCode {
    let ok = if target == "db" {
        match Config::from_env() {
            Ok(config) => match PgPoolOptions::new().max_connections(1).connect(&config.database_url).await {
                Ok(pool) => sqlx::query("SELECT 1").execute(&pool).await.is_ok(),
                Err(_) => false,
            },
            Err(_) => false,
        }
    } else {
        let url = if target.is_empty() { "http://127.0.0.1:8080/healthz".to_string() } else { target };
        matches!(reqwest::get(&url).await, Ok(r) if r.status().is_success())
    };
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Ctrl+C, and SIGTERM from `docker stop` on Unix.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}
