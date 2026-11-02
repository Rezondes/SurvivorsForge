use chrono::{DateTime, Duration, Utc};
use forge_api::github::{GitHubApi, GitHubClient, GitHubError};
use jsonwebtoken::EncodingKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use serde_json::json;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::AsyncWriteExt;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Generated per test run, never stored in the repo.
fn key() -> EncodingKey {
    static PEM: OnceLock<String> = OnceLock::new();
    let pem = PEM.get_or_init(|| {
        rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap().to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
    });
    EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap()
}

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z").unwrap().with_timezone(&Utc)
}

fn client(server: &MockServer, now: Arc<Mutex<DateTime<Utc>>>) -> GitHubClient {
    GitHubClient::new(&server.uri(), "42", key(), Arc::new(move || *now.lock().unwrap())).with_download_hosts(&["127.0.0.1"])
}

async fn mount_token(server: &MockServer, expires_at: DateTime<Utc>) {
    Mock::given(method("POST"))
        .and(path("/app/installations/1/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "token": "inst-token", "expires_at": expires_at })))
        .mount(server)
        .await;
}

fn repo_json() -> serde_json::Value {
    json!({ "id": 5, "full_name": "o/r", "private": false, "owner": { "login": "o" } })
}

async fn token_calls(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().iter().filter(|r| r.url.path().ends_with("/access_tokens")).count()
}

#[tokio::test]
async fn installation_token_is_cached_until_near_expiry() {
    let server = MockServer::start().await;
    mount_token(&server, t0() + Duration::minutes(60)).await;
    Mock::given(method("GET"))
        .and(path("/repositories/5"))
        .and(header("authorization", "Bearer inst-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json()))
        .mount(&server)
        .await;
    let now = Arc::new(Mutex::new(t0()));
    let gh = client(&server, now.clone());

    assert_eq!(gh.repo(Some(1), 5).await.unwrap().full_name, "o/r");
    gh.repo(Some(1), 5).await.unwrap();
    assert_eq!(token_calls(&server).await, 1);

    *now.lock().unwrap() = t0() + Duration::minutes(56);
    gh.repo(Some(1), 5).await.unwrap();
    assert_eq!(token_calls(&server).await, 2, "renewed 5 minutes before expiry");
}

#[tokio::test]
async fn etag_304_returns_cached_body() {
    let server = MockServer::start().await;
    mount_token(&server, t0() + Duration::hours(1)).await;
    Mock::given(method("GET"))
        .and(path("/repositories/5"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repositories/5"))
        .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"v1\"").set_body_json(repo_json()))
        .with_priority(2)
        .mount(&server)
        .await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));
    let first = gh.repo(Some(1), 5).await.unwrap();
    let second = gh.repo(Some(1), 5).await.unwrap();
    assert_eq!(first, second);
    let conditional = server.received_requests().await.unwrap().iter().filter(|r| r.headers.contains_key("if-none-match")).count();
    assert_eq!(conditional, 1);
}

#[tokio::test]
async fn rate_limit_and_not_found_errors() {
    let server = MockServer::start().await;
    mount_token(&server, t0() + Duration::hours(1)).await;
    Mock::given(method("GET"))
        .and(path("/repositories/1"))
        .respond_with(
            ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0").insert_header("x-ratelimit-reset", "1767272400"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/repositories/2")).respond_with(ResponseTemplate::new(429)).mount(&server).await;
    Mock::given(method("GET")).and(path("/repositories/3")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    Mock::given(method("GET")).and(path("/repositories/4")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));

    match gh.repo(Some(1), 1).await {
        Err(GitHubError::RateLimited { reset_at }) => assert_eq!(reset_at.unwrap().timestamp(), 1767272400),
        other => panic!("{other:?}"),
    }
    assert!(matches!(gh.repo(Some(1), 2).await, Err(GitHubError::RateLimited { .. })));
    assert_eq!(gh.repo(Some(1), 3).await, Err(GitHubError::NotFound));
    assert_eq!(gh.repo(Some(1), 4).await, Err(GitHubError::Http(403)), "403 with requests left is no rate limit");
}

#[tokio::test]
async fn installation_lookup_uses_app_jwt_and_404_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 77 })))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/repos/o/none/installation")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    let gh = client(&server, Arc::new(Mutex::new(Utc::now())));
    assert_eq!(gh.installation_for_repo("o/r").await.unwrap(), Some(77));
    assert_eq!(gh.installation_for_repo("o/none").await.unwrap(), None);
    let auth = server.received_requests().await.unwrap()[0].headers["authorization"].to_str().unwrap().to_string();
    assert_eq!(auth.split('.').count(), 3, "a JWT: {auth}");
}

#[tokio::test]
async fn releases_beyond_the_first_page_are_read() {
    // GitHub sends at most 100 per page; a sync must not take older releases for deleted.
    let server = MockServer::start().await;
    let page = |from: i64, n: i64| -> Vec<serde_json::Value> {
        (from..from + n).map(|i| json!({ "id": i, "tag_name": format!("v{i}"), "draft": false, "prerelease": false, "assets": [] })).collect()
    };
    for (number, body) in [("1", page(0, 100)), ("2", page(100, 3))] {
        Mock::given(method("GET"))
            .and(path("/repositories/9/releases"))
            .and(query_param("page", number))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    let gh = client(&server, Arc::new(Mutex::new(Utc::now())));
    assert_eq!(gh.releases(None, 9).await.unwrap().len(), 103);
}

#[tokio::test]
async fn a_read_token_lifts_the_limit_without_installation() {
    // GitHub allows 60 requests per hour without a token and 5000 with one.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/o/repos"))
        .and(header("authorization", "Bearer read-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([repo_json()])))
        .expect(1)
        .mount(&server)
        .await;
    let gh = client(&server, Arc::new(Mutex::new(Utc::now()))).with_read_token(Some("read-token".into()));
    assert_eq!(gh.user_repos("o").await.unwrap().len(), 1);
}

#[tokio::test]
async fn user_repos_need_no_installation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/o/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([repo_json()])))
        .mount(&server)
        .await;
    let gh = client(&server, Arc::new(Mutex::new(Utc::now())));
    let repos = gh.user_repos("o").await.unwrap();
    assert_eq!(repos.iter().map(|r| r.full_name.as_str()).collect::<Vec<_>>(), ["o/r"]);
    assert!(!server.received_requests().await.unwrap()[0].headers.contains_key("authorization"), "public data, no token");
}

#[tokio::test]
async fn download_rejects_large_content_length() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/a.zip")).respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 100])).mount(&server).await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));
    let url = format!("{}/a.zip", server.uri());
    assert_eq!(gh.download(&url, 99).await, Err(GitHubError::TooLarge));
    assert_eq!(gh.download(&url, 100).await.unwrap().len(), 100);
}

#[tokio::test]
async fn download_stops_at_max_bytes_without_content_length() {
    // A chunked answer has no Content-Length; the limit must hold while reading.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        for _ in 0..100 {
            if socket.write_all(b"400\r\n").await.is_err() {
                return;
            }
            let _ = socket.write_all(&[b'x'; 0x400]).await;
            let _ = socket.write_all(b"\r\n").await;
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });
    let server = MockServer::start().await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));
    assert_eq!(gh.download(&format!("http://{addr}/big"), 4096).await, Err(GitHubError::TooLarge));
}

#[tokio::test]
async fn oauth_user_trades_code_and_reads_user() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(wiremock::matchers::body_partial_json(json!({ "code": "abc", "code_verifier": "ver", "client_secret": "sec" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "gho_user", "expires_in": 28800, "refresh_token": "ghr_1", "refresh_token_expires_in": 15897600 })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "error": "bad_verification_code" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", "Bearer gho_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 9, "login": "nine", "avatar_url": "a" })))
        .mount(&server)
        .await;
    let gh = client(&server, Arc::new(Mutex::new(t0()))).with_oauth(forge_api::github::OAuthApp {
        oauth_url: server.uri(),
        client_id: "cid".into(),
        client_secret: "sec".into(),
    });
    let (user, token) = gh.oauth_user("abc", "ver", "https://api.test/cb").await.unwrap();
    assert_eq!((user.id, user.login.as_str()), (9, "nine"));
    assert_eq!((token.access_token.as_str(), token.refresh_token.as_deref()), ("gho_user", Some("ghr_1")), "kept for writes as the user");
    assert_eq!(token.expires_at, Some(t0() + chrono::Duration::hours(8)));
    assert!(!format!("{token:?}").contains("gho_user"), "Debug never prints the token");
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(wiremock::matchers::body_partial_json(json!({ "grant_type": "refresh_token", "refresh_token": "ghr_1", "client_secret": "sec" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "gho_new", "expires_in": 28800, "refresh_token": "ghr_2" })))
        .with_priority(1)
        .mount(&server)
        .await;
    let renewed = gh.refresh_user_token("ghr_1").await.unwrap();
    assert_eq!((renewed.access_token.as_str(), renewed.refresh_token.as_deref()), ("gho_new", Some("ghr_2")));
    assert_eq!(gh.oauth_user("wrong", "ver", "https://api.test/cb").await.map(|(u, _)| u), Err(GitHubError::Auth("bad_verification_code".into())));
}

#[tokio::test]
async fn download_refuses_foreign_hosts_and_redirects() {
    let server = MockServer::start().await;
    let foreign = format!("http://localhost:{}/evil.zip", server.address().port());
    Mock::given(method("GET"))
        .and(path("/redirect.zip"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", foreign.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/evil.zip")).respond_with(ResponseTemplate::new(200).set_body_bytes(b"x".to_vec())).mount(&server).await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));
    assert!(matches!(gh.download(&foreign, 100).await, Err(GitHubError::ForbiddenHost(_))), "localhost is not a listed host");
    let redirected = gh.download(&format!("{}/redirect.zip", server.uri()), 100).await;
    assert!(matches!(redirected, Err(GitHubError::ForbiddenHost(_))), "{redirected:?}");
}

#[test]
fn download_hosts_are_githubs_over_https() {
    let hosts: Vec<String> = forge_api::github::DOWNLOAD_HOSTS.iter().map(|h| h.to_string()).collect();
    assert!(forge_api::github::download_allowed("https://github.com/o/r/releases/download/v1/a.zip", &hosts));
    assert!(forge_api::github::download_allowed("https://codeload.github.com/o/r/zip/refs/tags/v1", &hosts));
    assert!(!forge_api::github::download_allowed("http://github.com/o/r/a.zip", &hosts), "no plain http");
    assert!(!forge_api::github::download_allowed("https://github.com.evil.test/a.zip", &hosts));
    assert!(!forge_api::github::download_allowed("https://evil.test/a.zip", &hosts));
}

#[tokio::test]
async fn without_installation_reads_public_api_without_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json()))
        .mount(&server)
        .await;
    let gh = client(&server, Arc::new(Mutex::new(t0())));
    assert_eq!(gh.repo_by_name(None, "o/r").await.unwrap().id, 5);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "no token request");
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn issue_rights_come_fresh_from_the_installation() {
    let server = MockServer::start().await;
    let now = Arc::new(Mutex::new(t0()));
    let gh = client(&server, now.clone());
    Mock::given(method("GET"))
        .and(path("/app/installations/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "permissions": { "contents": "read" } })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/app/installations/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "permissions": { "contents": "read", "issues": "write" } })))
        .mount(&server)
        .await;
    assert!(!gh.issues_writable(1).await.unwrap());
    assert!(!gh.issues_writable(1).await.unwrap(), "kept for a minute");
    *now.lock().unwrap() += chrono::Duration::seconds(61);
    assert!(gh.issues_writable(1).await.unwrap(), "the owner accepted the permission meanwhile");
}
