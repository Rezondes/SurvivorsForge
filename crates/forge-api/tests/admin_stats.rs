mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use common::{TestApp, send};
use forge_core::roles::Role;
use serde_json::json;
use sqlx::PgPool;

/// Root is admin by configuration, mia a moderator.
async fn setup(pool: PgPool, stats_roles: Vec<Role>) -> (TestApp, String, String) {
    let app = TestApp::with_config(pool, |c| {
        c.admin_github_ids = vec![99];
        c.stats_roles = stats_roles;
    });
    app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    app.login(7, "mia").await;
    app.put("/v1/admin/users/7/role", Some(&admin), json!({ "role": "moderator", "note": "helps" })).await;
    let moderator = app.login(7, "mia").await;
    (app, admin, moderator)
}

#[sqlx::test]
async fn aggregates_requests_by_route(pool: PgPool) {
    let (app, admin, _) = setup(pool, vec![Role::Admin, Role::Moderator]).await;
    for slug in ["a", "b", "lucky"] {
        send(&app.router, Request::get(format!("/v1/addons/{slug}")).header("user-agent", "SurvivorsForge/0.4.3").body(Body::empty()).unwrap()).await;
    }
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    let stats = app.get("/v1/admin/stats?range=24h", Some(&admin)).await.json();
    let route = stats["requests"]["routes"].as_array().unwrap().iter().find(|r| r["route"] == "/v1/addons/{slug}").cloned().unwrap();
    assert_eq!((route["method"].as_str(), route["count"].as_i64()), (Some("GET"), Some(3)));
    assert!(stats["requests"]["clients"].as_array().unwrap().iter().any(|c| c["client"] == "launcher" && c["count"] == 3));
    let statuses: Vec<i64> = stats["requests"]["statuses"].as_array().unwrap().iter().map(|s| s["status"].as_i64().unwrap()).collect();
    assert!(statuses.contains(&200) && statuses.contains(&404), "{statuses:?}");
    assert!(stats["github"]["quotas"].is_array() && stats["github"]["origins"].is_array(), "{}", stats["github"]);
    let installation: i64 = sqlx::query_scalar("SELECT installation_id FROM addons WHERE slug = 'lucky'").fetch_one(&app.pool).await.unwrap();
    // Addons from an account without the App have no installation.
    sqlx::query("INSERT INTO addons (slug, name, owner_id, repo_id, repo_full_name, installation_id, source_type, asset_pattern, category, created_at, updated_at)
                 SELECT 'solo', 'Solo', owner_id, 4242, 'bob/solo', NULL, source_type, asset_pattern, category, now(), now() FROM addons WHERE slug = 'lucky'")
        .execute(&app.pool)
        .await
        .unwrap();
    let stats = app.get("/v1/admin/stats?range=24h", Some(&admin)).await.json();
    assert_eq!(stats["github"]["installations"][installation.to_string()], "alice", "the account behind each App installation");
    assert_eq!(app.get("/v1/admin/stats?range=1y", Some(&admin)).await.error(), "invalid_range");
}

#[sqlx::test]
async fn job_queue_numbers(pool: PgPool) {
    let (app, admin, _) = setup(pool, vec![Role::Admin]).await;
    let now = *app.now.lock().unwrap();
    sqlx::query(
        "INSERT INTO jobs (kind, run_at, done_at, failed_at, last_error) VALUES
             ('demo_job', $1, $1, NULL, NULL), ('demo_job', $1, NULL, $1, 'GitHub down'), ('demo_job', $1, NULL, NULL, NULL),
             ('demo_job', $2, $2, NULL, NULL)",
    )
    .bind(now)
    .bind(now - Duration::days(3))
    .execute(&app.pool)
    .await
    .unwrap();
    let jobs = app.get("/v1/admin/stats", Some(&admin)).await.json()["jobs"].clone();
    let sync = jobs.as_array().unwrap().iter().find(|j| j["kind"] == "demo_job").cloned().unwrap();
    assert_eq!(sync, json!({ "kind": "demo_job", "open": 1, "running": 0, "done": 1, "failed": 1, "last_error": "GitHub down" }), "done counts in the range only");
}

#[sqlx::test]
async fn telemetry_failure_rates(pool: PgPool) {
    let (app, admin, _) = setup(pool, vec![Role::Admin]).await;
    let event = |kind: &str, code: Option<&str>| json!({ "kind": kind, "addon": "lucky", "version": "v1", "launcher_version": "0.4.3", "os": "windows", "error_code": code });
    for (i, (kind, code)) in [("install", None), ("install_failed", Some("hash_mismatch")), ("update_failed", Some("network"))].into_iter().enumerate() {
        let request = Request::post("/v1/telemetry/events")
            .header("content-type", "application/json")
            .header("x-survivorsforge-install", format!("3f2504e0-4f89-41d3-9a0c-0305e82c330{i}"))
            .body(Body::from(json!({ "events": [event(kind, code)] }).to_string()))
            .unwrap();
        send(&app.router, request).await;
    }
    let telemetry = app.get("/v1/admin/stats?range=7d", Some(&admin)).await.json()["telemetry"].clone();
    assert_eq!(telemetry["failing_addons"], json!([{ "slug": "lucky", "failed": 2, "total": 3 }]));
    assert_eq!(telemetry["error_codes"], json!([{ "code": "hash_mismatch", "count": 1 }, { "code": "network", "count": 1 }]));
    assert_eq!(telemetry["launcher_versions"], json!([{ "version": "0.4.3", "count": 3 }]));
}

#[sqlx::test]
async fn permission_from_config(pool: PgPool) {
    let (app, admin, moderator) = setup(pool.clone(), vec![Role::Admin]).await;
    assert_eq!(app.get("/v1/admin/stats", Some(&admin)).await.status, StatusCode::OK);
    assert_eq!(app.get("/v1/admin/stats", Some(&moderator)).await.status, StatusCode::FORBIDDEN, "not in ADMIN_STATS_ROLES");
    let owner = app.login(1, "alice").await;
    assert_eq!(app.get("/v1/admin/stats", Some(&owner)).await.status, StatusCode::FORBIDDEN);

    let both = TestApp::with_config(pool, |c| c.stats_roles = vec![Role::Admin, Role::Moderator]);
    let moderator = both.login(7, "mia").await;
    assert_eq!(both.get("/v1/admin/stats", Some(&moderator)).await.status, StatusCode::OK, "only the configuration changed");
}

#[sqlx::test]
async fn sections_send_only_their_part(pool: PgPool) {
    let (app, admin, _) = setup(pool, vec![Role::Admin]).await;
    let keys = |v: serde_json::Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().filter(|k| *k != "since" && *k != "section").cloned().collect();
        k.sort();
        k
    };
    let overview = app.get("/v1/admin/stats?range=7d", Some(&admin)).await.json();
    assert_eq!(keys(overview.clone()), ["github", "jobs", "requests", "scans", "telemetry", "website"]);
    assert_eq!(overview["section"], "overview");
    assert_eq!(keys(app.get("/v1/admin/stats?section=github", Some(&admin)).await.json()), ["github"]);
    assert_eq!(keys(app.get("/v1/admin/stats?section=jobs", Some(&admin)).await.json()), ["jobs", "scans"]);
    assert_eq!(keys(app.get("/v1/admin/stats?section=website", Some(&admin)).await.json()), ["website"]);
    assert_eq!(app.get("/v1/admin/stats?section=secrets", Some(&admin)).await.error(), "invalid_section");
}
