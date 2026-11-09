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
    send(&app.router, get("/v1/addons", "MyTool/1.0")).await;
    assert_eq!(
        rows(&app).await,
        [("/v1/addons".into(), "GET".into(), 200, "other".into(), 1), ("/v1/addons/{slug}".into(), "GET".into(), 404, "launcher".into(), 4)]
    );
}

#[sqlx::test]
async fn active_launchers_are_counted_per_hour_platform_and_version(pool: PgPool) {
    let app = TestApp::new(pool);
    let with_id = |agent: &str, id: &str| Request::get("/v1/addons").header("user-agent", agent).header("x-survivorsforge-install", id).body(Body::empty()).unwrap();
    let a = "11111111-2222-3333-4444-555555555555";
    let b = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    for request in [
        with_id("SurvivorsForge/0.6.4 (windows)", a),
        with_id("SurvivorsForge/0.6.4 (windows)", a),
        with_id("SurvivorsForge/0.6.4 (linux)", b),
        // Before 0.6.4 the launcher did not name its platform; a strange one is not stored as it is.
        with_id("SurvivorsForge/0.6.3", "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee"),
        with_id("SurvivorsForge/0.6.4 (amiga)", "dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee"),
        // Without an install ID nobody can be told apart; another client is no launcher.
        get("/v1/addons", "SurvivorsForge/0.6.4 (windows)"),
        with_id("curl/8", "eeeeeeee-bbbb-cccc-dddd-eeeeeeeeeeee"),
    ] {
        send(&app.router, request).await;
    }
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    let active: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT os, version, count(DISTINCT install_id) FROM launcher_active GROUP BY 1, 2 ORDER BY 1, 2").fetch_all(&app.pool).await.unwrap();
    assert_eq!(
        active,
        [("linux".into(), "0.6.4".into(), 1), ("unknown".into(), "0.6.3".into(), 1), ("unknown".into(), "0.6.4".into(), 1), ("windows".into(), "0.6.4".into(), 1)]
    );
    let hours: Vec<DateTime<Utc>> = sqlx::query_scalar("SELECT DISTINCT hour FROM launcher_active").fetch_all(&app.pool).await.unwrap();
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0].timestamp() % 3600, 0, "a whole hour");
}

#[sqlx::test]
async fn a_request_without_the_platform_keeps_the_one_already_known(pool: PgPool) {
    let app = TestApp::new(pool);
    let id = "11111111-2222-3333-4444-555555555555";
    let with_id = |agent: &str| Request::get("/v1/addons").header("user-agent", agent).header("x-survivorsforge-install", id).body(Body::empty()).unwrap();
    let os = || async { sqlx::query_scalar::<_, String>("SELECT os FROM launcher_active").fetch_one(&app.pool).await.unwrap() };
    // In one flush, and over two: launcher 0.7.1 sent its telemetry without the platform.
    send(&app.router, with_id("SurvivorsForge/0.7.1 (windows)")).await;
    send(&app.router, with_id("SurvivorsForge/0.7.1")).await;
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    assert_eq!(os().await, "windows");
    send(&app.router, with_id("SurvivorsForge/0.7.1")).await;
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    assert_eq!(os().await, "windows");
}

#[sqlx::test]
async fn public_api_use_is_counted_by_client_and_country(pool: PgPool) {
    let app = TestApp::new(pool);
    let call = |uri: &str, agent: Option<&str>, country: Option<&str>| {
        let mut request = Request::get(uri);
        if let Some(agent) = agent {
            request = request.header("user-agent", agent);
        }
        if let Some(country) = country {
            request = request.header("cf-ipcountry", country);
        }
        request.body(Body::empty()).unwrap()
    };
    for request in [
        call("/v1/registry", Some("MyLauncher/1.2 (+https://github.com/me/mylauncher)"), Some("de")),
        call("/v1/registry", Some("MyLauncher/1.2 (+https://github.com/me/mylauncher)"), Some("DE")),
        call("/v1/addons/lucky", Some("curl/8.0"), Some("US")),
        call("/v1/registry", None, Some("nonsense")),
        // Not public: the launcher and other routes are counted elsewhere.
        call("/v1/registry", Some("SurvivorsForge/0.6.4 (windows)"), Some("DE")),
        call("/v1/stats", Some("MyLauncher/1.2"), Some("DE")),
    ] {
        send(&app.router, request).await;
    }
    app.app_state.request_stats.flush(&app.pool).await.unwrap();
    let counted: Vec<(String, String, String, i64)> =
        sqlx::query_as("SELECT route, client, country, count FROM public_api_stats ORDER BY route, client").fetch_all(&app.pool).await.unwrap();
    assert_eq!(
        counted,
        [
            ("/v1/addons/{slug}".into(), "curl/8.0".into(), "US".into(), 1),
            ("/v1/registry".into(), "MyLauncher/1.2".into(), "DE".into(), 2),
            ("/v1/registry".into(), "none".into(), "unknown".into(), 1),
        ]
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
    send(&app.router, get("/v1/addons", "MyTool/1.0")).await;
    let limited = send(&app.router, get("/v1/addons", "MyTool/1.0")).await;
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

#[sqlx::test]
async fn only_official_builds_may_name_themselves_survivorsforge(pool: PgPool) {
    let token = forge_api::request_stats::LauncherToken { token: "build-secret".into(), from: "0.7.3".parse().unwrap() };
    let app = TestApp::with_config(pool, |c| c.launcher_token = Some(token));
    let call = |uri: &str, user_agent: &str, sent: Option<&str>| {
        let mut request = Request::get(uri).header("user-agent", user_agent);
        if let Some(sent) = sent {
            request = request.header("x-survivorsforge-build", sent);
        }
        request.body(Body::empty()).unwrap()
    };
    let official = send(&app.router, call("/v1/categories", "SurvivorsForge/0.7.3 (windows)", Some("build-secret"))).await;
    assert_eq!(official.status, StatusCode::OK);
    let spoofed = send(&app.router, call("/v1/categories", "SurvivorsForge/0.7.3 (windows)", None)).await;
    assert_eq!(spoofed.status, StatusCode::FORBIDDEN);
    assert!(spoofed.body.contains("unofficial_client"), "{}", spoofed.body);
    let wrong = send(&app.router, call("/v1/categories", "SurvivorsForge/1.0.0", Some("guess"))).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    // Launchers from before the token and clients with a name of their own pass.
    for user_agent in ["SurvivorsForge/0.7.2 (windows)", "RebornForge/1.2 (windows)"] {
        assert_eq!(send(&app.router, call("/v1/categories", user_agent, None)).await.status, StatusCode::OK, "{user_agent}");
    }
    // The self-update stays open, so a launcher can always reach the build with the token.
    let update = send(&app.router, call("/v1/github/Rezondes/SurvivorsForge/releases/latest", "SurvivorsForge/0.7.3", None)).await;
    assert_ne!(update.status, StatusCode::FORBIDDEN);
    // A spoofed request counts as another client, not as the launcher.
    let counted: Vec<(String, i16, String)> = rows(&app).await.into_iter().filter(|r| r.0 == "/v1/categories").map(|r| (r.0, r.2, r.3)).collect();
    assert!(counted.contains(&("/v1/categories".into(), 403, "other".into())), "{counted:?}");
    assert!(!counted.iter().any(|r| r.1 == 403 && r.2 == "launcher"), "{counted:?}");
}

#[sqlx::test]
async fn the_public_api_wants_a_client_name_of_its_own(pool: PgPool) {
    let app = TestApp::new(pool);
    for user_agent in ["", "curl/8.5.0", "python-requests/2.31", "Mozilla/5.0 (X11; Linux x86_64) Firefox/130.0", "MyLauncher/1.2 (+https://github.com/me/mylauncher)", "SurvivorsForge-unofficial/0.7.3 (windows)"] {
        let reply = send(&app.router, get("/v1/categories", user_agent)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{user_agent}");
        assert!(reply.body.contains("user_agent_required"), "{}", reply.body);
    }
    assert_eq!(send(&app.router, get("/v1/categories", "MyTool/1.0 (+https://github.com/me/mytool)")).await.status, StatusCode::OK);
    // Only the public routes: the login opens in a browser.
    assert_ne!(send(&app.router, get("/v1/auth/github/start", "Mozilla/5.0 Firefox/130.0")).await.status, StatusCode::FORBIDDEN);
    // The website (WEBSITE_IP) passes with any name.
    let app = TestApp::with_config(app.pool.clone(), |c| c.website_ip = Some("10.0.0.11".parse().unwrap()));
    let router = app.router.clone().layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 11], 4000))));
    assert_eq!(send(&router, get("/v1/categories", "reqwest")).await.status, StatusCode::OK);
}
