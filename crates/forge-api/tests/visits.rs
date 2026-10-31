mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::{Reply, TestApp, send};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;

const WEBSITE: [u8; 4] = [10, 213, 17, 11];

fn setup(pool: PgPool) -> TestApp {
    TestApp::with_config(pool, |c| c.website_ip = Some(WEBSITE.into()))
}

async fn post(app: &TestApp, peer: [u8; 4], rows: Value) -> Reply {
    let router: Router = app.router.clone().layer(MockConnectInfo(SocketAddr::from((peer, 4000))));
    let request = Request::post("/v1/internal/visits").header("content-type", "application/json").body(Body::from(json!({ "rows": rows }).to_string())).unwrap();
    send(&router, request).await
}

fn row(kind: &str, value: &str, target: &str, views: i64, senders: &[&str]) -> Value {
    json!({ "day": Utc::now().date_naive(), "kind": kind, "value": value, "target": target, "views": views, "senders": senders })
}

async fn counts(app: &TestApp) -> Vec<(String, String, String, i64, i64)> {
    sqlx::query_as("SELECT kind, value, target, views, visitors FROM web_counts ORDER BY kind, value, target").fetch_all(&app.pool).await.unwrap()
}

#[sqlx::test]
async fn batches_add_up_and_visitors_count_once(pool: PgPool) {
    let app = setup(pool);
    let first = post(&app, WEBSITE, json!([row("site", "", "", 3, &["aa01", "bb02"]), row("page", "/addons/{slug}", "lucky", 2, &["aa01"])])).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
    post(&app, WEBSITE, json!([row("site", "", "", 2, &["aa01", "cc03"]), row("referrer", "reddit.com", "", 1, &["cc03"])])).await;
    assert_eq!(counts(&app).await, [
        ("page".into(), "/addons/{slug}".into(), "lucky".into(), 2, 1),
        ("referrer".into(), "reddit.com".into(), "".into(), 1, 1),
        ("site".into(), "".into(), "".into(), 5, 3),
    ]);
}

#[sqlx::test]
async fn only_the_website_sends_visits(pool: PgPool) {
    let app = setup(pool);
    assert_eq!(post(&app, [203, 0, 113, 9], json!([row("site", "", "", 1000, &[])])).await.status, StatusCode::FORBIDDEN);
    assert_eq!(post(&app, WEBSITE, json!([row("cookies", "x", "", 1, &[])])).await.error(), "invalid_visits");
    assert!(counts(&app).await.is_empty());
}

#[sqlx::test]
async fn stats_show_the_website(pool: PgPool) {
    let app = setup(pool);
    post(&app, WEBSITE, json!([row("site", "", "", 7, &["aa01", "bb02"]), row("country", "DE", "", 7, &["aa01", "bb02"])])).await;
    let stats = app.get("/v1/stats", None).await.json();
    assert_eq!(stats["website"], json!({ "views": 7, "daily_visitors": 2, "views_30d": 7, "daily_visitors_30d": 2 }));

    let admin = TestApp::with_config(app.pool.clone(), |c| c.admin_github_ids = vec![99]);
    let token = admin.login(99, "root").await;
    let website = admin.get("/v1/admin/stats?range=7d", Some(&token)).await.json()["website"].clone();
    assert_eq!(website["countries"], json!([{ "value": "DE", "views": 7, "visitors": 2 }]));

    let now = *app.now.lock().unwrap();
    forge_api::routes::visits::prune(&app.pool, now + Duration::days(3)).await.unwrap();
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM web_seen").fetch_one(&app.pool).await.unwrap();
    assert_eq!((seen, counts(&app).await.len()), (0, 2), "hashes go, sums stay");
}
