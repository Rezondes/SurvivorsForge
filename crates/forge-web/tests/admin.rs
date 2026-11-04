mod common;

use axum::http::StatusCode;
use common::{TestWeb, me};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

async fn get_json(web: &TestWeb, route: &str, body: Value) {
    Mock::given(method("GET")).and(path(route)).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&web.api).await;
}

async fn expect(web: &TestWeb, verb: &str, route: &str, body: Value) {
    Mock::given(method(verb))
        .and(path(route))
        .and(body_partial_json(body))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&web.api)
        .await;
}

async fn as_admin() -> TestWeb {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("root", "admin")).await;
    // Low priority: a test can mount its own list.
    Mock::given(method("GET"))
        .and(path("/v1/admin/admins"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "github_id": 99, "login": "root", "by_config": true, "granted": false }])))
        .with_priority(10)
        .mount(&web.api)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/verification-requests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .with_priority(10)
        .mount(&web.api)
        .await;
    get_json(&web, "/v1/admin/reports", json!([
        { "id": 3, "target_type": "addon", "target_id": 1, "addon": "lucky", "reason": "malware", "text": "Deletes files", "reporter": "eve", "created_at": "2026-05-01T10:00:00Z" }
    ]))
    .await;
    get_json(&web, "/v1/admin/review-queue", json!([
        { "version_id": 9, "tag": "v2", "trust": "flagged", "slug": "lucky", "name": "Lucky", "compare_url": "https://github.com/a/lucky/compare/v1...v2",
          "findings": [{ "rule": "lua.url", "severity": "warn", "file": "a.lua", "line": 3, "message": "Contains a web address", "snippet": "x" }] }
    ]))
    .await;
    web
}

#[tokio::test]
async fn non_admin_forbidden_everywhere() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("eve", "user")).await;
    for page in ["/admin", "/admin/review", "/admin/reports", "/admin/users", "/admin/addons/new", "/admin/list/categories", "/admin/audit"] {
        assert_eq!(web.get(page).await.status, StatusCode::FORBIDDEN, "{page}");
    }
    assert_eq!(web.post_form("/admin/versions/9/verify", &[("note", "x")]).await.status, StatusCode::FORBIDDEN);
    let mut anonymous = TestWeb::new().await;
    assert_eq!(anonymous.get("/admin").await.location(), "/login?next=%2Fadmin");
}

#[tokio::test]
async fn queue_renders_findings_and_diff() {
    let mut web = as_admin().await;
    let index = web.get("/admin").await;
    assert!(index.body.contains("<strong>1</strong> version to review") && index.body.contains("<strong>1</strong> open report"));
    let page = web.get("/admin/review/lucky").await;
    for text in ["Lucky", "v2", "a.lua:3", "Contains a web address", "https://github.com/a/lucky/compare/v1...v2", "required: why the findings are fine"] {
        assert!(page.body.contains(text), "missing {text}");
    }
}

/// Queue entries for the review tests: two versions of Lucky (flagged, changed file), one of Other.
async fn grouped_queue(web: &TestWeb) {
    Mock::given(method("GET"))
        .and(path("/v1/admin/review-queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "version_id": 9, "tag": "v2", "trust": "flagged", "slug": "lucky", "name": "Lucky", "compare_url": null,
              "findings": [{ "rule": "lua.url", "severity": "warn", "file": "a.lua", "line": 3, "message": "Contains a web address", "snippet": "x" }] },
            { "version_id": 10, "tag": "v1", "trust": "scanned", "slug": "other", "name": "Other", "compare_url": null, "findings": [] },
            { "version_id": 11, "tag": "v3", "trust": "source_changed", "slug": "lucky", "name": "Lucky", "compare_url": null, "findings": [] }
        ])))
        .with_priority(1)
        .mount(&web.api)
        .await;
}

#[tokio::test]
async fn review_lists_addons_first_then_their_versions() {
    let mut web = as_admin().await;
    grouped_queue(&web).await;
    let page = web.get("/admin/review").await;
    for text in ["href=\"/admin/review/lucky\"", "2 versions", "href=\"/admin/review/other\"", "1 version", "action=\"/admin/review/other/skip\"", "Review one by one"] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    assert!(!page.body.contains("a.lua:3"), "versions only on the addon's page");
    assert!(!page.body.contains("action=\"/admin/review/lucky/skip\""), "no blind bulk decision on a flagged addon");
    assert!(page.body.find("Lucky") < page.body.find("Other"), "addons in queue order");

    let lucky = web.get("/admin/review/lucky").await;
    for text in ["v2", "v3", "a.lua:3", "action=\"/admin/versions/9/skip\"", "action=\"/admin/versions/11/accept-source\"", "action=\"/admin/review/lucky/block\""] {
        assert!(lucky.body.contains(text), "missing {text}");
    }
    assert!(!lucky.body.contains("Other"));
    assert!(web.get("/admin/review/nothing").await.body.contains("Nothing left to review"));
}

#[tokio::test]
async fn skip_returns_to_the_addon() {
    let mut web = as_admin().await;
    expect(&web, "POST", "/v1/admin/versions/9/skip", json!({ "note": "" })).await;
    let done = web.post_form("/admin/versions/9/skip", &[("note", ""), ("back", "/admin/review/lucky")]).await;
    assert!(done.location().starts_with("/admin/review/lucky?msg="), "{}", done.location());
}

#[tokio::test]
async fn bulk_actions_cover_every_queued_version_of_the_addon() {
    let mut web = as_admin().await;
    grouped_queue(&web).await;
    expect(&web, "POST", "/v1/admin/versions/9/verification", json!({ "note": "all fine" })).await;
    expect(&web, "POST", "/v1/admin/versions/11/accept-source", json!({ "note": "all fine" })).await;
    let done = web.post_form("/admin/review/lucky/verify", &[("note", "all fine")]).await;
    assert!(done.location().starts_with("/admin/review?msg="), "{}", done.location());
    assert!(web.get(done.location()).await.body.contains("2 versions of Lucky verified."));

    expect(&web, "POST", "/v1/admin/versions/9/skip", json!({})).await;
    expect(&web, "POST", "/v1/admin/versions/11/skip", json!({})).await;
    web.post_form("/admin/review/lucky/skip", &[("note", "")]).await;

    expect(&web, "POST", "/v1/admin/versions/9/block", json!({ "note": "malware" })).await;
    Mock::given(method("POST"))
        .and(path("/v1/admin/versions/11/block"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "not_found", "message": "Not found" })))
        .mount(&web.api)
        .await;
    let done = web.post_form("/admin/review/lucky/block", &[("note", "malware")]).await;
    assert!(done.location().starts_with("/admin/review/lucky?"), "back to the versions left: {}", done.location());
    let page = web.get(done.location()).await;
    assert!(page.body.contains("1 of 2 versions of Lucky blocked. v3: Not found"), "{}", page.body);
    let bare = web.post_form("/admin/review/lucky/block", &[("note", " ")]).await;
    assert!(web.get(bare.location()).await.body.contains("Write a note first."), "one message, no API calls");
}

#[tokio::test]
async fn actions_ask_for_note() {
    let mut web = as_admin().await;
    expect(&web, "POST", "/v1/admin/versions/9/verification", json!({ "note": "URL is the wiki" })).await;
    let page = web.post_form("/admin/versions/9/verify", &[("note", "URL is the wiki")]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER);
    assert!(page.location().starts_with("/admin/review?msg="), "{}", page.location());

    Mock::given(method("POST"))
        .and(path("/v1/admin/versions/9/block"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "note_required", "message": "Admin actions need a note" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/admin/versions/9/block", &[("note", "")]).await;
    let follow = web.get(page.location()).await;
    assert!(follow.body.contains("Admin actions need a note"), "error shown on the page");

    expect(&web, "POST", "/v1/admin/reports/3/resolve", json!({ "status": "dismissed", "note": "fine" })).await;
    web.post_form("/admin/reports/3/resolve", &[("status", "dismissed"), ("note", "fine")]).await;
    let reports = web.get("/admin/reports").await.body;
    assert!(reports.contains("Deletes files") && reports.contains(r#"<span class="badge bad">Harmful code"#), "the reason in words, marked: {reports}");

    get_json(&web, "/v1/admin/users/eve", json!({ "github_id": 5, "login": "eve", "role": "user", "banned": false, "addons": 2 })).await;
    let users = web.get("/admin/users?login=eve").await;
    assert!(users.body.contains("Owns 2 addons") && users.body.contains("Ban eve"));
    expect(&web, "POST", "/v1/admin/users/5/ban", json!({ "note": "spam" })).await;
    let page = web.post_form("/admin/users/5/ban", &[("note", "spam"), ("login", "eve")]).await;
    assert!(page.location().starts_with("/admin/users?login=eve&msg="), "{}", page.location());
}

#[tokio::test]
async fn game_versions_and_categories_crud() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/game-versions", json!([{ "id": 4, "name": "0.69.0", "sort_key": 10 }])).await;
    get_json(&web, "/v1/categories", json!([{ "name": "Unit Frames", "parent": null, "selectable": true, "sort_key": 40 }, { "name": "Classes", "parent": null, "selectable": false, "sort_key": 10 }, { "name": "Miscellaneous", "parent": null, "selectable": true, "sort_key": 60 }])).await;
    assert!(web.get("/admin/list/game-versions").await.body.contains("value=\"0.69.0\""));
    let page = web.get("/admin/list/categories").await;
    assert!(page.body.contains("value=\"Miscellaneous\""), "{}", page.body);
    assert!(page.body.contains("<option value=\"Classes\">Classes</option>"), "top-level categories offered as parents");

    expect(&web, "POST", "/v1/admin/categories", json!({ "name": "Death Knight", "sort_key": 10, "parent": "Classes" })).await;
    web.post_form("/admin/list/categories", &[("name", "Death Knight"), ("sort_key", "10"), ("parent", "Classes")]).await;

    expect(&web, "POST", "/v1/admin/game-versions", json!({ "name": "0.70.0", "sort_key": 20 })).await;
    web.post_form("/admin/list/game-versions", &[("name", "0.70.0"), ("sort_key", "20")]).await;
    expect(&web, "PATCH", "/v1/admin/game-versions/4", json!({ "name": "0.69", "sort_key": 11 })).await;
    web.post_form("/admin/list/game-versions/4/save", &[("name", "0.69"), ("sort_key", "11")]).await;
    Mock::given(method("DELETE")).and(path("/v1/admin/categories/Miscellaneous")).respond_with(ResponseTemplate::new(204)).expect(1).mount(&web.api).await;
    let page = web.post_form("/admin/list/categories/Miscellaneous/delete", &[]).await;
    assert!(page.location().starts_with("/admin/list/categories?msg="));
}

fn lucky_versions() -> Value {
    json!([
        { "id": 12, "tag": "v2", "published_at": "2026-06-01T00:00:00Z", "trust": "flagged", "findings": { "critical": 0, "warn": 2, "info": 0 } },
        { "id": 11, "tag": "v1", "published_at": "2026-05-01T00:00:00Z", "trust": "verified", "findings": { "critical": 0, "warn": 0, "info": 1 } },
        { "id": 10, "tag": "v0", "published_at": "2026-04-01T00:00:00Z", "trust": "blocked", "findings": { "critical": 1, "warn": 0, "info": 0 } },
    ])
}

#[tokio::test]
async fn every_version_of_an_addon_can_be_verified_or_blocked() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky", "owner": null, "blocked": false })).await;
    get_json(&web, "/v1/addons/lucky/versions", lucky_versions()).await;
    let page = web.get("/admin/addons/lucky").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["v2", "2 warn", "/admin/versions/12/verify", "/admin/versions/12/block", "/admin/versions/11/unverify", "/admin/versions/10/unblock", "/addons/lucky/versions/v2"] {
        assert!(page.body.contains(text), "missing {text}");
    }
    assert!(!page.body.contains("/admin/versions/11/verify\""), "verified versions offer no second verify");

    expect(&web, "POST", "/v1/admin/versions/12/verification", json!({ "note": "fine" })).await;
    let done = web.post_form("/admin/versions/12/verify", &[("note", "fine"), ("back", "/admin/addons/lucky")]).await;
    assert!(done.location().starts_with("/admin/addons/lucky?msg="), "back to the addon: {}", done.location());
    expect(&web, "DELETE", "/v1/admin/versions/10/block", json!({ "note": "ok now" })).await;
    let done = web.post_form("/admin/versions/10/unblock", &[("note", "ok now"), ("back", "https://evil.test/")]).await;
    assert!(done.location().starts_with("/admin/review?"), "only admin pages as target: {}", done.location());
}

#[tokio::test]
async fn admins_find_addons_by_name() {
    let mut web = as_admin().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/addons"))
        .and(wiremock::matchers::query_param("q", "luck"))
        .and(wiremock::matchers::query_param("page", "1"))
        .and(wiremock::matchers::header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 51, "page": 1, "per_page": 50, "addons": [
            { "slug": "lucky", "name": "Lucky Odds", "repo": "cadoky/LuckyOdds", "owner": "cadoky", "author": "cadoky", "blocked": false, "locked": false,
              "latest_tag": "v1.2.0", "trust": "flagged", "findings": { "critical": 0, "warn": 2 } },
            { "slug": "gone", "name": "Gone", "repo": "x/gone", "owner": null, "author": "someone", "blocked": true, "locked": true,
              "latest_tag": null, "trust": null, "findings": { "critical": 0, "warn": 0 } }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/admin/addons?q=luck").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in [
        "<th>Addon</th><th>Owner</th><th>Newest version</th>",
        r#"<a href="/admin/addons/lucky">Lucky Odds</a>"#,
        r#"href="https://github.com/cadoky/LuckyOdds""#,
        "v1.2.0",
        "2 findings",
        r#"<a class="button secondary" href="/addons/lucky/edit">Edit</a>"#,
        r#"<a class="button secondary" href="/admin/addons/lucky">Moderate</a>"#,
        r#"<span class="badge bad">Blocked</span>"#,
        r#"<span class="badge warn">Repo not reachable</span>"#,
        "someone (not claimed)",
        "No release yet",
        r#"href="/admin/addons?q=luck&#38;page=2""#,
    ] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    assert!(web.get("/admin").await.body.contains("action=\"/admin/addons\""), "search on the admin start page");
}

#[tokio::test]
async fn owner_transfer_form() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky", "owner": null, "blocked": false })).await;
    get_json(&web, "/v1/addons/lucky/versions", json!([])).await;
    let page = web.get("/admin/addons/lucky").await;
    assert!(page.body.contains("Nobody yet"));
    expect(&web, "POST", "/v1/admin/addons/lucky/owner", json!({ "login": "cadoky", "note": "author" })).await;
    let page = web.post_form("/admin/addons/lucky/owner", &[("login", "cadoky"), ("note", "author")]).await;
    assert!(page.location().starts_with("/admin/addons/lucky?msg="));
}

#[tokio::test]
async fn admin_adds_addon_without_app() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/categories", json!([{ "name": "Unit Frames", "parent": null, "selectable": true, "sort_key": 40 }, { "name": "Classes", "parent": null, "selectable": false, "sort_key": 10 }, { "name": "Miscellaneous", "parent": null, "selectable": true, "sort_key": 60 }])).await;
    Mock::given(method("POST"))
        .and(path("/v1/admin/addons"))
        .and(body_partial_json(json!({ "repo": "cadoky/LuckyOdds", "author_name": "cadoky", "folders": ["LuckyOdds"] })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "slug": "luckyodds" })))
        .expect(1)
        .mount(&web.api)
        .await;
    let fields = [
        ("repo", "cadoky/LuckyOdds"), ("author_name", "cadoky"), ("name", "LuckyOdds"), ("slug", "luckyodds"), ("summary", "Odds"),
        ("category", "Miscellaneous"), ("source_type", "release"), ("asset_pattern", "LuckyOdds-v*.zip"), ("folders", "LuckyOdds"),
    ];
    let page = web.post_form("/admin/addons/new", &fields).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER, "{}", page.body);
    assert!(page.location().starts_with("/admin/addons/luckyodds?msg=LuckyOdds+is+added."), "says it worked: {}", page.location());
}

#[tokio::test]
async fn blocking_sits_in_a_danger_zone_and_notes_name_their_version() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky", "owner": "alice", "blocked": false })).await;
    get_json(&web, "/v1/addons/lucky/versions", json!([{ "id": 9, "tag": "v2", "trust": "warn", "findings": { "critical": 0, "warn": 1 } }])).await;
    let page = web.get("/admin/addons/lucky").await.body;
    assert!(page.contains(r#"<section class="panel danger-zone">"#), "{page}");
    assert!(page.contains(r#"placeholder="Note for block v2""#), "{page}");
}

#[tokio::test]
async fn admins_search_and_fill_in_repos_of_any_account() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/categories", json!([{ "name": "Miscellaneous", "parent": null, "selectable": true, "sort_key": 60 }])).await;
    let page = web.get("/admin/addons/new").await;
    assert!(page.body.contains(r#"<input name="repo_owner""#) && page.body.contains(r#"name="repo_name""#), "the owner is editable: {}", page.body);

    Mock::given(method("GET"))
        .and(path("/v1/me/repos"))
        .and(wiremock::matchers::query_param("owner", "cadoky"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "owner": "cadoky", "repos": [{ "full_name": "cadoky/LuckyOdds", "description": null }] })))
        .mount(&web.api)
        .await;
    let found = web.get("/addons/new/repos?repo_owner=cadoky&repo_name=&page=admin").await;
    assert!(found.body.contains("cadoky/LuckyOdds"), "{}", found.body);
    let picked = web.get("/addons/new/repo?repo=cadoky/LuckyOdds&page=admin").await;
    assert!(picked.body.contains(r#"href="/admin/addons/new?repo=cadoky/LuckyOdds""#), "{}", picked.body);

    get_json(&web, "/v1/me/repos/cadoky/LuckyOdds/suggestion", json!({
        "repo": "cadoky/LuckyOdds", "name": "Lucky Odds", "slug": "luckyodds", "summary": "Odds", "source_type": "release",
        "asset_pattern": "LuckyOdds-v*.zip", "folders": ["LuckyOdds"], "read_from": "release v0.7.0, LuckyOdds-v0.7.0.zip"
    }))
    .await;
    let filled = web.get("/admin/addons/new?repo=cadoky/LuckyOdds").await;
    for text in [r#"value="cadoky""#, r#"value="LuckyOdds""#, r#"value="Lucky Odds""#, "release v0.7.0"] {
        assert!(filled.body.contains(text), "missing {text}: {}", filled.body);
    }
}

#[tokio::test]
async fn audit_filters() {
    let mut web = as_admin().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/audit"))
        .and(wiremock::matchers::query_param("action", "ban_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 2, "entries": [
            { "id": 12, "admin": "root", "action": "ban_user", "target_type": "user", "target_id": "5", "target": { "label": "eve", "login": "eve" },
              "note": "spam", "details": {}, "created_at": "2026-05-01T10:00:00Z" },
            { "id": 11, "admin": "root", "action": "verify", "target_type": "version", "target_id": "333",
              "target": { "label": "Lucky Odds v1.2.0", "addon_slug": "lucky", "addon_name": "Lucky Odds", "tag": "v1.2.0" },
              "note": "", "details": {}, "created_at": "2026-05-01T09:00:00Z" }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/admin/audit?action=ban_user").await;
    for text in [
        "ban_user",
        "spam",
        r#"<a href="/admin/users?login=eve">eve</a>"#,
        r#"<a href="/admin/addons/lucky">Lucky Odds v1.2.0</a>"#,
        r#"<a href="/admin/audit/12">Details</a>"#,
        r#"<td>Ban a user</td>"#,
        r#"<option value="ban_user" selected>Ban a user</option>"#,
        r#"<option value="version">Version</option>"#,
        "Entries 1 to 2 of 2",
    ] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
}

#[tokio::test]
async fn moderators_cannot_lift_a_staff_ban() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mod", "moderator")).await;
    get_json(&web, "/v1/admin/admins", json!([])).await;
    get_json(&web, "/v1/admin/users/bob", json!({ "github_id": 6, "login": "bob", "role": "moderator", "banned": true, "ban_reason": "x", "addons": 0 })).await;
    let page = web.get("/admin/users?login=bob").await.body;
    assert!(!page.contains("Lift the ban") && page.contains("Only an admin lifts the ban of a moderator or admin."), "{page}");
}

#[tokio::test]
async fn an_audit_entry_shows_everything_and_its_history() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/admin/audit/11", json!({
        "id": 11, "admin": "root", "action": "verify", "target_type": "version", "target_id": "333",
        "target": { "label": "Lucky Odds v1.2.0", "addon_slug": "lucky", "addon_name": "Lucky Odds", "tag": "v1.2.0" },
        "note": "read the code", "details": { "sha256": "abc123", "trust_before": "flagged" }, "created_at": "2026-05-01T09:00:00.123Z",
        "history": [{ "id": 9, "admin": "mia", "action": "block_addon", "target_type": "addon", "target_id": "4",
                      "target": { "label": "Lucky Odds", "addon_slug": "lucky" }, "note": "malware", "details": {}, "created_at": "2026-04-30T08:00:00Z" }]
    })).await;
    let page = web.get("/admin/audit/11").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in [
        "2026-05-01 09:00:00 UTC",
        "verify",
        "version 333",
        r#"<a href="/admin/addons/lucky">Lucky Odds</a>"#,
        r#"<a href="/addons/lucky/versions/v1.2.0">v1.2.0</a>"#,
        "read the code",
        "<th>sha256</th><td><code>abc123</code></td>",
        "<th>trust_before</th><td><code>flagged</code></td>",
        r#"<a href="/admin/audit/9">"#,
        "malware",
        r#"href="/admin/audit""#,
    ] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    Mock::given(method("GET")).and(path("/v1/admin/audit/77")).respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "not_found", "message": "Not found" }))).mount(&web.api).await;
    assert_eq!(web.get("/admin/audit/77").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn staff_is_listed_and_roles_are_set_on_the_users_page() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/admin/admins", json!([
        { "github_id": 99, "login": "root", "role": "admin", "by_config": true, "granted": false },
        { "github_id": 7, "login": "mia", "role": "moderator", "by_config": false, "granted": true },
    ]))
    .await;
    get_json(&web, "/v1/admin/users/eve", json!({ "github_id": 5, "login": "eve", "role": "user", "banned": false, "addons": 0 })).await;
    let page = web.get("/admin/users?login=eve").await;
    assert!(page.body.contains("root") && page.body.contains("ADMIN_GITHUB_IDS"), "{}", page.body);
    assert!(page.body.contains(r#"<span class="badge">moderator</span>"#), "roles as badges");
    assert!(page.body.contains(r#"action="/admin/users/7/role""#), "given roles can be changed");
    assert!(!page.body.contains(r#"action="/admin/users/99/role""#), "config admins cannot");
    assert!(page.body.contains(r#"action="/admin/users/5/role""#) && page.body.contains(r#"<option value="moderator">"#));

    expect(&web, "PUT", "/v1/admin/users/5/role", json!({ "role": "moderator", "note": "helps out" })).await;
    let done = web.post_form("/admin/users/5/role", &[("role", "moderator"), ("note", "helps out"), ("login", "eve")]).await;
    assert!(done.location().starts_with("/admin/users?login=eve&msg="), "{}", done.location());
    expect(&web, "PUT", "/v1/admin/users/7/role", json!({ "role": "user", "note": "left" })).await;
    web.post_form("/admin/users/7/role", &[("role", "user"), ("note", "left"), ("login", "")]).await;
}

#[tokio::test]
async fn moderator_sees_admin_pages() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mia", "moderator")).await;
    get_json(&web, "/v1/admin/reports", json!([])).await;
    get_json(&web, "/v1/admin/review-queue", json!([])).await;
    get_json(&web, "/v1/admin/verification-requests", json!([])).await;
    get_json(&web, "/v1/admin/admins", json!([])).await;
    let home = web.get("/").await;
    assert!(home.body.contains(r#"<a href="/admin">Admin</a>"#), "the navigation links the admin pages");
    for page in ["/admin", "/admin/review", "/admin/reports", "/admin/users"] {
        assert_eq!(web.get(page).await.status, StatusCode::OK, "{page}");
    }
    assert!(web.get("/admin").await.body.contains(r#"href="/packs?all=1""#), "every pack, also the unlisted ones");
}

#[tokio::test]
async fn only_role_managers_see_role_select() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mia", "moderator")).await;
    get_json(&web, "/v1/admin/admins", json!([{ "github_id": 7, "login": "mia", "role": "moderator", "by_config": false, "granted": true }])).await;
    get_json(&web, "/v1/admin/users/max", json!({ "github_id": 8, "login": "max", "role": "moderator", "banned": false, "addons": 0 })).await;
    let page = web.get("/admin/users?login=max").await;
    assert!(!page.body.contains("/role\""), "no role forms for moderators: {}", page.body);
    assert!(!page.body.contains(r#"action="/admin/users/8/ban""#), "moderators cannot ban other staff");
    assert!(page.body.contains("Role: <span class=\"badge\">moderator</span>"));

    get_json(&web, "/v1/admin/users/eve", json!({ "github_id": 5, "login": "eve", "role": "user", "banned": false, "addons": 0 })).await;
    let page = web.get("/admin/users?login=eve").await;
    assert!(page.body.contains(r#"action="/admin/users/5/ban""#), "plain users can be banned");
}

#[tokio::test]
async fn admin_checks_every_addon_for_updates() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/admin/addons", json!({ "total": 0, "page": 1, "per_page": 50, "addons": [] })).await;
    let page = web.get("/admin/addons").await;
    assert!(page.body.contains(r#"action="/admin/addons/sync""#), "{}", page.body);
    expect(&web, "POST", "/v1/admin/addons/sync", json!({})).await;
    let done = web.post_form("/admin/addons/sync", &[]).await;
    assert!(done.location().starts_with("/admin/addons?msg="), "{}", done.location());
}

fn admin_stats_body() -> Value {
    json!({
        "since": "2026-10-30T10:00:00Z",
        "website": { "per_day": [{ "day": "2026-10-31", "views": 40, "visitors": 12 }], "pages": [{ "page": "/addons/{slug}", "views": 30, "visitors": 9 }],
                     "targets": [{ "page": "/addons/{slug}", "target": "lucky", "views": 20, "visitors": 6 }],
                     "referrers": [{ "value": "reddit.com", "views": 5, "visitors": 4 }], "countries": [], "devices": [{ "value": "steam", "views": 3, "visitors": 1 }] },
        "requests": { "total": 1234, "rate_limited": 3, "per_hour": [],
                      "routes": [{ "route": "/v1/registry", "method": "GET", "count": 900, "avg_ms": 12, "max_ms": 80 }],
                      "statuses": [{ "status": 200, "count": 1200 }], "clients": [{ "client": "launcher", "count": 1000 }] },
        "jobs": [{ "kind": "sync_addon", "open": 1, "running": 0, "done": 40, "failed": 2, "last_error": "GitHub down" }],
        "scans": { "scans": 5, "flagged": 1, "errors": 0, "findings": 3, "blocked_versions": 0 },
        "telemetry": { "kinds": [], "failing_addons": [], "error_codes": [], "launcher_versions": [] },
        "github": { "total": 77, "per_hour": [{ "hour": "2026-10-31T09:00:00Z", "count": 30 }, { "hour": "2026-10-31T10:00:00Z", "count": 47 }],
                    "quotas": [{ "quota": "read_token", "resource": "core", "limit": 5000, "used": 679, "remaining": 4321, "reset_at": "2099-01-01T00:00:00Z", "seen_at": "2026-10-31T10:05:00Z" }],
                    "origins": [{ "origin": "job sync_addon", "count": 60 }],
                    "endpoints": [{ "endpoint": "/repositories/{id}/releases", "quota": "installation:7", "count": 50, "errors": 2 }] }
    })
}

#[tokio::test]
async fn stats_page_needs_permission() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("eve", "user")).await;
    assert_eq!(web.get("/admin/stats").await.status, StatusCode::FORBIDDEN);

    let mut web = TestWeb::new().await;
    let mut no_stats = me("mia", "moderator");
    no_stats["permissions"] = json!(["moderate"]);
    web.login_as("tok", no_stats).await;
    assert_eq!(web.get("/admin/stats").await.status, StatusCode::FORBIDDEN, "a moderator outside ADMIN_STATS_ROLES");
    get_json(&web, "/v1/admin/review-queue", json!([])).await;
    get_json(&web, "/v1/admin/reports", json!([])).await;
    get_json(&web, "/v1/admin/verification-requests", json!([])).await;
    assert!(!web.get("/admin").await.body.contains("/admin/stats"), "no tile without the right");
}

#[tokio::test]
async fn stats_range_switch() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mia", "moderator")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).and(query_param("range", "7d")).respond_with(ResponseTemplate::new(200).set_body_json(admin_stats_body())).mount(&web.api).await;
    let mut body = String::new();
    for part in ["", "/website", "/api", "/jobs", "/github", "/launcher"] {
        let page = web.get(&format!("/admin/stats{part}?range=7d")).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        body.push_str(&page.body);
    }
    let page = common::Page { status: StatusCode::OK, headers: Default::default(), body };
    for text in ["1,234", "GET /v1/registry", "GitHub down", "4,321 of 5,000", "/addons/lucky", "reddit.com", "steam", "Read token (core)", "job sync_addon", "/repositories/{id}/releases", "2026-10-31 10:00", "No failed installs.", r#"<a href="/admin/stats?range=7d" aria-current="page">7 days</a>"#] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    get_json(&web, "/v1/admin/review-queue", json!([])).await;
    get_json(&web, "/v1/admin/reports", json!([])).await;
    get_json(&web, "/v1/admin/verification-requests", json!([])).await;
    assert!(web.get("/admin").await.body.contains(r#"href="/admin/stats""#), "linked for moderators by default");
}

/// The overview and the five part pages, one after the other.
async fn admin_stats_page(body: Value) -> String {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("root", "admin")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&web.api).await;
    let mut pages = String::new();
    for page in ["/admin/stats", "/admin/stats/website", "/admin/stats/api", "/admin/stats/jobs", "/admin/stats/github", "/admin/stats/launcher"] {
        let reply = web.get(page).await;
        assert_eq!(reply.status, StatusCode::OK, "{page}");
        pages.push_str(&reply.body);
    }
    pages
}

#[tokio::test]
async fn problems_come_first_and_sections_are_grouped() {
    let mut body = admin_stats_body();
    body["github"]["quotas"][0]["remaining"] = json!(120);
    body["telemetry"]["failing_addons"] = json!([{ "slug": "lucky", "failed": 3, "total": 4 }]);
    body["scans"]["errors"] = json!(2);
    let page = admin_stats_page(body).await;
    let attention = page.find("Needs attention").expect("a problem block");
    for text in ["sync_addon: 2 failed runs", "GitHub quota Read token (core): 120 of 5,000 left", "2 scans failed", "lucky: 3 of 4 installs or updates failed"] {
        let at = page.find(text).unwrap_or_else(|| panic!("missing {text}: {page}"));
        assert!(at > attention && at < page.find(r#"id="website""#).unwrap(), "{text} sits in the block at the top");
    }
    for (part, title) in [("website", "Website"), ("api", "API"), ("jobs", "Jobs and scans"), ("github", "GitHub"), ("launcher", "Launcher")] {
        assert!(page.contains(&format!(r#"<a href="/admin/stats/{part}?range=24h">{title}</a>"#)), "tab {part}");
        assert!(page.contains(&format!(r#"<a href="/admin/stats/{part}?range=24h">All {title} numbers</a>"#)), "overview links {part}");
        assert!(page.contains(&format!(r#"<h2 id="{part}">{title}</h2>"#)), "page {part}");
    }
    assert!(page.contains("<h3>Busiest routes</h3>"), "tables under their group");
    assert!(page.contains(r#"<li><a href="/admin/stats/jobs?range=24h">sync_addon: 2 failed runs"#), "each problem links to its part: {page}");
    assert!(page.contains(r#"<li><a href="/admin/stats/launcher?range=24h">lucky: 3 of 4"#), "{page}");
    assert!(page.contains(r#"<tr class="failed"><td>sync_addon</td>"#), "a job with failed runs stands out: {page}");
}

#[tokio::test]
async fn the_admin_start_shows_system_problems() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("root", "admin")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).respond_with(ResponseTemplate::new(200).set_body_json(admin_stats_body())).mount(&web.api).await;
    let page = web.get("/admin").await.body;
    assert!(page.contains(r#"<a href="/admin/stats/jobs?range=24h">sync_addon: 2 failed runs"#), "{page}");
}

#[tokio::test]
async fn no_problem_block_when_all_is_well() {
    let mut body = admin_stats_body();
    body["jobs"][0]["failed"] = json!(0);
    let page = admin_stats_page(body).await;
    assert!(!page.contains("Needs attention"), "{page}");
    assert!(page.contains("Nothing needs attention"), "says all is well: {page}");
}

#[tokio::test]
async fn present_state_is_told_apart_from_the_range() {
    let page = admin_stats_page(admin_stats_body()).await;
    assert!(page.contains("Since 2026-10-30 10:00 UTC"), "the range in plain dates: {page}");
    for text in [r#"<th class="num">Waiting now</th>"#, r#"<th class="num">Running now</th>"#, "<h3>GitHub quotas now</h3>", "public GitHub requests left now"] {
        assert!(page.contains(text), "missing {text}");
    }
}

#[tokio::test]
async fn only_number_columns_align_right() {
    let page = admin_stats_page(admin_stats_body()).await;
    assert!(page.contains(r#"<td class="num">2</td><td>GitHub down</td>"#), "error text stays left: {page}");
    assert!(page.contains(r#"<th>Endpoint</th><th>Quota</th><th class="num">Requests</th>"#), "quota is text");
    assert!(page.contains(r#"<td>/repositories/{id}/releases</td><td>App installation 7</td><td class="num">50</td>"#));
}

#[tokio::test]
async fn hourly_github_table_is_capped() {
    let mut body = admin_stats_body();
    let hours: Vec<Value> = (0..60).map(|h| json!({ "hour": format!("2026-10-{:02}T{:02}:00:00Z", 28 + h / 24, h % 24), "count": h })).collect();
    body["github"]["per_hour"] = json!(hours);
    let page = admin_stats_page(body).await;
    let table = &page[page.find("<h3>GitHub requests per hour</h3>").unwrap()..];
    let table = &table[..table.find("</section>").unwrap()];
    assert_eq!(table.matches("<tr>").count(), 1 + 48, "head and the newest 48 hours");
    assert!(table.contains("2026-10-30 11:00") && !table.contains("2026-10-28 00:00"), "newest first");
    assert!(table.contains("The last 48 hours; Grafana has the rest."));
}

#[tokio::test]
async fn grafana_is_linked_for_those_who_may_open_it() {
    let page = admin_stats_page(admin_stats_body()).await;
    assert!(page.contains(r#"<a href="/grafana/">Grafana</a> has response times"#), "admins: {page}");

    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mia", "moderator")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).respond_with(ResponseTemplate::new(200).set_body_json(admin_stats_body())).mount(&web.api).await;
    assert!(!web.get("/admin/stats").await.body.contains("Grafana"), "moderators cannot go there");
}

#[tokio::test]
async fn range_switch_is_big_enough_to_tap() {
    let mut web = TestWeb::new().await;
    let css = web.get("/static/app.css").await.body;
    assert!(css.contains(".range-toggle a { min-height: 44px;"), "a 44 px target on phones");
}

#[tokio::test]
async fn website_numbers_say_they_count_whole_days() {
    let page = admin_stats_page(admin_stats_body()).await;
    let website = &page[page.find(r#"<h2 id="website">"#).unwrap()..page.find(r#"<h2 id="api">"#).unwrap()];
    assert!(website.contains("Counted by whole days (UTC)"), "{website}");
    assert!(website.contains("<h3>Website visits per day (UTC)</h3>"));
}

#[tokio::test]
async fn names_instead_of_raw_values() {
    let mut body = admin_stats_body();
    body["github"]["installations"] = json!({ "7": "alice" });
    body["telemetry"]["kinds"] = json!([{ "kind": "install_failed", "count": 2 }]);
    body["website"]["countries"] = json!([{ "value": "DE", "views": 7, "visitors": 2 }]);
    let page = admin_stats_page(body).await;
    assert!(page.contains("<td>App installation of alice</td>"), "{page}");
    assert!(page.contains("<td>Read token (core)</td>"));
    assert!(page.contains("<td>Install failed</td>"));
    assert!(page.contains(r#"<table class="stats-table" data-cells="country">"#), "named in the browser");
}

#[tokio::test]
async fn error_statuses_stand_out() {
    let mut body = admin_stats_body();
    body["requests"]["statuses"] = json!([{ "status": 200, "count": 9 }, { "status": 429, "count": 2 }, { "status": 502, "count": 1 }]);
    let page = admin_stats_page(body).await;
    assert!(page.contains(r#"<tr><td>200</td>"#), "{page}");
    assert!(page.contains(r#"<tr class="warn"><td>429</td>"#));
    assert!(page.contains(r#"<tr class="failed"><td>502</td>"#));
}

#[tokio::test]
async fn tables_are_named_for_screen_readers() {
    let page = admin_stats_page(admin_stats_body()).await;
    assert!(page.contains(r#"<caption class="sr-only">Busiest routes</caption>"#), "{page}");
}

#[tokio::test]
async fn visits_are_called_visits_everywhere() {
    let page = admin_stats_page(admin_stats_body()).await;
    assert!(page.contains(r#"<th class="num">Visits</th>"#), "{page}");
    assert!(!page.contains("Visitors"), "the public page says visits too");
}

#[tokio::test]
async fn parts_are_pages_of_their_own() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("root", "admin")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).and(query_param("section", "github")).and(query_param("range", "7d"))
        .respond_with(ResponseTemplate::new(200).set_body_json(admin_stats_body())).expect(1).mount(&web.api).await;
    let page = web.get("/admin/stats/github?range=7d").await.body;
    assert!(page.contains(r#"<a href="/admin/stats/github?range=7d" aria-current="page">GitHub</a>"#), "{page}");
    assert!(page.contains(r#"<a href="/admin/stats/github?range=30d">30 days</a>"#), "the range stays on the part");
    assert!(!page.contains("Busiest routes") && !page.contains("Needs attention"), "only its own part");
    assert_eq!(web.get("/admin/stats/secrets").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn verification_requests_are_listed_and_decided() {
    let mut web = as_admin().await;
    let link = "https://discord.com/channels/1557821819032051852/1564396545837768864/1570000000000000000";
    get_json(&web, "/v1/admin/verification-requests", json!([
        { "id": 4, "slug": "lucky", "name": "Lucky", "version_id": 9, "tag": "v2", "trust": "flagged", "discord_url": link, "requested_by": "alice", "created_at": "2026-05-01T10:00:00Z" }
    ]))
    .await;
    assert!(web.get("/admin").await.body.contains(r#"<a class="panel tile due" href="/admin/verification-requests"><strong>1</strong> verification request"#));
    let page = web.get("/admin/verification-requests").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["Lucky", "v2", "alice", &format!(r#"href="{link}""#), r#"action="/admin/verification-requests/4/approve""#, r#"action="/admin/verification-requests/4/reject""#, "Flagged"] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    expect(&web, "POST", "/v1/admin/verification-requests/4/approve", json!({ "note": "looked at it" })).await;
    let reply = web.post_form("/admin/verification-requests/4/approve", &[("note", "looked at it")]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert!(reply.location().starts_with("/admin/verification-requests?msg="), "{}", reply.location());
    expect(&web, "POST", "/v1/admin/verification-requests/4/reject", json!({ "note": "no post" })).await;
    assert_eq!(web.post_form("/admin/verification-requests/4/reject", &[("note", "no post")]).await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_verification_request_that_is_no_link_shows_as_text() {
    let mut web = as_admin().await;
    get_json(&web, "/v1/admin/verification-requests", json!([
        { "id": 5, "slug": "lucky", "name": "Lucky", "version_id": 9, "tag": "v2", "trust": "scanned", "discord_url": "javascript:alert(1) see my thread", "requested_by": "alice", "created_at": "2026-05-01T10:00:00Z" }
    ]))
    .await;
    let page = web.get("/admin/verification-requests").await;
    assert!(page.body.contains("javascript:alert(1) see my thread") && !page.body.contains(r#"href="javascript"#), "{}", page.body);
}

/// Forms with several actions: Enter in the note field must not send the first one, so the
/// first submit button is a disabled one (implicit submission then does nothing).
fn enter_sends_nothing(body: &str) {
    for form in body.split("<form").skip(1).map(|f| f.split("</form>").next().unwrap_or_default()) {
        let buttons: Vec<&str> = form.split("<button").skip(1).filter(|b| b.contains("type=\"submit\"")).collect();
        if buttons.len() > 1 {
            assert!(buttons[0].contains("disabled"), "first submit button must be disabled: <form{form}");
        }
    }
}

#[tokio::test]
async fn enter_in_a_note_field_never_picks_an_action() {
    let mut web = as_admin().await;
    grouped_queue(&web).await;
    for page in ["/admin/review", "/admin/review/lucky", "/admin/reports"] {
        let page = web.get(page).await;
        assert_eq!(page.status, StatusCode::OK);
        enter_sends_nothing(&page.body);
    }
}

#[tokio::test]
async fn destructive_admin_actions_ask_first() {
    let mut web = as_admin().await;
    grouped_queue(&web).await;
    let review = web.get("/admin/review/lucky").await.body;
    assert!(review.contains("data-confirm=\"Block every queued version") && review.contains("data-confirm=\"Block v2?"), "{review}");
    get_json(&web, "/v1/admin/users/eve", json!({ "github_id": 5, "login": "eve", "role": "user", "banned": false, "addons": 2 })).await;
    let users = web.get("/admin/users?login=eve").await.body;
    assert!(users.contains("data-confirm=\"Ban eve?") && users.contains("data-confirm=\"Change the role of eve?"), "{users}");
}

#[tokio::test]
async fn an_admin_cannot_pick_a_new_role_for_themself() {
    let mut web = as_admin().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/admins"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "github_id": 99, "login": "root", "role": "admin", "by_config": false, "granted": true }])))
        .with_priority(1)
        .mount(&web.api)
        .await;
    let page = web.get("/admin/users").await.body;
    assert!(page.contains("You (another admin changes your role)") && !page.contains("/admin/users/99/role"), "{page}");
}

#[tokio::test]
async fn version_and_image_reports_show_what_was_reported() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("root", "admin")).await;
    get_json(&web, "/v1/admin/reports", json!([
        { "id": 4, "target_type": "version", "target_id": 12, "addon": "lucky", "version_tag": "v2", "reason": "broken", "text": "", "reporter": "eve", "created_at": "2026-05-01T10:00:00Z" },
        { "id": 5, "target_type": "image", "target_id": 7, "addon": "lucky", "image_url": "https://img.test/abc.webp", "reason": "spam", "text": "", "reporter": "eve", "created_at": "2026-05-01T10:00:00Z" }
    ]))
    .await;
    let page = web.get("/admin/reports").await.body;
    assert!(page.contains(r#"<a href="/addons/lucky/versions/v2">lucky</a> <span class="muted">(version v2)</span>"#), "links the version: {page}");
    assert!(page.contains(r#"<img class="reported-image" src="https://img.test/abc.webp" alt="The reported image">"#), "shows the image: {page}");
}

#[tokio::test]
async fn a_report_closed_by_someone_else_says_so() {
    let mut web = as_admin().await;
    Mock::given(method("POST"))
        .and(path("/v1/admin/reports/3/resolve"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "not_found", "message": "Not found" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/admin/reports/3/resolve", &[("status", "dismissed"), ("note", "fine")]).await;
    let follow = web.get(page.location()).await.body;
    assert!(follow.contains("Someone else closed this report already."), "{follow}");
}
