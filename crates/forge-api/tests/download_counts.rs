mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use common::{Reply, TestApp, send};
use sqlx::PgPool;
use std::net::SocketAddr;

const WEBSITE: [u8; 4] = [10, 213, 17, 11];

async fn setup(pool: PgPool) -> TestApp {
    let app = TestApp::with_config(pool, |c| {
        c.website_ip = Some(WEBSITE.into());
        c.trusted_proxies = vec![WEBSITE.into()];
    });
    app.publish(1, "alice", "lucky").await;
    app
}

/// A download the website reports for `visitor`, from `peer`.
async fn report(app: &TestApp, peer: [u8; 4], visitor: &str, slug: &str) -> Reply {
    let router: Router = app.router.clone().layer(MockConnectInfo(SocketAddr::from((peer, 4000))));
    let request = Request::post(format!("/v1/addons/{slug}/downloads")).header("x-forwarded-for", visitor).body(Body::empty()).unwrap();
    send(&router, request).await
}

async fn total(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(count), 0)::bigint FROM download_counts WHERE source = 'website'").fetch_one(&app.pool).await.unwrap()
}

#[sqlx::test]
async fn website_download_counted(pool: PgPool) {
    let app = setup(pool).await;
    assert_eq!(report(&app, WEBSITE, "203.0.113.7", "lucky").await.status, StatusCode::NO_CONTENT);
    assert_eq!(total(&app).await, 1);
    assert_eq!(report(&app, WEBSITE, "203.0.113.7", "ghost").await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn counter_only_from_website(pool: PgPool) {
    let app = setup(pool).await;
    assert_eq!(report(&app, [203, 0, 113, 9], "203.0.113.7", "lucky").await.status, StatusCode::FORBIDDEN);
    assert_eq!(total(&app).await, 0);
}

#[sqlx::test]
async fn same_visitor_counts_once_per_day(pool: PgPool) {
    let app = setup(pool).await;
    report(&app, WEBSITE, "203.0.113.7", "lucky").await;
    report(&app, WEBSITE, "203.0.113.7", "lucky").await;
    report(&app, WEBSITE, "203.0.113.8", "lucky").await;
    assert_eq!(total(&app).await, 2);
    app.advance(Duration::days(1));
    report(&app, WEBSITE, "203.0.113.7", "lucky").await;
    assert_eq!(total(&app).await, 3, "a new day");
    let now = *app.now.lock().unwrap();
    forge_api::downloads::prune(&app.pool, now + Duration::days(3)).await.unwrap();
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM download_seen").fetch_one(&app.pool).await.unwrap();
    assert_eq!((seen, total(&app).await), (0, 3), "dedupe rows go, counts stay");
}
