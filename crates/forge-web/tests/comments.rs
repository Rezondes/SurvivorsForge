mod common;

use axum::http::StatusCode;
use common::{TestWeb, me};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn comment(id: i64, author: &str, author_id: i64, body: &str) -> Value {
    json!({
        "id": id, "deleted": false, "body": body, "author_id": author_id, "author": author, "avatar_url": "",
        "created_at": "2026-11-01T10:00:00Z", "edited_at": null, "parent_id": null,
    })
}

/// As the API sends them, newest first: carol answers bob, a deleted comment, bob's.
fn discussion() -> Value {
    let mut reply = comment(3, "carol", 7, "Same <here>");
    reply["parent_id"] = json!(1);
    let mut deleted = comment(2, "mallory", 8, "");
    deleted["deleted"] = json!(true);
    json!([reply, deleted, comment(1, "bob", 1, "Works with the Mage.\nThanks!")])
}

async fn mock_get(web: &TestWeb, at: &str, answer: Value) {
    Mock::given(method("GET")).and(path(at)).respond_with(ResponseTemplate::new(200).set_body_json(answer)).mount(&web.api).await;
}

async fn addon(web: &TestWeb, comments: Value) {
    mock_get(web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky Odds", "comments_enabled": true, "icon_url": "https://img.test/lucky.webp" })).await;
    mock_get(web, "/v1/addons/lucky/comments", comments).await;
    mock_get(web, "/v1/addons/lucky/permissions", json!({ "owner": false, "admin": false, "edit_description": false })).await;
}

/// The part of the page that belongs to one comment.
fn article(body: &str, id: i64) -> &str {
    let start = body.find(&format!(r#"id="comment-{id}""#)).unwrap_or_else(|| panic!("comment {id} missing: {body}"));
    let rest = &body[body[..start].rfind("<article").unwrap()..];
    &rest[..rest.find("</article>").unwrap()]
}

#[tokio::test]
async fn visitors_read_threads_and_learn_how_to_join() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    let page = web.get("/addons/lucky/comments").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<a href="/addons/lucky/comments" aria-current="page">Comments</a>"#), "the Comments tab: {body}");
    assert!(body.contains(r#"<p class="back"><a href="/addons/lucky">Lucky Odds</a></p>"#), "back to the overview");
    assert!(body.contains(r#"<img class="icon big" src="https://img.test/lucky.webp""#) && body.contains("<h1>Lucky Odds</h1>"), "icon and name as on the overview");
    let order: Vec<usize> = [2, 1, 3].iter().map(|id| body.find(&format!(r#"id="comment-{id}""#)).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "newest comment first, the reply under bob's");
    assert!(article(body, 3).contains("depth-1") && article(body, 1).contains("depth-0"));
    assert!(article(body, 3).contains("Same &#60;here&#62;"), "escaped text");
    assert!(article(body, 2).contains("Deleted comment.") && !article(body, 2).contains("mallory"));
    assert!(body.contains(r#"<a href="/login?next=%2Faddons%2Flucky%2Fcomments">Log in with GitHub</a> to write a comment."#));
    assert!(!body.contains(r#"action="/addons/lucky/comments"#), "no forms for visitors");
    assert!(!body.contains("Older comments"), "one page only");
}

#[tokio::test]
async fn a_full_page_links_to_older_comments() {
    let mut web = TestWeb::new().await;
    let page: Vec<Value> = (1..=31).rev().map(|id| comment(id, "bob", 1, "x")).collect();
    addon(&web, json!(page)).await;
    let body = web.get("/addons/lucky/comments").await.body;
    assert!(body.contains(r#"href="/addons/lucky/comments?before=2""#), "{body}");
    assert!(!body.contains(r#"id="comment-1""#), "the extra comment only shows that there are more");
}

#[tokio::test]
async fn the_author_edits_and_deletes_others_get_a_report_form() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    web.login_as("t", me("bob", "user")).await;
    let body = web.get("/addons/lucky/comments").await.body;
    let own = article(&body, 1);
    assert!(own.contains("/comments/1/edit") && own.contains("/comments/1/delete") && !own.contains("/comments/1/report"), "{own}");
    let other = article(&body, 3);
    assert!(other.contains("/comments/3/report") && !other.contains("/comments/3/edit") && !other.contains("/comments/3/delete"), "{other}");
    assert!(other.contains(r#"<option value="inappropriate">"#));
    assert!(!article(&body, 2).contains("<form"), "nothing to do with a deleted comment");
}

#[tokio::test]
async fn moderators_and_the_team_delete_but_never_edit() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    web.login_as("t", me("root", "moderator")).await;
    let other = web.get("/addons/lucky/comments").await.body;
    assert!(article(&other, 3).contains("/comments/3/delete") && !article(&other, 3).contains("/comments/3/edit"));

    let mut team = TestWeb::new().await;
    addon(&team, discussion()).await;
    Mock::given(method("GET"))
        .and(path("/v1/addons/lucky/permissions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "owner": true, "admin": false, "edit_description": true })))
        .with_priority(1)
        .mount(&team.api)
        .await;
    team.login_as("t", me("alice", "user")).await;
    let body = team.get("/addons/lucky/comments").await.body;
    assert!(article(&body, 3).contains("/comments/3/delete") && !article(&body, 3).contains("/comments/3/edit"));
}

#[tokio::test]
async fn a_reply_goes_to_the_api_and_back_to_the_comment() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    web.login_as("t", me("bob", "user")).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/comments"))
        .and(body_partial_json(json!({ "body": "Agreed", "parent_id": 3 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 4 })))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/comments", &[("body", "Agreed"), ("parent_id", "3")]).await;
    assert_eq!(page.location(), "/addons/lucky/comments?done=Your+comment+is+posted.#comment-4");
}

#[tokio::test]
async fn a_refusal_keeps_the_text_next_to_the_error() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    web.login_as("t", me("bob", "user")).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/comments"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "error": "comment_limit", "message": "At most 100 comments a day" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/comments", &[("body", "one more")]).await;
    assert_eq!(page.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.body.contains(">one more</textarea>"), "the text stays: {}", page.body);
    assert!(page.body.contains(r#"<p class="field-error" id="new-comment-error">At most 100 comments a day</p>"#), "{}", page.body);
    assert!(page.body.contains(r#"aria-describedby="new-comment-error""#));

    let reply = web.post_form("/addons/lucky/comments", &[("body", "my reply"), ("parent_id", "3")]).await;
    let form = article(&reply.body, 3);
    assert!(form.contains("<details open>") && form.contains(">my reply</textarea>"), "the reply form stays open with its text: {form}");
}

#[tokio::test]
async fn a_report_goes_into_the_admin_queue() {
    let mut web = TestWeb::new().await;
    addon(&web, discussion()).await;
    web.login_as("t", me("bob", "user")).await;
    Mock::given(method("POST"))
        .and(path("/v1/reports"))
        .and(body_partial_json(json!({ "target_type": "comment", "target": "3", "reason": "spam", "text": "Ads" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/comments/3/report", &[("reason", "spam"), ("text", " Ads ")]).await;
    assert!(page.location().starts_with("/addons/lucky/comments?done=") && page.location().ends_with("#comment-3"), "{}", page.location());
}

#[tokio::test]
async fn the_admin_queue_shows_the_reported_comment() {
    let mut web = TestWeb::new().await;
    web.login_as("t", me("root", "admin")).await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/reports"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 4, "target_type": "comment", "target_id": 3, "addon": "lucky", "comment": "Buy gold", "reason": "spam", "text": "", "reporter": "bob", "created_at": "2026-11-01T10:00:00Z" },
            { "id": 5, "target_type": "comment", "target_id": 2, "addon": "lucky", "comment": "", "reason": "spam", "text": "", "reporter": "bob", "created_at": "2026-11-01T10:00:00Z" },
        ])))
        .mount(&web.api)
        .await;
    let body = web.get("/admin/reports").await.body;
    assert!(body.contains(r#"<a href="/addons/lucky/comments#comment-3">lucky</a>"#), "{body}");
    assert!(body.contains("Buy gold") && body.contains("Deleted since."));
}

#[tokio::test]
async fn turned_off_comments_say_so_and_show_nothing() {
    let mut web = TestWeb::new().await;
    mock_get(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky Odds", "comments_enabled": false })).await;
    web.login_as("t", me("bob", "user")).await;
    let page = web.get("/addons/lucky/comments").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("The creator has not turned on comments") && !page.body.contains("Your comment") && !page.body.contains("No comments yet"), "{}", page.body);
}

#[tokio::test]
async fn older_pages_ask_the_api_for_older_comments() {
    let mut web = TestWeb::new().await;
    mock_get(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky Odds", "comments_enabled": true })).await;
    Mock::given(method("GET"))
        .and(path("/v1/addons/lucky/comments"))
        .and(query_param("before", "40"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([comment(39, "bob", 1, "older")])))
        .expect(1)
        .mount(&web.api)
        .await;
    assert!(web.get("/addons/lucky/comments?before=40").await.body.contains("older"));
}
