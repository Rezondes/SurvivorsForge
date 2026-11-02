mod common;

use axum::http::StatusCode;
use common::fake_github::fake_issue;
use common::{RETURN_TO, TestApp, query_param};
use forge_api::github::types::User;
use serde_json::json;
use sqlx::PgPool;

const REPO: &str = "alice/lucky";

fn user(id: i64, login: &str) -> User {
    User { id, login: login.into(), avatar_url: format!("https://avatars.test/{id}") }
}

/// Alice's addon `lucky` with the Issues tab on, and Bob logged in; Bob's session and GitHub token.
async fn setup(pool: PgPool) -> (TestApp, String, String) {
    let app = TestApp::new(pool);
    let alice = app.publish(1, "alice", "lucky").await;
    let on = app.patch("/v1/addons/lucky", Some(&alice), json!({ "issues_enabled": true })).await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    let bob = app.login(2, "bob").await;
    (app, bob, "gho-2".into())
}

#[sqlx::test]
async fn until_the_owner_turns_issues_on_only_the_way_to_github_is_shown(pool: PgPool) {
    let app = TestApp::new(pool);
    let alice = app.publish(1, "alice", "lucky").await;
    let bob = app.login(2, "bob").await;
    add_issue(&app, 6, "Crash", &user(2, "bob"));
    let list = app.get("/v1/addons/lucky/issues", None).await.json();
    assert_eq!(list["issues_here"], false, "{list}");
    assert_eq!(list["new_issue_url"], "https://github.com/alice/lucky/issues/new");
    assert!(list["issues"].is_null(), "no issues read from GitHub");
    assert_eq!(app.get("/v1/addons/lucky/issues/6", None).await.error(), "issues_off");
    let body = json!({ "kind": "bug", "title": "Crash", "body": "Steps" });
    let refused = app.call_as("POST", "/v1/addons/lucky/issues", Some(&bob), Some("gho-2"), Some(body)).await;
    assert_eq!((refused.status, refused.error().as_str()), (StatusCode::FORBIDDEN, "issues_off"));
    assert_eq!(app.github.data().issues[REPO].len(), 1, "nothing reached GitHub");

    assert_eq!(app.patch("/v1/addons/lucky", Some(&bob), json!({ "issues_enabled": true })).await.status, StatusCode::FORBIDDEN, "only the owner");
    app.patch("/v1/addons/lucky", Some(&alice), json!({ "issues_enabled": true })).await;
    assert_eq!(app.get("/v1/addons/lucky/issues", None).await.json()["issues"][0]["number"], 6);
}

fn add_issue(app: &TestApp, number: i64, title: &str, author: &User) {
    app.github.data().issues.entry(REPO.into()).or_default().push(fake_issue(REPO, number, title, "Body", author));
}

#[sqlx::test]
async fn exchange_hands_out_the_github_token_once(pool: PgPool) {
    let app = TestApp::new(pool);
    app.github.data().oauth_users.insert("c".into(), user(5, "eve"));
    let state = app.start_login().await;
    let reply = app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await;
    let code = query_param(&reply.location(), "code").unwrap();
    let body = json!({ "code": code, "return_to": RETURN_TO, "github_token": true });
    let answer = app.post("/v1/auth/exchange", None, body.clone()).await.json();
    assert_eq!(answer["github_token"]["access_token"], "gho-5", "{answer}");
    assert_eq!(answer["github_token"]["refresh_token"], "ghr-gho-5");
    assert_eq!(app.post("/v1/auth/exchange", None, body).await.error(), "invalid_code", "the code works once");

    // Clients that do not ask (the launcher) never see it.
    let token = app.login(6, "frank").await;
    assert!(!token.is_empty());
    let mine: String = sqlx::query_scalar("SELECT string_agg(login::text, ',') FROM users").fetch_one(&app.pool).await.unwrap();
    assert!(!mine.contains("gho"), "nothing about the token in the database");
}

#[sqlx::test]
async fn refresh_gives_a_new_github_token_for_the_own_account_only(pool: PgPool) {
    let (app, bob, _) = setup(pool).await;
    let reply = app.post("/v1/auth/github/refresh", Some(&bob), json!({ "refresh_token": "ghr-gho-2" })).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["access_token"], "gho2-2");
    // Alice's refresh token in Bob's session.
    let reply = app.post("/v1/auth/github/refresh", Some(&bob), json!({ "refresh_token": "ghr-gho-1" })).await;
    assert_eq!((reply.status, reply.error()), (StatusCode::FORBIDDEN, "github_reauth".into()));
    let dead = app.post("/v1/auth/github/refresh", Some(&bob), json!({ "refresh_token": "nope" })).await;
    assert_eq!(dead.error(), "github_reauth");
    assert_eq!(app.post("/v1/auth/github/refresh", None, json!({ "refresh_token": "ghr-gho-2" })).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn lists_issues_without_pull_requests(pool: PgPool) {
    let (app, _, _) = setup(pool).await;
    let bob = user(2, "bob");
    add_issue(&app, 1, "Crash on load", &bob);
    add_issue(&app, 2, "Dark mode", &user(3, "carol"));
    add_issue(&app, 3, "Old bug", &bob);
    {
        let mut data = app.github.data();
        let all = data.issues.get_mut(REPO).unwrap();
        all[0].labels.push(forge_api::github::types::Label { name: "bug".into() });
        all[2].state = "closed".into();
        let mut pr = fake_issue(REPO, 4, "A pull request", "", &bob);
        pr.pull_request = Some(json!({ "url": "x" }));
        all.push(pr);
    }
    let list = app.get("/v1/addons/lucky/issues", None).await.json();
    assert_eq!(list["counts"], json!({ "open": 2, "closed": 1, "all": 3 }), "{list}");
    assert_eq!((list["issues_enabled"].clone(), list["can_write"].clone()), (json!(true), json!(true)));
    let titles: Vec<&str> = list["issues"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(titles.len(), 2, "open ones by default: {titles:?}");
    assert!(titles.contains(&"Crash on load") && !titles.contains(&"A pull request"));
    let crash = list["issues"].as_array().unwrap().iter().find(|i| i["number"] == 1).unwrap();
    assert_eq!((crash["kind"].clone(), crash["status"].clone(), crash["author"]["login"].clone()), (json!("bug"), json!("open"), json!("bob")));

    let closed = app.get("/v1/addons/lucky/issues?state=closed", None).await.json();
    assert_eq!(closed["issues"][0]["title"], "Old bug");
    let found = app.get("/v1/addons/lucky/issues?state=all&q=carol", None).await.json();
    assert_eq!((found["matching"].clone(), found["issues"][0]["title"].clone()), (json!(1), json!("Dark mode")));
}

#[sqlx::test]
async fn issue_shows_replies_and_sanitized_text(pool: PgPool) {
    let (app, _, gh) = setup(pool).await;
    add_issue(&app, 1, "Crash", &user(2, "bob"));
    app.github.data().issues.get_mut(REPO).unwrap()[0].body = Some("**Steps**<script>alert(1)</script>".into());
    let bob = app.login(2, "bob").await;
    let reply = app.call_as("POST", "/v1/addons/lucky/issues/1/comments", Some(&bob), Some(&gh), Some(json!({ "body": "Also on Linux" }))).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let issue = app.get("/v1/addons/lucky/issues/1", None).await.json();
    let html = issue["issue"]["body_html"].as_str().unwrap();
    assert!(html.contains("<strong>Steps</strong>") && !html.contains("<script"), "{html}");
    assert_eq!(issue["replies"][0]["body"], "Also on Linux");
    assert_eq!(issue["replies"][0]["author"]["login"], "bob");
    assert_eq!(app.get("/v1/addons/lucky/issues/99", None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn issues_disabled_repo_says_so(pool: PgPool) {
    let (app, _, _) = setup(pool).await;
    app.github.data().repos.get_mut(&100).unwrap().has_issues = false;
    let list = app.get("/v1/addons/lucky/issues", None).await.json();
    assert_eq!((list["issues_enabled"].clone(), list["can_write"].clone()), (json!(false), json!(false)));
    assert!(list.get("issues").is_none());
}

#[sqlx::test]
async fn creates_issue_as_the_user_and_labels_it(pool: PgPool) {
    let (app, bob, gh) = setup(pool).await;
    let body = json!({ "kind": "bug", "title": " Crash on load ", "body": "Steps: open the bag" });
    let reply = app.call_as("POST", "/v1/addons/lucky/issues", Some(&bob), Some(&gh), Some(body)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.json()["number"], 1);
    let issue = app.github.data().issues[REPO][0].clone();
    assert_eq!((issue.title.as_str(), issue.user.as_ref().unwrap().login.as_str()), ("Crash on load", "bob"), "Bob is the author on GitHub");
    assert_eq!(issue.labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["bug"]);

    let feature = json!({ "kind": "feature", "title": "Dark mode", "body": "Please" });
    app.call_as("POST", "/v1/addons/lucky/issues", Some(&bob), Some(&gh), Some(feature)).await;
    assert_eq!(app.github.data().issues[REPO][1].labels[0].name, "enhancement");

    let question = json!({ "kind": "question", "title": "Does it work with the Mage?", "body": "Asking before I install" });
    app.call_as("POST", "/v1/addons/lucky/issues", Some(&bob), Some(&gh), Some(question)).await;
    assert_eq!(app.github.data().issues[REPO][2].labels[0].name, "question");
}

#[sqlx::test]
async fn writes_need_login_own_github_token_and_valid_text(pool: PgPool) {
    let (app, bob, gh) = setup(pool).await;
    let body = json!({ "kind": "bug", "title": "Crash", "body": "Steps" });
    let uri = "/v1/addons/lucky/issues";
    assert_eq!(app.call_as("POST", uri, None, Some(&gh), Some(body.clone())).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.call_as("POST", uri, Some(&bob), None, Some(body.clone())).await.error(), "github_reauth", "no GitHub token");
    assert_eq!(app.call_as("POST", uri, Some(&bob), Some("gho-1"), Some(body.clone())).await.error(), "github_reauth", "Alice's token");
    let empty = app.call_as("POST", uri, Some(&bob), Some(&gh), Some(json!({ "kind": "bug", "title": "  ", "body": "x" }))).await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    let kind = app.call_as("POST", uri, Some(&bob), Some(&gh), Some(json!({ "kind": "rant", "title": "x", "body": "x" }))).await;
    assert_eq!(kind.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!app.github.data().issues.contains_key(REPO), "nothing reached GitHub");

    let bob_id = app.user_id(2).await;
    sqlx::query("UPDATE users SET banned_at = now() WHERE id = $1").bind(bob_id).execute(&app.pool).await.unwrap();
    assert_eq!(app.call_as("POST", uri, Some(&bob), Some(&gh), Some(body)).await.error(), "banned", "banned here, GitHub itself stays open to them");
}

#[sqlx::test]
async fn missing_permission_falls_back_to_github_link(pool: PgPool) {
    let (app, bob, gh) = setup(pool).await;
    app.github.data().issues_read_only.insert(1);
    assert_eq!(app.get("/v1/addons/lucky/issues", None).await.json()["can_write"], false);
    let body = json!({ "kind": "bug", "title": "Crash", "body": "Steps" });
    let reply = app.call_as("POST", "/v1/addons/lucky/issues", Some(&bob), Some(&gh), Some(body)).await;
    assert_eq!((reply.status, reply.error()), (StatusCode::CONFLICT, "issues_not_enabled".into()));
    assert!(reply.json()["message"].as_str().unwrap().contains("https://github.com/alice/lucky/issues/new"));
}

#[sqlx::test]
async fn only_the_author_edits_and_closes(pool: PgPool) {
    let (app, bob, gh) = setup(pool).await;
    let alice = app.login(1, "alice").await;
    add_issue(&app, 1, "Crash", &user(2, "bob"));
    let reply = app.call_as("POST", "/v1/addons/lucky/issues/1/comments", Some(&alice), Some("gho-1"), Some(json!({ "body": "Fixed in 1.1" }))).await;
    let comment = reply.json()["id"].as_i64().unwrap();
    let uri = format!("/v1/addons/lucky/issues/1/comments/{comment}");

    let foreign = app.call_as("PATCH", &uri, Some(&bob), Some(&gh), Some(json!({ "body": "Not fixed" }))).await;
    assert_eq!((foreign.status, foreign.error()), (StatusCode::FORBIDDEN, "forbidden".into()));
    assert_eq!(app.call_as("DELETE", &uri, Some(&bob), Some(&gh), None).await.status, StatusCode::FORBIDDEN);
    let own = app.call_as("PATCH", &uri, Some(&alice), Some("gho-1"), Some(json!({ "body": "Fixed in 1.2" }))).await;
    assert_eq!((own.status, own.json()["edited"].clone()), (StatusCode::OK, json!(true)), "{}", own.body);
    assert_eq!(app.call_as("DELETE", &uri, Some(&alice), Some("gho-1"), None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky/issues/1", None).await.json()["replies"], json!([]));

    let close = json!({ "state": "closed" });
    assert_eq!(app.call_as("PATCH", "/v1/addons/lucky/issues/1", Some(&alice), Some("gho-1"), Some(close.clone())).await.status, StatusCode::FORBIDDEN);
    let closed = app.call_as("PATCH", "/v1/addons/lucky/issues/1", Some(&bob), Some(&gh), Some(close)).await;
    assert_eq!(closed.json()["status"], "closed", "{}", closed.body);
    let bad = app.call_as("PATCH", "/v1/addons/lucky/issues/1", Some(&bob), Some(&gh), Some(json!({ "state": "deleted" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn blocked_addon_has_no_issues(pool: PgPool) {
    let (app, _, _) = setup(pool).await;
    sqlx::query("UPDATE addons SET blocked_at = now() WHERE slug = 'lucky'").execute(&app.pool).await.unwrap();
    assert_eq!(app.get("/v1/addons/lucky/issues", None).await.error(), "addon_blocked");
}

#[sqlx::test]
async fn the_platform_has_issues_of_its_own_repo(pool: PgPool) {
    // One test only: the platform's installation is kept for a minute per process.
    let app = TestApp::new(pool);
    app.github.add_repo(900, "Rezondes/SurvivorsForge", "rezondes");
    let bob = app.login(2, "bob").await;
    app.github.data().issues.entry("rezondes/survivorsforge".into()).or_default().push(fake_issue("Rezondes/SurvivorsForge", 1, "Launcher crashes", "Body", &user(3, "carol")));
    let list = app.get("/v1/platform/issues", None).await.json();
    assert_eq!((list["repo"].clone(), list["can_write"].clone()), (json!("Rezondes/SurvivorsForge"), json!(true)), "{list}");
    assert_eq!(list["issues"][0]["title"], "Launcher crashes");
    let body = json!({ "kind": "feature", "title": "Dark mode", "body": "Please" });
    let reply = app.call_as("POST", "/v1/platform/issues", Some(&bob), Some("gho-2"), Some(body)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let reply = app.call_as("POST", "/v1/platform/issues/1/comments", Some(&bob), Some("gho-2"), Some(json!({ "body": "Same" }))).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(app.get("/v1/platform/issues/1", None).await.json()["replies"][0]["author"]["login"], "bob");
    assert_eq!(app.get("/v1/platform/issues/x", None).await.status, StatusCode::NOT_FOUND);
}
