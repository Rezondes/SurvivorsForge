#![allow(dead_code)]

pub mod fake_github;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::{DateTime, Utc};
use fake_github::FakeGitHub;
use forge_api::app::{AppState, router};
use forge_api::config::Config;
use forge_api::github::types::User;
use http_body_util::BodyExt;
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

pub const RETURN_TO: &str = "https://web.test/auth/callback";

pub struct TestApp {
    pub router: Router,
    pub app_state: AppState,
    pub pool: PgPool,
    pub github: Arc<FakeGitHub>,
    pub now: Arc<Mutex<DateTime<Utc>>>,
}

impl TestApp {
    pub fn new(pool: PgPool) -> TestApp {
        TestApp::with_config(pool, |_| {})
    }

    pub fn with_config(pool: PgPool, change: impl FnOnce(&mut Config)) -> TestApp {
        let mut config = Config::for_tests("postgres://unused");
        change(&mut config);
        let github = Arc::new(FakeGitHub::default());
        let now = Arc::new(Mutex::new(Utc::now()));
        let clock = {
            let now = now.clone();
            Arc::new(move || *now.lock().unwrap())
        };
        let state = AppState { pool: pool.clone(), config: Arc::new(config), github: github.clone(), clock, request_stats: Default::default(), sender_key: Default::default() };
        TestApp { router: router(state.clone()), app_state: state, pool, github, now }
    }

    /// The state the router runs with, for calling jobs and services directly.
    pub fn state(&self) -> AppState {
        self.app_state.clone()
    }

    pub fn advance(&self, by: chrono::Duration) {
        *self.now.lock().unwrap() += by;
    }

    pub async fn get(&self, uri: &str, token: Option<&str>) -> Reply {
        self.call("GET", uri, token, None).await
    }

    pub async fn post(&self, uri: &str, token: Option<&str>, body: serde_json::Value) -> Reply {
        self.call("POST", uri, token, Some(body)).await
    }

    pub async fn patch(&self, uri: &str, token: Option<&str>, body: serde_json::Value) -> Reply {
        self.call("PATCH", uri, token, Some(body)).await
    }

    pub async fn put(&self, uri: &str, token: Option<&str>, body: serde_json::Value) -> Reply {
        self.call("PUT", uri, token, Some(body)).await
    }

    pub async fn delete(&self, uri: &str, token: Option<&str>) -> Reply {
        self.call("DELETE", uri, token, None).await
    }

    pub async fn call(&self, method: &str, uri: &str, token: Option<&str>, body: Option<serde_json::Value>) -> Reply {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => request.header("content-type", "application/json").body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        };
        send(&self.router, request.unwrap()).await
    }

    /// Runs the whole GitHub login for this user and returns the bearer token.
    pub async fn login(&self, github_id: i64, login: &str) -> String {
        let code = format!("gh-code-{github_id}-{login}");
        self.github.data().oauth_users.insert(
            code.clone(),
            User { id: github_id, login: login.into(), avatar_url: format!("https://avatars.test/{github_id}") },
        );
        let state = self.start_login().await;
        let reply = self.get(&format!("/v1/auth/github/callback?state={state}&code={code}"), None).await;
        let code = query_param(&reply.location(), "code").unwrap_or_else(|| panic!("no code: {}", reply.location()));
        let reply = self.post("/v1/auth/exchange", None, serde_json::json!({ "code": code, "return_to": RETURN_TO })).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        reply.json()["token"].as_str().unwrap().to_string()
    }

    /// `/v1/auth/github/start` for `RETURN_TO`; the `state` GitHub would echo back.
    pub async fn start_login(&self) -> String {
        let reply = self.get(&format!("/v1/auth/github/start?return_to={}", urlencode(RETURN_TO)), None).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        query_param(&reply.location(), "state").unwrap()
    }

    /// Logs in `login` (GitHub id `github_id`) as admin of a new repo `login/slug` with id
    /// `github_id * 100`, publishes the addon `slug` with folder `Slug` and returns the token.
    pub async fn publish(&self, github_id: i64, login: &str, slug: &str) -> String {
        let repo = format!("{login}/{slug}");
        self.github.add_repo(github_id * 100, &repo, login);
        let token = self.login(github_id, login).await;
        let body = addon_body(slug, &repo, &[&capitalized(slug)]);
        let reply = self.post("/v1/addons", Some(&token), body).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        token
    }

    pub async fn user_id(&self, github_id: i64) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE github_id = $1").bind(github_id).fetch_one(&self.pool).await.unwrap()
    }
}

pub fn addon_body(slug: &str, repo: &str, folders: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "slug": slug, "name": format!("Addon {slug}"), "repo": repo, "source_type": "release",
        "asset_pattern": format!("{slug}-v*.zip"), "category": "Unit Frames", "summary": "Short.", "folders": folders,
    })
}

pub fn capitalized(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn urlencode(s: &str) -> String {
    reqwest::Url::parse_with_params("http://x/", &[("v", s)]).unwrap().query().unwrap()[2..].to_string()
}

pub fn query_param(url: &str, key: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

pub fn state(pool: PgPool) -> AppState {
    let clock = Arc::new(Utc::now);
    AppState {
        pool,
        config: Arc::new(Config::for_tests("postgres://unused")),
        github: Arc::new(FakeGitHub::default()),
        clock,
        request_stats: Default::default(),
        sender_key: Default::default(),
    }
}

pub fn app(pool: PgPool) -> Router {
    router(state(pool))
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not json ({e}): {}", self.body))
    }

    pub fn location(&self) -> String {
        self.headers.get("location").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
    }

    pub fn error(&self) -> String {
        self.json()["error"].as_str().unwrap_or_default().to_string()
    }
}

pub async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() }
}

pub async fn get(app: &Router, uri: &str) -> Reply {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}
