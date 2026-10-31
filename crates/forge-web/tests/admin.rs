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
    assert_eq!(anonymous.get("/admin").await.location(), "/login");
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
    for text in ["href=\"/admin/review/lucky\"", "2 versions", "href=\"/admin/review/other\"", "1 version", "action=\"/admin/review/lucky/skip\""] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    assert!(!page.body.contains("a.lua:3"), "versions only on the addon's page");
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
    let page = web.get(done.location()).await;
    assert!(page.body.contains("1 of 2 versions of Lucky blocked. v3: Not found"), "{}", page.body);
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
    assert!(web.get("/admin/reports").await.body.contains("Deletes files"));

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
        .and(path("/v1/addons"))
        .and(wiremock::matchers::query_param("q", "luck"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 1, "addons": [{ "slug": "lucky", "name": "Lucky Odds", "trust": "flagged" }] })))
        .mount(&web.api)
        .await;
    let page = web.get("/admin/addons?q=luck").await;
    assert!(page.body.contains("href=\"/admin/addons/lucky\"") && page.body.contains("Lucky Odds"), "{}", page.body);
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
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/luckyodds"), "{}", page.body);
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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 1, "entries": [
            { "admin": "root", "action": "ban_user", "target_type": "user", "target_id": "5", "note": "spam", "created_at": "2026-05-01T10:00:00Z" }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/admin/audit?action=ban_user").await;
    assert!(page.body.contains("ban_user") && page.body.contains("user 5") && page.body.contains("spam"));
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
    get_json(&web, "/v1/admin/admins", json!([])).await;
    let home = web.get("/").await;
    assert!(home.body.contains(r#"<a href="/admin">Admin</a>"#), "the navigation links the admin pages");
    for page in ["/admin", "/admin/review", "/admin/reports", "/admin/users"] {
        assert_eq!(web.get(page).await.status, StatusCode::OK, "{page}");
    }
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
    get_json(&web, "/v1/addons", json!({ "addons": [] })).await;
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
    assert!(!web.get("/admin").await.body.contains("/admin/stats"), "no tile without the right");
}

#[tokio::test]
async fn stats_range_switch() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("mia", "moderator")).await;
    Mock::given(method("GET")).and(path("/v1/admin/stats")).and(query_param("range", "7d")).respond_with(ResponseTemplate::new(200).set_body_json(admin_stats_body())).mount(&web.api).await;
    let page = web.get("/admin/stats?range=7d").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["1,234", "GET /v1/registry", "GitHub down", "4,321 of 5,000", "/addons/lucky", "reddit.com", "steam", "read_token (core)", "job sync_addon", "/repositories/{id}/releases", "2026-10-31 10:00", "No failed installs.", r#"<a href="/admin/stats?range=7d" aria-current="page">7 days</a>"#] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    get_json(&web, "/v1/admin/review-queue", json!([])).await;
    get_json(&web, "/v1/admin/reports", json!([])).await;
    assert!(web.get("/admin").await.body.contains(r#"href="/admin/stats""#), "linked for moderators by default");
}
