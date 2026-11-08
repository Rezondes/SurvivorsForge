mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ASSETS: &str = include_str!("../../../tests/fixtures/github/assets_survivor_ui.html");
const REFS: &[u8] = include_bytes!("../../../tests/fixtures/github/refs_bordoui.txt");

async fn app_with_github(pool: PgPool) -> (TestApp, MockServer) {
    let github = MockServer::start().await;
    let uri = github.uri();
    let app = TestApp::with_config(pool, |c| {
        c.github_web_url = uri;
        c.cache_seconds = 60;
    });
    // The proxy serves only repos of the registry: these are addons.
    for (n, repo) in ["morderous/survivor-ui", "o/r", "threenox/BordoUI", "nope/missing", "down/repo"].iter().enumerate() {
        let slug = format!("addon{n}");
        app.publish(100 + n as i64, &format!("owner{n}"), &slug).await;
        sqlx::query("UPDATE addons SET repo_full_name = $1 WHERE slug = $2").bind(repo).bind(&slug).execute(&app.pool).await.unwrap();
    }
    (app, github)
}

#[sqlx::test]
async fn latest_release_with_assets_and_cached(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    let location = format!("{}/morderous/survivor-ui/releases/tag/v3.0.36", github.uri());
    Mock::given(method("GET"))
        .and(path("/morderous/survivor-ui/releases/latest"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str()))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/morderous/survivor-ui/releases/expanded_assets/v3.0.36"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ASSETS))
        .expect(1)
        .mount(&github)
        .await;

    for _ in 0..2 {
        let reply = app.get("/v1/github/morderous/survivor-ui/releases/latest", None).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let release = reply.json();
        assert_eq!(release["tag"], "v3.0.36");
        let zip = format!("{}/morderous/survivor-ui/releases/download/v3.0.36/SurvivorUI-v3.0.36.zip", github.uri());
        assert!(release["assets"].as_array().unwrap().iter().any(|a| a["url"] == zip.as_str()), "{release}");
    }
    // `expect(1)` on both mocks: the second answer came from the cache.
}

#[sqlx::test]
async fn no_release_is_null(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    let location = format!("{}/o/r/releases", github.uri());
    Mock::given(path("/o/r/releases/latest")).respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str())).mount(&github).await;
    let reply = app.get("/v1/github/o/r/releases/latest", None).await;
    assert_eq!((reply.status, reply.json()), (StatusCode::OK, json!(null)));
}

#[sqlx::test]
async fn refs_give_head_branches_and_sorted_tags(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(path("/threenox/BordoUI.git/info/refs"))
        .and(query_param("service", "git-upload-pack"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(REFS))
        .mount(&github)
        .await;
    let refs = app.get("/v1/github/threenox/BordoUI/refs", None).await.json();
    let head = refs["head"].as_str().unwrap();
    assert_eq!(refs["branches"][head].as_str().unwrap().len(), 40);
    assert!(refs["tags"][0]["name"].as_str().unwrap().starts_with('v'));
}

#[sqlx::test]
async fn proxy_serves_the_launcher_repo(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(path("/Rezondes/SurvivorsForge/releases/latest"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/Rezondes/SurvivorsForge/releases", github.uri()).as_str()))
        .expect(1)
        .mount(&github)
        .await;
    let reply = app.get("/v1/github/Rezondes/SurvivorsForge/releases/latest", None).await;
    assert_eq!(reply.status, StatusCode::OK, "the launcher's self-update: {}", reply.body);
}

#[sqlx::test]
async fn proxy_refuses_other_repos_without_calling_github(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&github).await;
    for route in ["/v1/github/someone/else/releases/latest", "/v1/github/someone/else/releases/v1", "/v1/github/someone/else/refs"] {
        let reply = app.get(route, None).await;
        assert_eq!((reply.status, reply.error()), (StatusCode::NOT_FOUND, "not_found".to_string()), "{route}");
    }
}

#[sqlx::test]
async fn proxy_refuses_blocked_and_deleted_addons(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&github).await;
    sqlx::query("UPDATE addons SET blocked_at = now() WHERE repo_full_name = 'o/r'").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE addons SET deleted_at = now() WHERE repo_full_name = 'down/repo'").execute(&app.pool).await.unwrap();
    assert_eq!(app.get("/v1/github/o/r/refs", None).await.status, StatusCode::NOT_FOUND, "blocked");
    assert_eq!(app.get("/v1/github/down/repo/refs", None).await.status, StatusCode::NOT_FOUND, "deleted");
}

#[sqlx::test]
async fn proxy_repo_match_ignores_case(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(path("/MORDEROUS/Survivor-UI/releases/expanded_assets/v1")).respond_with(ResponseTemplate::new(200).set_body_string("")).mount(&github).await;
    let reply = app.get("/v1/github/MORDEROUS/Survivor-UI/releases/v1", None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

#[sqlx::test]
async fn bad_names_and_missing_repos(pool: PgPool) {
    let (app, github) = app_with_github(pool).await;
    Mock::given(path("/nope/missing/releases/expanded_assets/v1")).respond_with(ResponseTemplate::new(401)).mount(&github).await;
    Mock::given(path("/down/repo.git/info/refs")).respond_with(ResponseTemplate::new(500)).mount(&github).await;

    assert_eq!(app.get("/v1/github/a%20b/c/refs", None).await.error(), "invalid_repo");
    assert_eq!(app.get("/v1/github/../c/refs", None).await.error(), "invalid_repo");
    let missing = app.get("/v1/github/nope/missing/releases/v1", None).await;
    assert_eq!((missing.status, missing.error()), (StatusCode::NOT_FOUND, "not_found".to_string()));
    let down = app.get("/v1/github/down/repo/refs", None).await;
    assert_eq!((down.status, down.error()), (StatusCode::BAD_GATEWAY, "github_unavailable".to_string()));
}
