mod common;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use common::{TestApp, send};
use sqlx::PgPool;
use std::net::SocketAddr;

type Row = (String, String, i16, String, i64);

async fn rows(app: &TestApp) -> Vec<Row> {
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    sqlx::query_as("SELECT route, method, status, client, count FROM api_request_stats ORDER BY route, status, client")
        .fetch_all(&app.pool)
        .await
        .unwrap()
}

fn get(uri: &str, user_agent: &str) -> Request<Body> {
    Request::get(uri).header("user-agent", user_agent).body(Body::empty()).unwrap()
}

#[sqlx::test]
async fn counts_by_route_template(pool: PgPool) {
    let app = TestApp::new(pool);
    for slug in ["a", "a", "a", "b"] {
        send(&app.router, get(&format!("/v1/addons/{slug}"), "SurvivorsForge/0.4.3")).await;
    }
    send(&app.router, get("/v1/addons", "curl/8")).await;
    assert_eq!(
        rows(&app).await,
        [("/v1/addons".into(), "GET".into(), 200, "other".into(), 1), ("/v1/addons/{slug}".into(), "GET".into(), 404, "launcher".into(), 4)]
    );
}

#[sqlx::test]
async fn unknown_paths_are_unmatched(pool: PgPool) {
    let app = TestApp::new(pool);
    send(&app.router, get("/wp-admin/setup.php", "bot")).await;
    assert_eq!(rows(&app).await, [("unmatched".into(), "GET".into(), 404, "other".into(), 1)]);
}

#[sqlx::test]
async fn website_requests_are_marked(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.website_ip = Some("10.0.0.11".parse().unwrap()));
    let router = app.router.clone().layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 11], 4000))));
    send(&router, get("/v1/addons", "reqwest")).await;
    assert_eq!(rows(&app).await, [("/v1/addons".into(), "GET".into(), 200, "website".into(), 1)]);
}

#[sqlx::test]
async fn rate_limited_requests_counted(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.rate_limit_per_minute = 1);
    send(&app.router, get("/v1/addons", "curl/8")).await;
    let limited = send(&app.router, get("/v1/addons", "curl/8")).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let statuses: Vec<i16> = rows(&app).await.into_iter().map(|r| r.2).collect();
    assert_eq!(statuses, [200, 429]);
}

#[sqlx::test]
async fn flush_upserts_same_hour(pool: PgPool) {
    let app = TestApp::new(pool);
    send(&app.router, get("/v1/addons", "curl/8")).await;
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    send(&app.router, get("/v1/addons", "curl/8")).await;
    assert_eq!(rows(&app).await.iter().map(|r| r.4).collect::<Vec<_>>(), [2], "two flushes add up in one bucket");
    let hours: Vec<DateTime<Utc>> = sqlx::query_scalar("SELECT hour FROM api_request_stats").fetch_all(&app.pool).await.unwrap();
    let now = *app.now.lock().unwrap();
    assert_eq!(hours[0].timestamp() % 3600, 0, "a whole hour");
    assert!(now - hours[0] < Duration::hours(1));
}

#[sqlx::test]
async fn prune_drops_old_buckets(pool: PgPool) {
    let app = TestApp::new(pool);
    send(&app.router, get("/v1/addons", "curl/8")).await;
    rows(&app).await;
    app.advance(Duration::days(91));
    send(&app.router, get("/v1/addons/x", "curl/8")).await;
    rows(&app).await;
    let now = *app.now.lock().unwrap();
    forge_api::request_stats::prune(&app.pool, now).await.unwrap();
    let left: Vec<String> = sqlx::query_scalar("SELECT route FROM api_request_stats").fetch_all(&app.pool).await.unwrap();
    assert_eq!(left, ["/v1/addons/{slug}"]);
}

#[sqlx::test]
async fn healthchecks_not_counted(pool: PgPool) {
    let app = TestApp::new(pool);
    send(&app.router, get("/healthz", "forge-api")).await;
    assert_eq!(rows(&app).await, Vec::<Row>::new());
}

#[sqlx::test]
async fn metrics_not_on_public_router(pool: PgPool) {
    let app = TestApp::new(pool);
    assert_eq!(app.get("/metrics", None).await.status, StatusCode::NOT_FOUND);
}
