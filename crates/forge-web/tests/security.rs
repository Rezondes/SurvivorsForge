mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestWeb, WEB};
use forge_web::app::{AppState, router};
use forge_web::config::Config;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROTECTED: &[&str] = &[
    "/me/addons",
    "/addons/new",
    "/addons/lucky/edit",
    "/addons/lucky/images",
    "/addons/lucky/members",
    "/addons/lucky/dependencies",
    "/addons/lucky/versions/v1/edit",
    "/addons/lucky/delete",
    "/admin",
    "/admin/review",
    "/admin/reports",
    "/admin/users",
    "/admin/addons/new",
    "/admin/addons/lucky",
    "/admin/audit",
    "/admin/list/categories",
    "/packs/new",
];

#[tokio::test]
async fn protected_pages_send_anonymous_visitors_to_the_login_without_asking_the_api() {
    let mut web = TestWeb::new().await;
    for route in PROTECTED {
        let page = web.get(route).await;
        assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/login"), "{route}");
        assert!(page.body.is_empty(), "{route} leaks a body");
    }
    assert!(web.api.received_requests().await.unwrap().is_empty(), "no API data fetched for anonymous visitors");
}

#[tokio::test]
async fn preview_needs_a_login() {
    let mut web = TestWeb::new().await;
    let page = web.post_form("/addons/lucky/preview", &[("description_md", "# Hi")]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/login"));
}

#[tokio::test]
async fn pages_are_never_cached_but_static_files_are() {
    let mut web = TestWeb::new().await;
    assert_eq!(web.get("/").await.headers["cache-control"], "no-store");
    assert_eq!(web.get("/me/addons").await.headers["cache-control"], "no-store");
    assert_eq!(web.get("/static/app.css").await.headers["cache-control"], "public, max-age=86400");
}

fn web_with(api: &MockServer, limit: u32, proxies: &str) -> axum::Router {
    let mut config = Config::for_tests(&api.uri());
    config.rate_limit_per_minute = limit;
    config.trusted_proxies = proxies.split(',').filter(|p| !p.is_empty()).map(|p| p.parse().unwrap()).collect();
    router(AppState::new(config))
}

fn get(uri: &str, forwarded_for: &str) -> Request<Body> {
    Request::get(uri).header("x-forwarded-for", forwarded_for).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn too_many_requests_get_429_per_client() {
    let api = MockServer::start().await;
    // Tests have no socket peer: the peer is 0.0.0.0, trusted here so X-Forwarded-For picks the client.
    let web = web_with(&api, 2, "0.0.0.0");
    for _ in 0..2 {
        assert_eq!(web.clone().oneshot(get("/static/app.css", "1.1.1.1")).await.unwrap().status(), StatusCode::OK);
    }
    assert_eq!(web.clone().oneshot(get("/static/app.css", "1.1.1.1")).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(web.clone().oneshot(get("/static/app.css", "2.2.2.2")).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn the_api_learns_the_client_ip() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/addons"))
        .and(header("x-forwarded-for", "1.2.3.4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": [], "total": 0 })))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("GET")).and(path("/v1/categories")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([]))).mount(&api).await;
    let web = web_with(&api, 100, "0.0.0.0");
    let response = web.oneshot(Request::get("/addons").header("x-forwarded-for", "1.2.3.4").header("referer", WEB).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn grafana_auth_admin_ok() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", common::me("root", "admin")).await;
    let page = web.get("/auth/grafana").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.headers["x-grafana-user"], "root");
}

#[tokio::test]
async fn grafana_auth_redirects_anonymous() {
    let mut web = TestWeb::new().await;
    let page = web.get("/auth/grafana").await;
    assert!(page.status.is_redirection(), "{}", page.status);
    assert_eq!(page.location(), "/login");
    assert!(page.headers.get("x-grafana-user").is_none());
}

#[tokio::test]
async fn grafana_auth_forbids_moderator() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", common::me("mia", "moderator")).await;
    let page = web.get("/auth/grafana").await;
    assert_eq!(page.status, StatusCode::FORBIDDEN, "may see the admin statistics, not Grafana");
    assert!(page.headers.get("x-grafana-user").is_none());
}
