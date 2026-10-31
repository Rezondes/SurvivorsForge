use forge_web::app::{AppState, router};
use forge_web::config::Config;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // `forge-web healthcheck` for the container: alive when it serves its own files, even while the API is down.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let url = std::env::args().nth(2).unwrap_or_else(|| "http://127.0.0.1:8080/static/app.css".into());
        return match reqwest::get(&url).await {
            Ok(r) if r.status().is_success() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        };
    }
    let _trace = forge_trace::init("web");
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let bind = config.bind_addr;
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("cannot listen on {bind}: {e}");
            return ExitCode::FAILURE;
        }
    };
    forge_web::metrics::spawn_server(config.metrics_bind_addr);
    tracing::info!(addr = %bind, "forge-web listening");
    let served = axum::serve(listener, router(AppState::new(config)).into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(shutdown_signal()).await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
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
