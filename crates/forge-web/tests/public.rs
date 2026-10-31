mod common;

use axum::http::StatusCode;
use common::TestWeb;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const ASSET: &str = "https://github.com/alice/lucky/releases/download/v1/lucky-v1.zip";

async fn mock(web: &TestWeb, route: &str, body: Value) {
    Mock::given(method("GET")).and(path(route)).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&web.api).await;
}

fn addon(trust: &str) -> Value {
    json!({
        "slug": "lucky", "name": "Lucky Odds", "summary": "Odds for rare cards", "category": "Miscellaneous", "repo": "alice/lucky",
        "owner": { "login": "alice" }, "author": "alice", "claimed": true, "blocked": false,
        "description_html": "<h2>Features</h2><p>Shows odds.</p>", "description_md": "## Features",
        "icon_url": null, "images": [{ "id": 1, "url": "https://web.test/img/a.webp", "width": 800, "height": 450, "caption": "Card screen" }],
        "contributors": [{ "login": "bob" }],
        "dependencies": [{ "slug": "lib", "name": "Lib", "required": true, "trust": "verified", "available": true }],
        "trust": trust, "latest_tag": "v1", "verified_author": false, "findings": { "critical": 0, "warn": 1, "info": 0 }
    })
}

fn versions(trust: &str, source: &str) -> Value {
    json!([
        { "id": 2, "tag": "v1", "name": "First", "prerelease": false, "published_at": "2026-05-01T10:00:00Z", "asset_url": ASSET,
          "sha256": "ab".repeat(32), "trust": trust, "source": source, "deleted_on_github": false,
          "findings": { "critical": 0, "warn": if trust == "flagged" { 1 } else { 0 }, "info": 0 }, "game_min": "0.69.0", "game_max": null },
        { "id": 1, "tag": "v0", "name": "Old", "prerelease": true, "published_at": "2026-04-01T10:00:00Z", "asset_url": "https://github.com/old.zip",
          "sha256": null, "trust": "unscanned", "source": "available", "deleted_on_github": true,
          "findings": { "critical": 0, "warn": 0, "info": 0 }, "game_min": null, "game_max": null }
    ])
}

fn findings() -> Value {
    json!({ "tag": "v1", "scan": { "result": "flagged", "findings": [
        { "rule": "lua.url", "severity": "warn", "file": "Lucky/Lucky.lua", "line": 12, "message": "Contains a web address", "snippet": "local u = 'https://evil.test'" }
    ] } })
}

async fn with_addon(trust: &str, source: &str) -> TestWeb {
    let web = TestWeb::new().await;
    mock(&web, "/v1/addons/lucky", addon(trust)).await;
    mock(&web, "/v1/addons/lucky/versions", versions(trust, source)).await;
    mock(&web, "/v1/addons/lucky/versions/v1/findings", findings()).await;
    web
}

#[tokio::test]
async fn list_renders_items_without_js() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    Mock::given(method("GET"))
        .and(path("/v1/addons"))
        .and(query_param("q", "lucky"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 1, "page": 1, "per_page": 20, "addons": [
            { "slug": "lucky", "name": "Lucky Odds", "summary": "Odds", "category": "Miscellaneous", "trust": "flagged", "icon_url": null,
              "owner": { "login": "alice" }, "author": "alice", "verified_author": false, "downloads": 42, "updated_at": "2026-05-01" }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/addons?q=lucky").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    // Only the results scroll; search and filters stay in view.
    for text in ["Lucky Odds", "by alice", "42 downloads", "Flagged", "<form class=\"search\" method=\"get\" action=\"/addons\"", "<div id=\"results\" class=\"list\">",
        "href=\"https://github.com/Rezondes/SurvivorsForge\"", "href=\"https://rezondes.github.io/SurvivorsForge/\""] {
        assert!(page.body.contains(text), "missing {text}");
    }

    Mock::given(method("GET")).and(path("/v1/addons")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 0, "addons": [] }))).mount(&web.api).await;
    assert!(web.get("/addons?q=nothing").await.body.contains("No addon matches"));
}

#[tokio::test]
async fn addons_without_a_download_count_show_their_last_update() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 1, "page": 1, "per_page": 20, "addons": [
        { "slug": "tagged", "name": "Tagged", "summary": "From tags", "category": "Miscellaneous", "trust": "scanned", "icon_url": null,
          "owner": null, "author": "bob", "verified_author": false, "downloads": null, "updated_at": "2026-05-01T10:00:00Z" }
    ] }))
    .await;
    let page = web.get("/addons").await;
    assert!(page.body.contains("Updated 2026-05-01") && !page.body.contains("downloads</span>"), "{}", page.body);
}

fn categories() -> Value {
    json!([
        { "name": "Classes", "parent": null, "selectable": false, "sort_key": 10 },
        { "name": "Warrior", "parent": "Classes", "selectable": true, "sort_key": 1 },
        { "name": "Combat", "parent": null, "selectable": true, "sort_key": 20 },
    ])
}

#[tokio::test]
async fn category_filter_lists_subcategories_and_reaches_the_api() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    Mock::given(method("GET"))
        .and(path("/v1/addons"))
        .and(query_param("category", "Classes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 0, "addons": [] })))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.get("/addons?category=Classes").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    // Subcategories sit in a group of their parent; its first option filters the whole group.
    assert!(page.body.contains(r#"<optgroup label="Classes">"#), "{}", page.body);
    assert!(page.body.contains(r#"<option value="Classes" selected>All Classes</option>"#), "{}", page.body);
    assert!(page.body.contains(r#"<option value="Warrior">Warrior</option>"#));
    assert!(page.body.contains(r#"<option value="Combat">Combat</option>"#), "a category without subcategories stays a plain option");
    assert!(page.body.contains("All categories"));
}

#[tokio::test]
async fn addon_page_sections() {
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["<h2>Features</h2>", "Card screen", "bob", "Lib", "(required)", "0.69.0 and later", "Version v1", "Miscellaneous"] {
        assert!(page.body.contains(text), "missing {text}");
    }
}

#[tokio::test]
async fn admins_get_a_manage_link_on_addon_pages() {
    let mut web = with_addon("scanned", "available").await;
    assert!(!web.get("/addons/lucky").await.body.contains("/admin/addons/lucky"));
    web.login_as("tok", common::me("root", "admin")).await;
    assert!(web.get("/addons/lucky").await.body.contains("/admin/addons/lucky"));
}

#[tokio::test]
async fn the_repo_owner_may_claim_an_unclaimed_addon() {
    async fn unclaimed() -> TestWeb {
        let web = TestWeb::new().await;
        let mut a = addon("scanned");
        a["claimed"] = json!(false);
        a["owner"] = Value::Null;
        mock(&web, "/v1/addons/lucky", a).await;
        mock(&web, "/v1/addons/lucky/versions", versions("scanned", "available")).await;
        web
    }
    let claim = r#"action="/addons/lucky/claim""#;
    let mut web = unclaimed().await;
    assert!(!web.get("/addons/lucky").await.body.contains(claim), "not for visitors");
    web.login_as("bob-token", common::me("bob", "user")).await;
    assert!(!web.get("/addons/lucky").await.body.contains(claim), "not for others");

    // GitHub logins ignore case; the repo is alice/lucky.
    let mut web = unclaimed().await;
    web.login_as("alice-token", common::me("ALICE", "user")).await;
    let page = web.get("/addons/lucky?error=Install+the+App+first").await;    assert!(page.body.contains(claim) && page.body.contains("Claim it"), "the repo alice/lucky is hers: {}", page.body);
    assert!(page.body.contains("Install the App first"), "the claim error shows");
}

#[tokio::test]
async fn trust_notice_per_level() {
    let cases = [
        ("unscanned", "available", "The automatic check has not run yet", true),
        ("flagged", "available", "found something suspicious", true),
        ("scanned", "available", "The automatic check found nothing", true),
        ("verified", "available", "An admin checked this exact file by hand", true),
        ("blocked", "available", "An admin blocked this version", false),
        ("source_changed", "available", "was changed after it was checked", false),
        ("scanned", "missing", "no longer available on GitHub", false),
    ];
    for (trust, source, text, download) in cases {
        let mut web = with_addon(trust, source).await;
        let page = web.get("/addons/lucky").await;
        assert!(page.body.contains(text), "{trust}/{source}: missing {text}");
        // The text explains the badge instead of repeating it ("Verified. Verified by ...").
        let notice = page.body.split("<strong>").nth(1).unwrap_or_default();
        let (badge, rest) = notice.split_once(".</strong>").unwrap_or_default();
        assert!(!rest.trim_start().starts_with(badge), "{trust}: the text repeats {badge}");
        assert_eq!(page.body.contains("Download from GitHub"), download, "{trust}/{source}");
    }
    let mut web = with_addon("flagged", "available").await;
    let page = web.get("/addons/lucky").await;
    assert!(page.body.contains("Lucky/Lucky.lua:12") && page.body.contains("Contains a web address"), "findings listed");
}

#[tokio::test]
async fn risky_download_needs_confirmation() {
    let mut web = with_addon("flagged", "available").await;
    let page = web.get("/addons/lucky").await;
    assert!(page.body.contains("href=\"/addons/lucky/versions/v1/download\""));
    let confirm = web.get("/addons/lucky/versions/v1/download").await;
    assert_eq!(confirm.status, StatusCode::OK);
    assert!(confirm.body.contains(r#"href="/addons/lucky/versions/v1/download?confirmed=1""#), "the button goes through the website");
    let confirmed = web.get("/addons/lucky/versions/v1/download?confirmed=1").await;
    assert_eq!((confirmed.status, confirmed.location()), (StatusCode::SEE_OTHER, ASSET));

    let mut safe = with_addon("verified", "available").await;
    assert!(safe.get("/addons/lucky").await.body.contains("href=\"/addons/lucky/versions/v1/download\" rel=\"external noopener\""), "counted on the way");
    let direct = safe.get("/addons/lucky/versions/v1/download").await;
    assert_eq!((direct.status, direct.location()), (StatusCode::SEE_OTHER, ASSET), "even when the API cannot count it");

    let mut blocked = with_addon("blocked", "available").await;
    assert_eq!(blocked.get("/addons/lucky/versions/v1/download").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn version_history() {
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky/versions").await;
    for text in ["v1", "v0", "2026-05-01", "Pre-release", "Removed on GitHub", &"ab".repeat(6), "0.69.0 and later", "not stated by the author"] {
        assert!(page.body.contains(text), "missing {text}");
    }
    let version = web.get("/addons/lucky/versions/v1").await;
    assert_eq!(version.status, StatusCode::OK);
    assert!(version.body.contains("lua.url"));
    assert!(version.body.contains(&"ab".repeat(32)), "full hash on the version page");
}

#[tokio::test]
async fn seo_meta() {
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await;
    assert!(page.body.contains("<title>Lucky Odds - SurvivorsForge</title>"));
    assert!(page.body.contains("<meta name=\"description\" content=\"Odds for rare cards\">"));
    assert!(page.body.contains("<link rel=\"canonical\" href=\"https://web.test/addons/lucky\">"));
}

#[tokio::test]
async fn unknown_slug_404_and_blocked_page() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/addons/ghost"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "not_found", "message": "Not found" })))
        .mount(&web.api)
        .await;
    assert_eq!(web.get("/addons/ghost").await.status, StatusCode::NOT_FOUND);

    mock(&web, "/v1/addons/bad", json!({ "slug": "bad", "name": "Bad", "blocked": true, "block_reason": "malware" })).await;
    let page = web.get("/addons/bad").await;
    assert!(page.body.contains("An admin blocked this addon") && page.body.contains("malware"));
    assert!(!page.body.contains("Download"));
}

#[tokio::test]
async fn api_down_shows_error_page() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET")).and(path("/v1/addons")).respond_with(ResponseTemplate::new(500).set_body_json(json!({ "error": "internal", "message": "Internal server error" }))).mount(&web.api).await;
    let page = web.get("/addons").await;
    assert_eq!(page.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(page.body.contains("Something went wrong"));
}

#[tokio::test]
async fn file_pickers_look_like_the_site() {
    // The browser's grey "Choose file" button gets the outlined gold look of the other buttons.
    let mut web = TestWeb::new().await;
    let css = web.get("/static/app.css").await.body;
    assert!(css.contains("::file-selector-button"), "no style for the file picker button");
}

#[tokio::test]
async fn scrollbars_use_the_theme() {
    // Thin gold-brown thumbs on the dark field colour, like the launcher's scroll panels.
    let mut web = TestWeb::new().await;
    let css = web.get("/static/app.css").await.body;
    assert!(css.contains("scrollbar-color: var(--outline) var(--field)"), "no themed scrollbars");
}

#[tokio::test]
async fn card_lists_switch_between_tiles_and_a_list() {
    // The choice lives in the browser (view.js, localStorage); the pages only offer the switch.
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    mock(&web, "/v1/packs", json!({ "total": 0, "packs": [] })).await;
    for page in ["/addons", "/packs"] {
        let body = web.get(page).await.body;
        assert!(body.contains(r#"data-set-view="list""#) && body.contains(r#"data-set-view="tiles""#), "{page}: {body}");
        assert!(body.contains(r#"<script src="/static/view.js?v="#), "{page}: before the page is drawn");
    }
    let script = web.get("/static/view.js").await;
    assert!(script.headers["content-type"].to_str().unwrap().starts_with("text/javascript") && script.body.contains("localStorage"));
}

#[tokio::test]
async fn a_deploy_reaches_browsers_that_cached_the_old_files() {
    // The files are cached for a day, so their links carry a version that changes with their content.
    let mut web = TestWeb::new().await;
    let body = web.get("/").await.body;
    let version = body.split(r#"href="/static/app.css?v="#).nth(1).and_then(|r| r.split('"').next()).expect("versioned stylesheet").to_string();
    assert!(version.len() >= 8 && version.chars().all(|c| c.is_ascii_hexdigit()), "{version}");
    for file in ["view.js", "htmx.min.js"] {
        assert!(body.contains(&format!(r#"src="/static/{file}?v={version}""#)), "{file}");
    }
    assert!(web.get(&format!("/static/app.css?v={version}")).await.body.contains("--gold"));
}

#[tokio::test]
async fn searching_shows_that_it_loads() {
    // While htmx fetches new results the old ones are dimmed, so a slow answer does not look frozen.
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    mock(&web, "/v1/packs", json!({ "total": 0, "packs": [] })).await;
    for page in ["/addons", "/packs"] {
        assert!(web.get(page).await.body.contains(r##"hx-indicator="#results""##), "{page}");
    }
    assert!(web.get("/static/app.css").await.body.contains("#results.htmx-request"));
}

#[tokio::test]
async fn a_failed_reload_says_so() {
    // htmx keeps the old results when the reload fails; the script puts a message above them.
    let mut web = TestWeb::new().await;
    let script = web.get("/static/view.js").await.body;
    for event in ["htmx:responseError", "htmx:sendError"] {
        assert!(script.contains(event), "{event} not handled");
    }
    assert!(script.contains("Could not load the results"));
}

#[tokio::test]
async fn the_view_switch_tells_which_view_is_on() {
    // Screen readers hear the pressed state; the buttons come again with every htmx swap.
    let mut web = TestWeb::new().await;
    let script = web.get("/static/view.js").await.body;
    for text in ["aria-pressed", "DOMContentLoaded", "htmx:afterSwap"] {
        assert!(script.contains(text), "missing {text}");
    }
}

#[tokio::test]
async fn the_result_count_is_announced() {
    // A live region outside the swapped results, updated with them: screen readers hear the new count.
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 3, "addons": [] })).await;
    mock(&web, "/v1/packs", json!({ "total": 1, "packs": [] })).await;
    for (page, count) in [("/addons", "3 addons"), ("/packs", "1 pack")] {
        let body = web.get(page).await.body;
        assert!(body.contains(&format!(r#"<p id="result-status" class="sr-only" role="status">{count}</p>"#)), "{page}: {body}");
        assert!(body.contains(r##"hx-select-oob="#result-status""##), "{page}");
    }
}

#[tokio::test]
async fn the_whole_card_opens_the_addon() {
    // The title link stretches over the card; links inside it (the category) stay on top.
    let mut web = TestWeb::new().await;
    let css = web.get("/static/app.css").await.body;
    assert!(css.contains(".card h2 a::after") && css.contains(".card p a { position: relative; z-index: 1; }"), "no card-wide link");
}

#[tokio::test]
async fn the_trust_filter_says_what_it_filters() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    assert!(web.get("/addons").await.body.contains(r#"<label for="trust">Safety check</label>"#));
}

#[tokio::test]
async fn large_download_counts_are_grouped() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 1, "addons": [
        { "slug": "big", "name": "Big", "summary": "", "category": "Combat", "trust": "scanned", "icon_url": null, "owner": null,
          "author": "bob", "verified_author": false, "downloads": 1234567, "updated_at": "2026-05-01" }
    ] }))
    .await;
    assert!(web.get("/addons").await.body.contains("1,234,567 downloads"));
}

#[tokio::test]
async fn new_results_start_at_the_top() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    mock(&web, "/v1/packs", json!({ "total": 0, "packs": [] })).await;
    for page in ["/addons", "/packs"] {
        assert!(web.get(page).await.body.contains(r#"hx-swap="innerHTML scroll:top""#), "{page}");
    }
}

#[tokio::test]
async fn the_view_switch_is_one_segmented_control_with_icons() {
    // One framed control, the active half filled; icons say tiles or list at a glance.
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    let body = web.get("/addons").await.body;
    assert_eq!(body.matches(r#"<svg class="view-icon""#).count(), 2, "{body}");
    let css = web.get("/static/app.css").await.body;
    assert!(css.contains(".view-toggle { display: inline-flex;") && css.contains(r#".view-toggle button[aria-pressed="true"]"#));
}

#[tokio::test]
async fn an_empty_search_offers_the_way_back() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    mock(&web, "/v1/packs", json!({ "total": 0, "packs": [] })).await;
    assert!(web.get("/addons?q=nothing").await.body.contains(r#"<a href="/addons">Show all addons</a>"#));
    assert!(web.get("/packs?q=nothing").await.body.contains(r#"<a href="/packs">Show all packs</a>"#));
}

#[tokio::test]
async fn the_download_says_how_to_install() {
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains("unzip the download into the game's <code>AddOns</code> folder"), "{page}");
    assert!(page.contains(r#"<a href="https://rezondes.github.io/SurvivorsForge/">"#), "where to get the launcher");
}

#[tokio::test]
async fn shared_links_get_a_preview() {
    // Discord and other chats show these instead of a bare address.
    let mut web = TestWeb::new().await;
    let home = web.get("/").await.body;
    for tag in [
        r#"<meta property="og:site_name" content="SurvivorsForge">"#,
        r#"<meta property="og:title" content="SurvivorsForge">"#,
        r#"<meta property="og:image" content="https://web.test/static/icon.png">"#,
        r#"<meta name="twitter:card" content="summary">"#,
    ] {
        assert!(home.contains(tag), "{tag}");
    }
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains(r#"<meta property="og:title" content="Lucky Odds">"#), "{page}");
    assert!(page.contains(r#"<meta property="og:description" content="Odds for rare cards">"#), "{page}");
}

#[tokio::test]
async fn the_launcher_comes_first() {
    // The main button opens the addon in the installed launcher; GitHub is the second way.
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await.body;
    let launcher = page.find(r#"<a class="button large" href="survivorsforge://addon/lucky" data-launcher>"#).expect("launcher button");
    let github = page.find(r#"<a class="button secondary" href="#).expect("GitHub download as the second button");
    assert!(launcher < github, "{page}");
    assert!(page.contains(r#"data-launcher-missing"#), "a way to get the launcher when nothing opens");
}

#[tokio::test]
async fn screenshots_without_caption_still_have_a_text() {
    let mut web = TestWeb::new().await;
    let mut a = addon("scanned");
    a["images"] = json!([{ "id": 1, "url": "https://web.test/img/a.webp", "width": 800, "height": 450, "caption": "" }]);
    mock(&web, "/v1/addons/lucky", a).await;
    mock(&web, "/v1/addons/lucky/versions", versions("scanned", "available")).await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains(r#"alt="Screenshot 1 of Lucky Odds""#), "{page}");
    // A click opens the viewer on the page; without script the link still opens the image.
    assert!(page.contains(r#"<a href="https://web.test/img/a.webp" data-lightbox>"#), "{page}");
    assert!(page.contains(r#"<dialog class="lightbox""#), "{page}");
}

#[tokio::test]
async fn the_version_says_when_and_how_often() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/addons/lucky", addon("scanned")).await;
    let mut list = versions("scanned", "available");
    list[0]["downloads"] = json!(1234);
    mock(&web, "/v1/addons/lucky/versions", list).await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains("Published 2026-05-01 · 1,234 downloads"), "{page}");
}

#[tokio::test]
async fn long_tags_are_shortened_in_the_heading() {
    let mut web = TestWeb::new().await;
    let tag = "build-6284ab9995305ec45046c881ccfe88f776184a1b";
    let mut a = addon("scanned");
    a["latest_tag"] = json!(tag);
    mock(&web, "/v1/addons/lucky", a).await;
    let mut list = versions("scanned", "available");
    list[0]["tag"] = json!(tag);
    mock(&web, "/v1/addons/lucky/versions", list).await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains(&format!(r#"<h2>Version <span title="{tag}">build-6284ab999530…</span></h2>"#)), "{page}");
}

#[tokio::test]
async fn a_missing_game_version_reads_as_the_author_s_choice() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/addons/lucky", addon("scanned")).await;
    let mut list = versions("scanned", "available");
    list[0]["game_min"] = Value::Null;
    mock(&web, "/v1/addons/lucky/versions", list).await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains("Game version: not stated by the author"), "{page}");
}

#[tokio::test]
async fn only_logged_in_visitors_hear_about_claiming() {
    let mut web = TestWeb::new().await;
    let mut a = addon("scanned");
    a["claimed"] = json!(false);
    mock(&web, "/v1/addons/lucky", a).await;
    mock(&web, "/v1/addons/lucky/versions", versions("scanned", "available")).await;
    assert!(!web.get("/addons/lucky").await.body.contains("not claimed on this site yet"), "players do not care");
    web.login_as("tok", common::me("bob", "user")).await;
    assert!(web.get("/addons/lucky").await.body.contains("not claimed on this site yet"));
}

#[tokio::test]
async fn the_addon_page_leads_back_to_the_list() {
    let mut web = with_addon("scanned", "available").await;
    assert!(web.get("/addons/lucky").await.body.contains(r#"<p class="back"><a href="/addons">All addons</a></p>"#));
}

#[tokio::test]
async fn links_that_leave_the_site_are_marked() {
    let mut web = with_addon("scanned", "available").await;
    let page = web.get("/addons/lucky").await.body;
    assert!(page.contains(r#"<a href="https://github.com/alice/lucky" rel="external noopener">GitHub</a>"#), "{page}");
    assert!(page.contains(r#"rel="external noopener">Download from GitHub</a>"#), "{page}");
    assert!(web.get("/static/app.css").await.body.contains(r#"a[rel~="external"]::after"#));
}

#[tokio::test]
async fn category_links_are_encoded() {
    let mut web = TestWeb::new().await;
    let mut a = addon("scanned");
    a["category"] = json!("Action Bars");
    mock(&web, "/v1/addons/lucky", a).await;
    mock(&web, "/v1/addons/lucky/versions", versions("scanned", "available")).await;
    mock(&web, "/v1/categories", categories()).await;
    mock(&web, "/v1/addons", json!({ "total": 1, "addons": [
        { "slug": "lucky", "name": "Lucky", "summary": "", "category": "Action Bars", "trust": "scanned", "icon_url": null, "owner": null,
          "author": "bob", "verified_author": false, "downloads": 1, "updated_at": "2026-05-01" }
    ] }))
    .await;
    for page in ["/addons/lucky", "/addons"] {
        assert!(web.get(page).await.body.contains(r#"href="/addons?category=Action%20Bars""#), "{page}");
    }
}

#[tokio::test]
async fn the_home_page_leads_with_the_launcher() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    let page = web.get("/").await.body;
    let launcher = page.find(r#"class="button large" href="https://github.com/Rezondes/SurvivorsForge/releases/latest""#).expect("launcher download");
    let browse = page.find(r#"class="button secondary" href="/addons""#).expect("browse addons");
    assert!(launcher < browse, "the launcher comes first");
    assert!(page.contains("Windows · Linux &amp; Steam Deck") || page.contains("Windows · Linux &#38; Steam Deck"), "{page}");
}

#[tokio::test]
async fn browser_players_find_the_packs() {
    let mut web = TestWeb::new().await;
    mock(&web, "/v1/addons", json!({ "total": 0, "addons": [] })).await;
    let page = web.get("/").await.body;
    for text in ["Playing in the browser?", r#"href="/packs/new">Build an addon pack</a>"#, r#"href="/packs">Browse packs</a>"#] {
        assert!(page.contains(text), "missing {text}: {page}");
    }
}

#[tokio::test]
async fn the_home_page_shows_popular_addons() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/addons"))
        .and(query_param("sort", "downloads"))
        .and(query_param("per_page", "6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 1, "addons": [
            { "slug": "lucky", "name": "Lucky Odds", "summary": "Odds", "category": "Combat", "trust": "verified", "icon_url": null, "owner": null,
              "author": "alice", "verified_author": false, "downloads": 4200, "updated_at": "2026-05-01" }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/").await.body;
    for text in ["Popular addons", r#"<a href="/addons/lucky">Lucky Odds</a>"#, "4,200 downloads", r#"href="/addons">All addons</a>"#] {
        assert!(page.contains(text), "missing {text}: {page}");
    }

    // Without an answer from the API the home page still works, without the section.
    let mut down = TestWeb::new().await;
    let page = down.get("/").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("Popular addons"));
}

#[tokio::test]
async fn the_home_page_explains_the_checks() {
    let mut web = TestWeb::new().await;
    let page = web.get("/").await.body;
    for text in ["Checked automatically", "Verified by hand", "Straight from GitHub"] {
        assert!(page.contains(&format!("<h3>{text}</h3>")), "missing {text}: {page}");
    }
}

#[tokio::test]
async fn addon_authors_find_how_to_publish() {
    let mut web = TestWeb::new().await;
    let page = web.get("/").await.body;
    assert!(page.contains("Made an addon?") && page.contains(r#"href="/addons/new">Publish it from your GitHub repo</a>"#), "{page}");
}

#[tokio::test]
async fn the_home_page_describes_itself_to_search_engines() {
    let mut web = TestWeb::new().await;
    let page = web.get("/").await.body;
    assert!(page.contains(r#"<meta name="description" content="Addons for Azeroth Survivors, checked for harmful code."#), "{page}");
}

#[tokio::test]
async fn the_home_title_fits_a_phone() {
    // Short enough for one line between the gold rules; the game's name follows below.
    let mut web = TestWeb::new().await;
    let page = web.get("/").await.body;
    assert!(page.contains("<h1>Addons, checked</h1>") && page.contains("<p class=\"lead\">For Azeroth Survivors."), "{page}");
}

#[tokio::test]
async fn downloads_are_counted_before_the_redirect() {
    let mut web = with_addon("verified", "available").await;
    Mock::given(method("POST")).and(path("/v1/addons/lucky/downloads")).respond_with(ResponseTemplate::new(204)).expect(1).mount(&web.api).await;
    let direct = web.get("/addons/lucky/versions/v1/download").await;
    assert_eq!((direct.status, direct.location()), (StatusCode::SEE_OTHER, ASSET));
}
