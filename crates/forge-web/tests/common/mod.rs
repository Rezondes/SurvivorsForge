#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use forge_web::app::{AppState, router};
use forge_web::config::Config;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const WEB: &str = "https://web.test";

/// The website against a mocked API, with a cookie jar like a browser's.
pub struct TestWeb {
    pub router: Router,
    pub api: MockServer,
    pub cookies: HashMap<String, String>,
}

pub struct Page {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Page {
    pub fn location(&self) -> &str {
        self.headers.get("location").and_then(|v| v.to_str().ok()).unwrap_or_default()
    }

    pub fn set_cookies(&self) -> Vec<String> {
        self.headers.get_all("set-cookie").iter().map(|v| v.to_str().unwrap().to_string()).collect()
    }
}

impl TestWeb {
    pub async fn new() -> TestWeb {
        let api = MockServer::start().await;
        let router = router(AppState::new(Config::for_tests(&api.uri())));
        TestWeb { router, api, cookies: HashMap::new() }
    }

    pub async fn request(&mut self, request: Request<Body>) -> Page {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let page = Page { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() };
        for cookie in page.set_cookies() {
            let pair = cookie.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() || cookie.contains("Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_string(), value.to_string());
            }
        }
        page
    }

    fn cookie_header(&self) -> String {
        self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    pub async fn get(&mut self, uri: &str) -> Page {
        let request = Request::get(uri).header("cookie", self.cookie_header()).body(Body::empty()).unwrap();
        self.request(request).await
    }

    /// A form post from our own origin with the visitor's CSRF token.
    pub async fn post_form(&mut self, uri: &str, fields: &[(&str, &str)]) -> Page {
        let token = self.csrf().await;
        let mut pairs: Vec<(String, String)> = fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        pairs.push(("csrf".into(), token));
        let body = reqwest::Url::parse_with_params("http://x/", &pairs).unwrap().query().unwrap_or_default().to_string();
        let request = Request::post(uri)
            .header("cookie", self.cookie_header())
            .header("origin", WEB)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.request(request).await
    }

    /// A form with file inputs, posted from our own origin; the CSRF token travels as the field
    /// `csrf` like in the browser. Files are (field, file name, bytes).
    pub async fn post_multipart(&mut self, uri: &str, fields: &[(&str, &str)], files: &[(&str, &str, &[u8])]) -> Page {
        let token = self.csrf().await;
        self.post_multipart_with(uri, &token, fields, files).await
    }

    pub async fn post_multipart_with(&mut self, uri: &str, csrf: &str, fields: &[(&str, &str)], files: &[(&str, &str, &[u8])]) -> Page {
        const BOUNDARY: &str = "forge-test-boundary";
        fn part(body: &mut Vec<u8>, head: &str, data: &[u8]) {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n{head}\r\n\r\n").as_bytes());
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        let mut body = Vec::new();
        part(&mut body, "Content-Disposition: form-data; name=\"csrf\"", csrf.as_bytes());
        for (name, value) in fields {
            part(&mut body, &format!("Content-Disposition: form-data; name=\"{name}\""), value.as_bytes());
        }
        for (name, file, data) in files {
            part(&mut body, &format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: image/png"), data);
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let request = Request::post(uri)
            .header("cookie", self.cookie_header())
            .header("origin", WEB)
            .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
            .body(Body::from(body))
            .unwrap();
        self.request(request).await
    }

    /// The CSRF token as the page embeds it.
    pub async fn csrf(&mut self) -> String {
        let page = self.get("/").await;
        let marker = "\"X-CSRF-Token\": \"";
        let start = page.body.find(marker).expect("token in page") + marker.len();
        page.body[start..].split('"').next().unwrap().to_string()
    }

    /// Mocks `/v1/me` for `token` and logs in through the callback.
    pub async fn login_as(&mut self, token: &str, me: Value) {
        Mock::given(method("POST"))
            .and(path("/v1/auth/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": token, "expires_at": "2030-01-01T00:00:00Z" })))
            .mount(&self.api)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(me))
            .mount(&self.api)
            .await;
        self.get("/login").await;
        let page = self.get("/auth/callback?code=abc").await;
        assert_eq!(page.status, StatusCode::SEE_OTHER, "{}", page.body);
    }
}

/// `/v1/me` as the API answers it, with the permissions of the default `ADMIN_STATS_ROLES`.
pub fn me(login: &str, role: &str) -> Value {
    use forge_core::roles::{self, Role};
    let permissions = roles::permissions(role.parse().unwrap_or(Role::User), &[Role::Admin, Role::Moderator]);
    json!({ "id": 1, "github_id": 1, "login": login, "avatar_url": "https://avatars.githubusercontent.com/u/1", "role": role, "permissions": permissions })
}
