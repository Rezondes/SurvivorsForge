//! The real GitHub client against a mock server: what it counts and which quota state it keeps.

use chrono::{DateTime, Utc};
use forge_api::github::usage::{ORIGIN, USAGE};
use forge_api::github::{GitHubApi, GitHubClient};
use jsonwebtoken::EncodingKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn key() -> EncodingKey {
    let pem = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap().to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
    EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap()
}

#[sqlx::test]
async fn counts_requests_by_origin_and_keeps_quotas(pool: PgPool) {
    let server = MockServer::start().await;
    let quota = |remaining: &str| {
        ResponseTemplate::new(200)
            .insert_header("x-ratelimit-limit", "5000")
            .insert_header("x-ratelimit-used", "9")
            .insert_header("x-ratelimit-remaining", remaining)
            .insert_header("x-ratelimit-reset", "1793409297")
            .insert_header("x-ratelimit-resource", "core")
    };
    let repo = json!({ "id": 5, "full_name": "o/r", "private": false, "owner": { "login": "o" } });
    Mock::given(method("GET")).and(path("/repositories/5")).respond_with(quota("4991").set_body_json(repo)).mount(&server).await;
    Mock::given(method("GET")).and(path("/repositories/5/tags")).respond_with(quota("4990").set_body_json(json!([]))).mount(&server).await;
    let now: DateTime<Utc> = "2026-10-31T10:30:00Z".parse().unwrap();
    let client = GitHubClient::new(&server.uri(), "42", key(), Arc::new(move || now)).with_read_token(Some("read".into()));

    ORIGIN.scope("job sync_addon".into(), async {
        client.repo(None, 5).await.unwrap();
        client.tags(None, 5).await.unwrap();
    })
    .await;
    ORIGIN.scope("GET /v1/github/{owner}/{repo}/refs".into(), client.repo(None, 5)).await.unwrap();
    client.repo(None, 5).await.unwrap();
    USAGE.flush(&pool).await.unwrap();

    let rows: Vec<(DateTime<Utc>, String, String, String, i16, i64)> =
        sqlx::query_as("SELECT hour, origin, endpoint, quota, status, count FROM github_request_stats ORDER BY origin, endpoint").fetch_all(&pool).await.unwrap();
    let hour: DateTime<Utc> = "2026-10-31T10:00:00Z".parse().unwrap();
    let row = |origin: &str, endpoint: &str, count: i64| (hour, origin.to_string(), endpoint.to_string(), "read_token".to_string(), 200i16, count);
    assert_eq!(rows, [
        row("GET /v1/github/{owner}/{repo}/refs", "/repositories/{id}", 1),
        row("job sync_addon", "/repositories/{id}", 1),
        row("job sync_addon", "/repositories/{id}/tags", 1),
        row("other", "/repositories/{id}", 1),
    ]);

    let quota: (String, String, i64, i64, i64, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT quota, resource, \"limit\", used, remaining, reset_at FROM github_quota").fetch_one(&pool).await.unwrap();
    assert_eq!(quota, ("read_token".into(), "core".into(), 5000, 9, 4991, Some("2026-10-31T01:14:57Z".parse().unwrap())), "the last answer wins");
}
