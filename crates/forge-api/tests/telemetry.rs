mod common;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::Duration;
use common::{Reply, TestApp, send};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;

const ID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

async fn setup(pool: PgPool) -> TestApp {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app
}

fn event(kind: &str) -> Value {
    json!({ "kind": kind, "addon": "lucky", "version": "v1.0.0", "launcher_version": "0.4.3", "os": "windows" })
}

async fn post(router: &Router, install: Option<&str>, events: Value) -> Reply {
    let mut request = Request::post("/v1/telemetry/events").header("content-type", "application/json").header("user-agent", "SurvivorsForge/0.4.3");
    if let Some(id) = install {
        request = request.header("x-survivorsforge-install", id);
    }
    send(router, request.body(Body::from(json!({ "events": events }).to_string())).unwrap()).await
}

fn from(app: &TestApp, ip: [u8; 4]) -> Router {
    app.router.clone().layer(MockConnectInfo(SocketAddr::from((ip, 4000))))
}

async fn stored(app: &TestApp) -> Vec<(String, Option<String>, Option<String>)> {
    sqlx::query_as("SELECT kind, error_code, install_id::text FROM telemetry_events ORDER BY id").fetch_all(&app.pool).await.unwrap()
}

#[sqlx::test]
async fn accepts_valid_events(pool: PgPool) {
    let app = setup(pool).await;
    let failed = json!({ "error_code": "download_failed", "kind": "update_failed", "addon": "lucky", "version": "v1.1.0", "launcher_version": "0.4.3", "os": "linux" });
    let reply = post(&app.router, Some(ID), json!([event("install"), failed])).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.body);
    assert_eq!(reply.json(), json!({ "accepted": 2, "rejected": 0 }));
    assert_eq!(stored(&app).await, [("install".into(), None, Some(ID.into())), ("update_failed".into(), Some("download_failed".into()), Some(ID.into()))]);
}

#[sqlx::test]
async fn rejects_unknown_addon_and_kind(pool: PgPool) {
    let app = setup(pool).await;
    let mut ghost = event("install");
    ghost["addon"] = json!("ghost");
    let mut long = event("install");
    long["version"] = json!("v".repeat(65));
    let mut chatty = event("install_failed");
    chatty["error_code"] = json!("C:\\Users\\me\\AddOns is read-only");
    let reply = post(&app.router, None, json!([event("uninstall"), ghost, long, chatty, event("install_failed"), event("update")])).await;
    assert_eq!(reply.json(), json!({ "accepted": 1, "rejected": 5 }));
    let too_many: Vec<Value> = (0..51).map(|_| event("install")).collect();
    assert_eq!(post(&app.router, None, json!(too_many)).await.error(), "too_many_events");
}

#[sqlx::test]
async fn dedupes_per_sender_and_day(pool: PgPool) {
    let app = setup(pool).await;
    post(&app.router, Some(ID), json!([event("install"), event("install")])).await;
    post(&app.router, Some(ID), json!([event("install")])).await;
    assert_eq!(stored(&app).await.len(), 1, "one install ID");

    post(&from(&app, [203, 0, 113, 7]), None, json!([event("install")])).await;
    post(&from(&app, [203, 0, 113, 7]), None, json!([event("install")])).await;
    assert_eq!(stored(&app).await.len(), 2, "one IP without ID");
    post(&from(&app, [203, 0, 113, 8]), None, json!([event("install")])).await;
    post(&app.router, Some(ID), json!([event("update")])).await;
    assert_eq!(stored(&app).await.len(), 4, "another IP, another kind");
}

#[sqlx::test]
async fn next_day_counts_again(pool: PgPool) {
    let app = setup(pool).await;
    post(&app.router, Some(ID), json!([event("install")])).await;
    app.advance(Duration::days(1));
    post(&app.router, Some(ID), json!([event("install")])).await;
    assert_eq!(stored(&app).await.len(), 2);
}

#[sqlx::test]
async fn prune_forgets_install_ids(pool: PgPool) {
    let app = setup(pool).await;
    post(&app.router, Some(ID), json!([event("install")])).await;
    app.advance(Duration::days(91));
    post(&app.router, Some(ID), json!([event("update")])).await;
    let now = *app.now.lock().unwrap();
    forge_api::routes::telemetry::prune(&app.pool, now).await.unwrap();
    assert_eq!(stored(&app).await, [("install".into(), None, None), ("update".into(), None, Some(ID.into()))], "the count stays, the old ID goes");
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM telemetry_seen").fetch_one(&app.pool).await.unwrap();
    assert_eq!(seen, 1, "only today's dedupe row");
}

#[sqlx::test]
async fn bad_install_header_ignored(pool: PgPool) {
    let app = setup(pool).await;
    let reply = post(&app.router, Some("my-name-is-bob"), json!([event("install")])).await;
    assert_eq!(reply.json()["accepted"], 1);
    assert_eq!(stored(&app).await, [("install".into(), None, None)]);
}

#[sqlx::test]
async fn no_ip_stored(pool: PgPool) {
    let app = setup(pool).await;
    let columns: Vec<String> = sqlx::query_scalar("SELECT column_name::text FROM information_schema.columns WHERE table_name IN ('telemetry_events', 'telemetry_seen') ORDER BY 1")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert!(!columns.iter().any(|c| c.contains("ip") || c.contains("agent")), "{columns:?}");
}
