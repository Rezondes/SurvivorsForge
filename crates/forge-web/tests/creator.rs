mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestWeb, WEB, me};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

async fn get_json(web: &TestWeb, route: &str, body: Value) {
    Mock::given(method("GET")).and(path(route)).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&web.api).await;
}

async fn logged_in(perms: Value) -> TestWeb {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("alice", "user")).await;
    get_json(&web, "/v1/categories", json!([{ "name": "Unit Frames", "parent": null, "selectable": true, "sort_key": 40 }, { "name": "Classes", "parent": null, "selectable": false, "sort_key": 10 }, { "name": "Miscellaneous", "parent": null, "selectable": true, "sort_key": 60 }])).await;
    get_json(&web, "/v1/addons/lucky/permissions", perms).await;
    get_json(&web, "/v1/addons/lucky", json!({
        "slug": "lucky", "name": "Lucky", "category": "Miscellaneous", "source_type": "release", "asset_pattern": "Lucky-v*.zip",
        "folders": ["Lucky"], "summary": "Odds", "description_md": "# Hi", "images": [{ "id": 7, "url": "https://web.test/img/a.webp", "caption": "x" }],
        "icon_url": null, "dependencies": [{ "slug": "lib", "name": "Lib", "required": true }]
    }))
    .await;
    web
}

fn owner() -> Value {
    json!({ "owner": true, "edit_description": true, "edit_images": true, "admin": false })
}

#[tokio::test]
async fn wizard_happy_path_and_field_errors() {
    let mut web = logged_in(owner()).await;
    let page = web.get("/addons/new").await;
    assert!(page.body.contains(">Unit Frames</option>") && !page.body.contains(">Classes</option>"), "only categories without subcategories");
    assert!(!page.body.contains("Install the GitHub App"), "repos of the own account need no App");

    Mock::given(method("POST"))
        .and(path("/v1/addons"))
        .and(body_partial_json(json!({ "repo": "alice/taken" })))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({ "error": "folder_taken", "message": "The folder Lucky belongs to the addon Other" })))
        .mount(&web.api)
        .await;
    let fields = [("repo", "alice/taken"), ("slug", "lucky"), ("name", "Lucky"), ("category", "Miscellaneous"), ("source_type", "release"), ("asset_pattern", "L-*.zip"), ("folders", "Lucky")];
    let page = web.post_multipart("/addons/new", &fields, &[]).await;
    assert_eq!(page.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.body.contains("The folder Lucky belongs to the addon Other"));
    assert!(page.body.contains("value=\"taken\""), "input kept behind the fixed account");

    Mock::given(method("POST"))
        .and(path("/v1/addons"))
        .and(header("authorization", "Bearer tok"))
        .and(body_partial_json(json!({ "repo": "alice/lucky", "folders": ["Lucky", "LuckyData"], "summary": "Odds", "description_md": "## Features" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "slug": "lucky" })))
        .mount(&web.api)
        .await;
    let fields = [
        ("repo", "alice/lucky"), ("slug", "lucky"), ("name", "Lucky"), ("summary", "Odds"), ("description_md", "## Features"),
        ("category", "Miscellaneous"), ("source_type", "release"), ("asset_pattern", "L-*.zip"), ("folders", "Lucky\nLuckyData"),
    ];
    let page = web.post_multipart("/addons/new", &fields, &[]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/lucky/images?created=1"), "straight on to the screenshots");
    let images = web.get("/addons/lucky/images?created=1").await;
    assert!(images.body.contains("Your addon is published"), "{}", images.body);
}

#[tokio::test]
async fn the_new_addon_form_has_both_descriptions() {
    let mut web = logged_in(owner()).await;
    let page = web.get("/addons/new").await;
    assert!(page.body.contains(">Short description<") && !page.body.contains("One line about it"), "{}", page.body);
    assert!(page.body.contains(r#"name="description_md""#) && page.body.contains(r#"hx-post="/addons/new/preview""#));
    assert!(page.body.contains("screenshots"), "says where screenshots go");
    let preview = web.post_form("/addons/new/preview", &[("description_md", "**bold**")]).await;
    assert!(preview.body.contains("<strong>bold</strong>"), "{}", preview.body);
}

#[tokio::test]
async fn contributor_sees_only_allowed_sections() {
    let mut web = logged_in(json!({ "owner": false, "edit_description": true, "edit_images": false })).await;
    let page = web.get("/addons/lucky/edit").await;
    assert!(page.body.contains("Description (Markdown)"));
    assert!(!page.body.contains("Save details"), "no owner section");
    assert!(!page.body.contains("/addons/lucky/members"), "no owner links");
    for owner_page in ["/addons/lucky/members", "/addons/lucky/dependencies", "/addons/lucky/delete", "/addons/lucky/images"] {
        assert_eq!(web.get(owner_page).await.status, StatusCode::FORBIDDEN, "{owner_page}");
    }

    let mut images_only = logged_in(json!({ "owner": false, "edit_description": false, "edit_images": true })).await;
    let page = images_only.get("/addons/lucky/edit").await;
    assert_eq!(page.location(), "/addons/lucky/images");
}

#[tokio::test]
async fn owner_manages_members() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/addons/lucky/members", json!([{ "github_id": 2, "login": "bob", "can_edit_description": true, "can_edit_images": false }])).await;
    let page = web.get("/addons/lucky/members").await;
    assert!(page.body.contains("bob"));
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/members"))
        .and(body_partial_json(json!({ "login": "carol", "can_edit_description": false, "can_edit_images": true })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!([])))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/members", &[("login", "carol"), ("images", "1")]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER);
    Mock::given(method("PATCH"))
        .and(path("/v1/addons/lucky/members/2"))
        .and(body_partial_json(json!({ "can_edit_description": false, "can_edit_images": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&web.api)
        .await;
    web.post_form("/addons/lucky/members/2", &[("images", "1")]).await;
}

#[tokio::test]
async fn image_upload_messages() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/images"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 8, "notice": "resized", "original": { "width": 3000, "height": 1000 }, "stored": { "width": 1920, "height": 640 }
        })))
        .mount(&web.api)
        .await;
    let token = web.csrf().await;
    let boundary = "B0UND";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\nscreenshot\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n--{boundary}--\r\n"
    );
    let cookie = web.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    let request = Request::post("/addons/lucky/images")
        .header("cookie", cookie)
        .header("origin", WEB)
        .header("x-csrf-token", token)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    let page = web.request(request).await;
    assert!(page.body.contains("Image was scaled down from 3000 x 1000 to 1920 x 640 px."), "{}", page.body);
}

#[tokio::test]
async fn the_edit_toolbar_stays_on_every_edit_page() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/addons/lucky/members", json!([])).await;
    get_json(&web, "/v1/addons/lucky/versions", json!([{ "tag": "v1", "game_min": null, "game_max": null }])).await;
    get_json(&web, "/v1/game-versions", json!([{ "name": "0.69.0" }])).await;
    let toolbar = r#"<nav class="subnav" aria-label="Edit addon">"#;
    for page in ["/addons/lucky/edit", "/addons/lucky/images", "/addons/lucky/members", "/addons/lucky/dependencies", "/addons/lucky/versions", "/addons/lucky/versions/v1/edit", "/addons/lucky/delete"] {
        let body = web.get(page).await.body;
        assert!(body.contains(toolbar), "{page}: {body}");
    }
    let mut anonymous = TestWeb::new().await;
    get_json(&anonymous, "/v1/addons/lucky", json!({ "slug": "lucky", "name": "Lucky", "blocked": false, "verified_author": false })).await;
    get_json(&anonymous, "/v1/addons/lucky/versions", json!([])).await;
    assert!(!anonymous.get("/addons/lucky/versions").await.body.contains(toolbar), "only for those who may edit");
}

#[tokio::test]
async fn captions_can_be_changed_later() {
    let mut web = logged_in(owner()).await;
    let images = web.get("/addons/lucky/images").await.body;
    assert!(images.contains(r#"action="/addons/lucky/images/7/caption""#) && images.contains(r#"value="x""#), "{images}");
    Mock::given(method("PATCH"))
        .and(path("/v1/addons/lucky/images/7"))
        .and(body_partial_json(json!({ "caption": "Bags open" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&web.api)
        .await;
    // Back at the screenshot, not at the top; view.js saves in place and says so at the field.
    let page = web.post_form("/addons/lucky/images/7/caption", &[("caption", "Bags open")]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/lucky/images#shot-7"));
    assert!(images.contains(r#"data-caption-status"#), "{images}");
}

#[tokio::test]
async fn screenshots_take_a_dragged_order() {
    let mut web = logged_in(owner()).await;
    let images = web.get("/addons/lucky/images").await.body;
    assert!(images.contains(r#"data-order-url="/addons/lucky/images/order""#) && images.contains(r#"data-image-id="7""#), "{images}");
    assert!(images.contains(r#"aria-label="Move earlier""#) && images.contains(r#"aria-label="Delete screenshot""#), "icon buttons keep a name");
    Mock::given(method("PATCH"))
        .and(path("/v1/addons/lucky/images/7"))
        .and(body_partial_json(json!({ "position": 0 })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&web.api)
        .await;
    // An id of another addon is left out.
    let page = web.post_form("/addons/lucky/images/order", &[("order", "99,7")]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/lucky/images"));
}

#[tokio::test]
async fn moving_a_screenshot_keeps_it_in_view() {
    // Without script the page comes back scrolled to the moved screenshot, not to the top.
    let mut web = logged_in(owner()).await;
    let images = web.get("/addons/lucky/images").await.body;
    assert!(images.contains(r#"<figure data-image-id="7" id="shot-7">"#) && images.contains("data-move"), "{images}");
    let page = web.post_form("/addons/lucky/images/7/move", &[("direction", "up")]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/lucky/images#shot-7"));
}

#[tokio::test]
async fn the_icon_is_cropped_replaced_and_deleted() {
    let mut web = logged_in(owner()).await;
    let page = web.get("/addons/lucky/images").await.body;
    assert!(page.contains("data-cropper") && page.contains(r#"name="crop_size""#), "a square to pick for the icon");
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/images"))
        .and(body_string_contains("crop_size"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
        .expect(1)
        .mount(&web.api)
        .await;
    let token = web.csrf().await;
    let boundary = "B0UND";
    let field = |name: &str, value: &str| format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n");
    let body = format!(
        "{}{}{}{}--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"i.png\"\r\nContent-Type: image/png\r\n\r\nPNG\r\n--{boundary}--\r\n",
        field("kind", "icon"),
        field("crop_x", "10"),
        field("crop_y", "0"),
        field("crop_size", "50")
    );
    let cookie = web.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    let request = Request::post("/addons/lucky/images")
        .header("cookie", cookie)
        .header("origin", WEB)
        .header("x-csrf-token", token)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    assert!(web.request(request).await.body.contains("Image uploaded."));

    Mock::given(method("DELETE")).and(path("/v1/addons/lucky/icon")).respond_with(ResponseTemplate::new(204)).expect(1).mount(&web.api).await;
    let page = web.post_form("/addons/lucky/icon/delete", &[]).await;
    assert!(page.body.contains("Icon deleted."), "{}", page.body);
}

#[tokio::test]
async fn deleting_an_image_asks_first() {
    let mut web = logged_in(owner()).await;
    let page = web.get("/addons/lucky/images").await.body;
    assert!(page.contains(r#"class="inline" data-confirm="Delete this screenshot? This cannot be undone.""#), "{page}");
}

#[tokio::test]
async fn the_icon_square_moves_by_keyboard() {
    let mut web = logged_in(owner()).await;
    let page = web.get("/addons/lucky/images").await.body;
    assert!(page.contains(r#"<div class="cropper-square" tabindex="0" role="group" aria-label="Icon square: move it with the arrow keys""#), "{page}");
}

#[tokio::test]
async fn several_screenshots_in_one_upload() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/images"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 8 })))
        .expect(2)
        .mount(&web.api)
        .await;
    let token = web.csrf().await;
    let boundary = "B0UND";
    let file = |name: &str| format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n");
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\nscreenshot\r\n{}{}--{boundary}--\r\n",
        file("a.png"),
        file("b.png")
    );
    let cookie = web.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    let request = Request::post("/addons/lucky/images")
        .header("cookie", cookie)
        .header("origin", WEB)
        .header("x-csrf-token", token)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    let page = web.request(request).await;
    assert!(page.body.contains("2 images uploaded."), "{}", page.body);
    let form = web.get("/addons/lucky/images").await.body;
    assert!(form.contains(r#"name="file" accept="image/png,image/jpeg,image/webp" multiple"#));
    // Picking "Icon" turns the field into a single file one (view.js), the server keeps one anyway.
    assert!(form.contains(r#"<select id="kind" name="kind" data-single-file="icon">"#), "{form}");
}

#[tokio::test]
async fn dependency_cycle_message() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("PUT"))
        .and(path("/v1/addons/lucky/dependencies"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "dependency_cycle", "message": "This would make a cycle: lucky -> lib -> lucky" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/dependencies", &[("dependency", "lib2"), ("required", "1")]).await;
    assert!(page.body.contains("lucky -&gt; lib -&gt; lucky") || page.body.contains("lucky -&#62; lib -&#62; lucky"), "{}", page.body);
}

#[tokio::test]
async fn version_compat_form() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/addons/lucky/versions", json!([{ "tag": "v1", "game_min": "0.69.0", "game_max": null }])).await;
    get_json(&web, "/v1/game-versions", json!([{ "name": "0.69.0" }, { "name": "0.70.0" }])).await;
    let page = web.get("/addons/lucky/versions/v1/edit").await;
    assert!(page.body.contains("<option value=\"0.69.0\" selected>"), "{}", page.body);
    Mock::given(method("PATCH"))
        .and(path("/v1/addons/lucky/versions/v1"))
        .and(body_partial_json(json!({ "game_min": "0.69.0", "game_max": "0.70.0" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&web.api)
        .await;
    let page = web.post_form("/addons/lucky/versions/v1/edit", &[("game_min", "0.69.0"), ("game_max", "0.70.0")]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn dashboard_lists_own_and_shared() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/me/addons", json!([
        { "slug": "lucky", "name": "Lucky", "role": "owner", "trust": "flagged", "latest_tag": "v1", "findings": { "critical": 1, "warn": 2 }, "blocked": false, "locked": false },
        { "slug": "plates", "name": "Plates", "role": "contributor", "trust": null, "latest_tag": null, "findings": { "critical": 0, "warn": 0 }, "blocked": false, "locked": true }
    ]))
    .await;
    let page = web.get("/me/addons").await;
    for text in ["Lucky", "owner", "Flagged", "3 findings", "Plates", "contributor", "No release yet", "Repo not reachable"] {
        assert!(page.body.contains(text), "missing {text}");
    }
}

#[tokio::test]
async fn unclaimed_addons_of_my_repos_can_be_claimed() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/me/addons", json!([
        { "slug": "orphan", "name": "Orphan", "role": "claimable", "trust": "scanned", "latest_tag": "v1", "findings": { "critical": 0, "warn": 0 }, "blocked": false, "locked": false }
    ]))
    .await;
    let page = web.get("/me/addons").await;
    assert!(page.body.contains(r#"action="/addons/orphan/claim""#) && page.body.contains("Claim it"), "{}", page.body);
    assert!(!page.body.contains("/addons/orphan/edit"), "not editable before the claim");

    Mock::given(method("POST")).and(path("/v1/addons/orphan/claim")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "slug": "orphan" }))).expect(1).mount(&web.api).await;
    let done = web.post_form("/addons/orphan/claim", &[("back", "/me/addons")]).await;
    assert!(done.location().starts_with("/me/addons?msg="), "{}", done.location());

    Mock::given(method("POST"))
        .and(path("/v1/addons/other/claim"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "app_not_installed", "message": "Install the SurvivorsForge GitHub App on this repo first" })))
        .mount(&web.api)
        .await;
    let failed = web.post_form("/addons/other/claim", &[("back", "/addons/other")]).await;
    assert!(failed.location().starts_with("/addons/other?error="), "{}", failed.location());
    let evil = web.post_form("/addons/other/claim", &[("back", "https://evil.test/")]).await;
    assert!(evil.location().starts_with("/me/addons?"), "only our own pages: {}", evil.location());
}

#[tokio::test]
async fn delete_requires_slug() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("DELETE")).and(path("/v1/addons/lucky")).respond_with(ResponseTemplate::new(204)).expect(1).mount(&web.api).await;
    let wrong = web.post_form("/addons/lucky/delete", &[("confirm", "Lucky")]).await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    let right = web.post_form("/addons/lucky/delete", &[("confirm", "lucky")]).await;
    assert_eq!((right.status, right.location()), (StatusCode::SEE_OTHER, "/me/addons"));
}

#[tokio::test]
async fn creator_pages_need_login() {
    let mut web = TestWeb::new().await;
    let page = web.get("/me/addons").await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/login"));
}

#[tokio::test]
async fn my_repos_fill_in_the_form() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("GET"))
        .and(path("/v1/me/repos"))
        .and(wiremock::matchers::query_param("owner", "alice"))
        .and(wiremock::matchers::query_param("q", "luck"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "owner": "alice", "repos": [{ "full_name": "alice/LuckyOdds", "description": "Odds" }] })))
        .mount(&web.api)
        .await;
    get_json(&web, "/v1/me/repos/alice/LuckyOdds/suggestion", json!({
        "repo": "alice/LuckyOdds", "name": "Lucky Odds", "slug": "luckyodds", "summary": "Odds for rare cards", "source_type": "release",
        "asset_pattern": "LuckyOdds-v*.zip", "folders": ["LuckyOdds", "LuckyLib"], "read_from": "release v0.7.0, LuckyOdds-v0.7.0.zip"
    }))
    .await;

    // The owner is the visitor's account and cannot be changed; only the repo name is typed.
    let page = web.get("/addons/new").await;
    assert!(page.body.contains(r#"<span class="repo-owner">alice</span>"#) && page.body.contains(r#"name="repo_name""#), "{}", page.body);
    assert!(!page.body.contains(r#"<input name="repo_owner""#), "no owner field for creators");

    // Typing asks for matching public repos of the chosen account.
    let found = web.get("/addons/new/repos?repo_owner=alice&repo_name=luck").await;
    assert!(found.body.contains("alice/LuckyOdds") && found.body.contains("Odds"), "{}", found.body);
    let picked = web.get("/addons/new/repo?repo=alice/LuckyOdds").await;
    assert!(picked.body.contains(r#"value="LuckyOdds""#) && picked.body.contains(r#"href="/addons/new?repo=alice/LuckyOdds""#), "{}", picked.body);
    assert!(picked.body.contains("Fill in from this repo"));

    let filled = web.get("/addons/new?repo=alice/LuckyOdds").await;
    for text in [r#"value="LuckyOdds""#, r#"value="Lucky Odds""#, r#"value="luckyodds""#, r#"value="Odds for rare cards""#,
                 r#"value="LuckyOdds-v*.zip""#, "LuckyOdds\nLuckyLib</textarea>", "release v0.7.0, LuckyOdds-v0.7.0.zip"] {
        assert!(filled.body.contains(text), "missing {text}");
    }

    // The repo the API gets is the chosen account plus the typed name; the API checks the account.
    Mock::given(method("POST"))
        .and(path("/v1/addons"))
        .and(body_partial_json(json!({ "repo": "alice/LuckyOdds" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "slug": "luckyodds" })))
        .expect(1)
        .mount(&web.api)
        .await;
    let fields = [("repo_owner", "alice"), ("repo_name", "LuckyOdds"), ("slug", "luckyodds"), ("name", "Lucky"), ("category", "Miscellaneous"), ("source_type", "release"), ("folders", "LuckyOdds")];
    web.post_multipart("/addons/new", &fields, &[]).await;

    let broken = web.get("/addons/new?repo=alice/Other").await;
    assert_eq!(broken.status, StatusCode::OK);
    assert!(broken.body.contains("could not be read"), "the form stays usable: {}", broken.body);
}

#[tokio::test]
async fn creators_pick_their_account_or_organization() {
    let mut web = logged_in(owner()).await;
    get_json(&web, "/v1/me/accounts", json!(["alice", "guild"])).await;
    let page = web.get("/addons/new").await;    for text in [r#"<select name="repo_owner""#, r#"<option value="alice" selected>alice</option>"#, r#"<option value="guild">guild</option>"#, "need the App and your admin rights"] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
    assert!(!page.body.contains("Install the SurvivorsForge App on your repo"), "no install step any more");
    assert!(page.body.contains("<h2>1. Your repo</h2>"));
}

#[tokio::test]
async fn icon_and_screenshots_come_with_the_new_addon() {
    let mut web = logged_in(owner()).await;
    let form = web.get("/addons/new").await;
    for text in [r#"enctype="multipart/form-data""#, r#"name="icon""#, r#"name="screenshots""#] {
        assert!(form.body.contains(text), "missing {text}");
    }
    Mock::given(method("POST")).and(path("/v1/addons")).respond_with(ResponseTemplate::new(201).set_body_json(json!({ "slug": "lucky" }))).mount(&web.api).await;
    for (kind, times) in [("icon", 1), ("screenshot", 2)] {
        Mock::given(method("POST"))
            .and(path("/v1/addons/lucky/images"))
            .and(wiremock::matchers::body_string_contains(format!("name=\"kind\"\r\n\r\n{kind}\r\n")))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(times)
            .mount(&web.api)
            .await;
    }
    let fields = [("repo", "alice/lucky"), ("slug", "lucky"), ("name", "Lucky"), ("category", "Miscellaneous"), ("source_type", "release"), ("folders", "Lucky")];
    // An empty file input still sends a part without a file name; it uploads nothing.
    let files: [(&str, &str, &[u8]); 4] = [("icon", "icon.png", b"I"), ("screenshots", "a.png", b"A"), ("screenshots", "b.png", b"B"), ("screenshots", "", b"")];

    let forged = web.post_multipart_with("/addons/new", "wrong", &fields, &files).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN, "the CSRF token counts in file forms too");

    let page = web.post_multipart("/addons/new", &fields, &files).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/addons/lucky/images?created=1"), "{}", page.body);
}

#[tokio::test]
async fn a_failed_image_upload_still_lands_on_the_new_addon() {
    let mut web = logged_in(owner()).await;
    Mock::given(method("POST")).and(path("/v1/addons")).respond_with(ResponseTemplate::new(201).set_body_json(json!({ "slug": "lucky" }))).mount(&web.api).await;
    Mock::given(method("POST"))
        .and(path("/v1/addons/lucky/images"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "invalid_image", "message": "This is not a PNG, JPEG or WebP image" })))
        .mount(&web.api)
        .await;
    let fields = [("repo", "alice/lucky"), ("slug", "lucky"), ("name", "Lucky"), ("category", "Miscellaneous"), ("source_type", "release"), ("folders", "Lucky")];
    let page = web.post_multipart("/addons/new", &fields, &[("screenshots", "a.txt", b"nope")]).await;
    assert!(page.location().starts_with("/addons/lucky/images?created=1&error="), "{}", page.location());
    let images = web.get(page.location()).await;
    assert!(images.body.contains("a.txt: This is not a PNG, JPEG or WebP image"), "{}", images.body);
}
