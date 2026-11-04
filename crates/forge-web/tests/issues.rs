mod common;

use axum::http::StatusCode;
use common::{TestWeb, me};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn frame(can_write: bool) -> Value {
    json!({
        "repo": "alice/lucky", "issues_enabled": true, "can_write": can_write,
        "issues_url": "https://github.com/alice/lucky/issues", "new_issue_url": "https://github.com/alice/lucky/issues/new",
    })
}

fn list(can_write: bool) -> Value {
    let mut answer = frame(can_write);
    answer["state"] = json!("open");
    answer["counts"] = json!({ "open": 1, "closed": 2, "all": 3 });
    answer["matching"] = json!(1);
    answer["page"] = json!(1);
    answer["pages"] = json!(1);
    answer["truncated"] = json!(false);
    answer["issues"] = json!([{
        "number": 6, "title": "Crash <on> load", "kind": "bug", "status": "open", "comments": 2,
        "author": { "login": "bob", "avatar_url": "" }, "created_at": "2026-11-01T10:00:00Z", "updated_at": "2026-11-02T10:00:00Z",
    }]);
    answer
}

fn issue(author: &str) -> Value {
    let mut answer = frame(true);
    answer["issue"] = json!({
        "number": 6, "title": "Crash", "kind": "bug", "status": "in_progress", "state": "open", "edited": false,
        "author": { "login": author, "avatar_url": "" }, "created_at": "2026-11-01T10:00:00Z",
        "body": "Steps", "body_html": "<p>Steps</p>", "html_url": "https://github.com/alice/lucky/issues/6",
    });
    answer["replies"] = json!([
        { "id": 11, "author": { "login": "carol", "avatar_url": "" }, "body": "Same here", "body_html": "<p>Same here</p>", "created_at": "2026-11-01T11:00:00Z", "edited": true, "html_url": "u" },
        { "id": 12, "author": { "login": "bob", "avatar_url": "" }, "body": "Mine", "body_html": "<p>Mine</p>", "created_at": "2026-11-01T12:00:00Z", "edited": false, "html_url": "u" },
    ]);
    answer
}

async fn mock_get(web: &TestWeb, at: &str, answer: Value) {
    Mock::given(method("GET")).and(path(at)).respond_with(ResponseTemplate::new(200).set_body_json(answer)).mount(&web.api).await;
}

async fn addon(web: &TestWeb) {
    mock_get(web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky Odds", "repo": "alice/lucky", "issues_enabled": true, "icon_url": "https://img.test/lucky.webp" })).await;
}

fn hx_redirect(page: &common::Page) -> &str {
    page.headers.get("hx-redirect").and_then(|v| v.to_str().ok()).unwrap_or_default()
}

fn github_token(expires_at: &str) -> Value {
    json!({ "access_token": "gho-bob", "expires_at": expires_at, "refresh_token": "ghr-bob" })
}

#[tokio::test]
async fn issue_list_shows_rows_tabs_and_how_to_join() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    Mock::given(method("GET"))
        .and(path("/v1/addons/lucky/issues"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(true)))
        .mount(&web.api)
        .await;
    let page = web.get_htmx("/addons/lucky/issues").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<a href="/addons/lucky/issues" aria-current="page">Issues</a>"#), "{body}");
    assert!(body.contains("Crash &#60;on&#62; load") && body.contains("Bug report") && body.contains("2 replies"), "escaped title, kind, replies");
    assert!(body.contains(r#"Closed <span class="count">2</span>"#));
    assert!(body.contains(r#"<a href="/login?next=%2Faddons%2Flucky%2Fissues">Log in with GitHub</a>"#), "a visitor learns how to report");
    assert!(!body.contains(r#"href="/addons/lucky/issues/new">New issue"#), "no form for visitors");
}

#[tokio::test]
async fn without_write_access_the_page_points_to_github() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues", list(false)).await;
    mock_get(&web, "/v1/addons/lucky/issues/frame", frame(false)).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    let page = web.get_htmx("/addons/lucky/issues").await;
    assert!(page.body.contains(r#"href="https://github.com/alice/lucky/issues/new""#) && page.body.contains("Report on GitHub"), "{}", page.body);
    assert!(!page.body.contains(r#"href="/addons/lucky/issues/new""#));
}

#[tokio::test]
async fn login_keeps_the_github_token_encrypted_and_logout_drops_it() {
    let mut web = TestWeb::new().await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    let cookie = web.cookies.get("forge_github").expect("GitHub cookie");
    assert!(!cookie.contains("gho-bob"), "encrypted like the session");
    Mock::given(method("POST")).and(path("/v1/auth/logout")).respond_with(ResponseTemplate::new(204)).mount(&web.api).await;
    web.post_form("/logout", &[]).await;
    assert!(!web.cookies.contains_key("forge_github"));
}

#[tokio::test]
async fn new_issue_goes_to_github_as_the_visitor() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues", list(true)).await;
    mock_get(&web, "/v1/addons/lucky/issues/frame", frame(true)).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    let form = web.get_htmx("/addons/lucky/issues/new?kind=feature").await;
    assert!(form.body.contains(r#"value="feature" checked"#) && form.body.contains("Posted publicly on GitHub as bob"), "{}", form.body);
    let question = web.get_htmx("/addons/lucky/issues/new?kind=question").await;
    assert!(question.body.contains(r#"value="question" checked"#) && !question.body.contains(r#"value="bug" checked"#), "{}", question.body);
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/issues"))
        .and(header("x-github-token", "gho-bob"))
        .and(header("authorization", "Bearer t"))
        .and(body_partial_json(json!({ "kind": "bug", "title": "Crash", "body": "Steps" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "number": 7 })))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/issues", &[("kind", "bug"), ("title", "Crash"), ("body", "Steps")]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER, "{}", page.body);
    assert!(page.location().starts_with("/addons/lucky/issues/7?done="), "{}", page.location());
}

#[tokio::test]
async fn refused_issue_keeps_what_was_typed() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues", list(true)).await;
    mock_get(&web, "/v1/addons/lucky/issues/frame", frame(true)).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/issues"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "error": "too_many", "message": "At most 5 per hour, try again later" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/issues", &[("kind", "feature"), ("title", "Dark mode"), ("body", "Please add it")]).await;
    assert!(page.body.contains("At most 5 per hour"), "{}", page.body);
    assert!(page.body.contains(r#"value="Dark mode""#) && page.body.contains("Please add it</textarea>") && page.body.contains(r#"value="feature" checked"#));
}

#[tokio::test]
async fn an_expiring_github_token_is_renewed_first() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    web.login_with_github("t", me("bob", "user"), github_token("2020-01-01T00:00:00Z")).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/github/refresh"))
        .and(body_partial_json(json!({ "refresh_token": "ghr-bob" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "gho-new", "expires_at": "2030-01-01T00:00:00Z", "refresh_token": "ghr-new" })))
        .expect(1)
        .mount(&web.api)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/issues/6/comments"))
        .and(header("x-github-token", "gho-new"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 13 })))
        .expect(1)
        .mount(&web.api)
        .await;
    let before = web.cookies.get("forge_github").cloned();
    let page = web.post_form("/addons/lucky/issues/6/reply", &[("body", "Still there")]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER, "{}", page.body);
    assert_ne!(web.cookies.get("forge_github").cloned(), before, "the renewed token is kept");
}

#[tokio::test]
async fn logged_in_before_issues_asks_to_log_in_again() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues/6", issue("bob")).await;
    web.login_as("t", me("bob", "user")).await;
    let page = web.get_htmx("/addons/lucky/issues/6").await;
    assert!(page.body.contains(r#"<a href="/login?next=%2Faddons%2Flucky%2Fissues%2F6">Log in again</a>"#), "{}", page.body);
    let reply = web.post_form("/addons/lucky/issues/6/reply", &[("body", "Hi")]).await;
    assert!(reply.body.contains("Log in again to post on GitHub.") && reply.body.contains(">Hi</textarea>"), "the reply stays in the box: {}", reply.body);
}

#[tokio::test]
async fn issue_page_shows_replies_and_tools_only_on_own_posts() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues/6", issue("bob")).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    let page = web.get_htmx("/addons/lucky/issues/6?done=Saved").await;
    let body = &page.body;
    assert!(body.contains("In progress") && body.contains("<p>Same here</p>") && body.contains("· edited"), "{body}");
    assert!(body.contains(r#"role="status">Saved<"#));
    assert!(body.contains("/addons/lucky/issues/6/replies/12/edit") && !body.contains("/addons/lucky/issues/6/replies/11/edit"), "only Bob's reply");
    assert!(body.contains("Close issue"), "the author closes");
    assert!(body.contains("This deletes the reply on GitHub too"));
}

#[tokio::test]
async fn a_tab_the_owner_did_not_turn_on_shows_only_the_way_to_github() {
    let mut web = TestWeb::new().await;
    mock_get(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky Odds", "repo": "alice/lucky", "issues_enabled": false })).await;
    mock_get(&web, "/v1/addons/lucky/issues", json!({
        "repo": "alice/lucky", "issues_here": false,
        "issues_url": "https://github.com/alice/lucky/issues", "new_issue_url": "https://github.com/alice/lucky/issues/new",
    }))
    .await;
    let page = web.get("/addons/lucky/issues").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("The creator has not turned on issues on SurvivorsForge"), "{}", page.body);
    assert!(page.body.contains(r#"href="https://github.com/alice/lucky/issues/new" rel="external noopener">Report on GitHub</a>"#));
    assert!(!page.body.contains("issue-list") && !page.body.contains("New issue"), "no list, no form");
    assert_eq!(web.get("/addons/lucky/issues/6").await.location(), "/addons/lucky/issues", "an issue page goes back to the tab");
    assert_eq!(web.get("/addons/lucky/issues/new?kind=bug").await.location(), "https://github.com/alice/lucky/issues/new");
}

#[tokio::test]
async fn pages_that_ask_github_answer_at_once_and_load_the_rest() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    Mock::given(method("GET")).and(path("/v1/addons/lucky/issues")).respond_with(ResponseTemplate::new(200).set_body_json(list(true))).expect(1).mount(&web.api).await;
    Mock::given(method("GET")).and(path("/v1/addons/lucky/issues/6")).respond_with(ResponseTemplate::new(200).set_body_json(issue("bob"))).expect(1).mount(&web.api).await;
    for (uri, shape) in [("/addons/lucky/issues?state=closed", "skeleton-rows"), ("/addons/lucky/issues/6?done=Saved", "skeleton-line w-50 tall"), ("/addons/lucky/issues/new?kind=bug", "skeleton-block area")] {
        let page = web.get(uri).await;
        assert_eq!(page.status, StatusCode::OK, "{uri}: {}", page.body);
        let body = &page.body;
        assert!(body.contains(&format!(r#"hx-get="{}""#, uri.replace('&', "&#38;"))) && body.contains(r##"hx-select="#lazy-content""##), "{uri}: {body}");
        assert!(body.contains(shape) && body.contains(r#"aria-busy="true""#), "{uri}: the skeleton of what comes");
        assert!(body.contains(r#"<a href="/addons/lucky/issues" aria-current="page">Issues</a>"#), "{uri}: the tabs at once");
        assert!(body.contains(r#"<img class="icon big" src="https://img.test/lucky.webp""#) && body.contains("<h1>Lucky Odds</h1>"), "{uri}: icon and name");
        assert!(body.contains(r#"href="/addons/lucky/issues/new?kind=bug">Report a bug</a>"#), "{uri}: with the tab on, bugs are reported here");
        assert!(body.contains("full=1"), "{uri}: a way without JavaScript");
    }
    assert!(web.get("/addons/lucky/issues/6?done=Saved").await.body.contains(r#"role="status">Saved<"#), "notices at once");
    // Asked by htmx (or with full=1), the page comes whole; the API mocks expect one call each.
    assert!(web.get_htmx("/addons/lucky/issues").await.body.contains(r#"<div id="lazy-content">"#));
    assert!(web.get("/addons/lucky/issues/6?full=1").await.body.contains(r#"<div id="lazy-content">"#));
}

#[tokio::test]
async fn a_locked_issue_has_no_reply_form() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    let mut locked = issue("bob");
    locked["issue"]["locked"] = json!(true);
    mock_get(&web, "/v1/addons/lucky/issues/6", locked).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    let body = web.get_htmx("/addons/lucky/issues/6").await.body;
    assert!(body.contains("This issue is locked on GitHub") && !body.contains("/addons/lucky/issues/6/reply"), "{body}");
}

#[tokio::test]
async fn report_a_bug_goes_to_github_unless_the_app_may_post() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues", list(false)).await;
    mock_get(&web, "/v1/addons/lucky/issues/frame", frame(false)).await;
    let page = web.get_htmx("/addons/lucky/issues/new?kind=bug").await;
    assert_eq!((page.status, hx_redirect(&page)), (StatusCode::NO_CONTENT, "https://github.com/alice/lucky/issues/new"), "no login needed for GitHub's form");
    let plain = web.get("/addons/lucky/issues/new?kind=bug&full=1").await;
    assert_eq!((plain.status, plain.location()), (StatusCode::SEE_OTHER, "https://github.com/alice/lucky/issues/new"), "without JavaScript");

    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues", list(true)).await;
    mock_get(&web, "/v1/addons/lucky/issues/frame", frame(true)).await;
    let page = web.get_htmx("/addons/lucky/issues/new?kind=bug").await;
    assert_eq!(hx_redirect(&page), "/login", "the form here needs a login");
    assert_eq!(web.get("/addons/lucky/issues/new?kind=bug&full=1").await.location(), "/login?next=%2Faddons%2Flucky%2Fissues%2Fnew%3Fkind%3Dbug%26full%3D1");
}

#[tokio::test]
async fn survivorsforge_has_its_own_issue_pages() {
    let mut web = TestWeb::new().await;
    mock_get(&web, "/v1/platform/issues", list(false)).await;
    mock_get(&web, "/v1/platform/issues/frame", frame(false)).await;
    let page = web.get_htmx("/issues").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("<h1>SurvivorsForge</h1>") && page.body.contains("for the SurvivorsForge launcher and website"), "{}", page.body);
    assert!(page.body.contains(r#"<a href="/issues/6">"#) && !page.body.contains("addon-tabs"), "no addon tabs");
    assert!(page.body.contains(r#"<a href="/issues">Issues and feedback</a>"#), "linked in the footer");
    let new = web.get_htmx("/issues/new?kind=bug").await;
    assert_eq!(hx_redirect(&new), "https://github.com/alice/lucky/issues/new", "GitHub's form while the App may not post");
}

#[tokio::test]
async fn a_refused_edit_keeps_what_was_typed() {
    let mut web = TestWeb::new().await;
    addon(&web).await;
    mock_get(&web, "/v1/addons/lucky/issues/6", issue("bob")).await;
    web.login_with_github("t", me("bob", "user"), github_token("2030-01-01T00:00:00Z")).await;
    for route in ["/v1/addons/lucky/issues/6", "/v1/addons/lucky/issues/6/comments/12"] {
        Mock::given(method("PATCH"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "invalid", "message": "GitHub refused it" })))
            .mount(&web.api)
            .await;
    }
    let page = web.post_form("/addons/lucky/issues/6/edit", &[("title", "New title"), ("body", "New text")]).await;
    assert!(page.body.contains("GitHub refused it"), "{}", page.body);
    assert!(page.body.contains(r#"value="New title""#) && page.body.contains(">New text</textarea>"), "{}", page.body);
    assert!(page.body.contains("<details open>"), "the edit form stays open");
    let page = web.post_form("/addons/lucky/issues/6/replies/12/edit", &[("body", "Changed reply")]).await;
    assert!(page.body.contains(">Changed reply</textarea>") && page.body.contains("<details open>"), "{}", page.body);
}
