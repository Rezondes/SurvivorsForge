mod common;

use axum::http::StatusCode;
use common::{TestWeb, me};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

fn registry() -> serde_json::Value {
    let addon = |id: &str, name: &str, trust: &str, sha: Option<&str>| {
        json!({ "id": id, "name": name, "author": "a", "repo": "o/r", "folders": [name], "source": { "type": "tag" }, "verified_author": false,
                "latest": { "tag": "v1", "asset_url": "https://github.com/x.zip", "sha256": sha, "size": 2_500_000, "trust": trust, "findings": { "critical": 0, "warn": 0 } } })
    };
    json!({ "schema": 1, "addons": [
        addon("lucky", "Lucky", "scanned", Some("abc")),
        addon("bad", "Bad", "flagged", Some("abc")),
        addon("odd", "Odd", "verified", None),
        addon("aether", "Aether", "verified", Some("abc")),
    ] })
}

async fn logged_in() -> TestWeb {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("alice", "user")).await;
    Mock::given(method("GET")).and(path("/v1/registry")).respond_with(ResponseTemplate::new(200).set_body_json(registry())).mount(&web.api).await;
    web
}

#[tokio::test]
async fn picker_lists_only_packable_addons_and_ticks_the_given_ones() {
    let mut web = logged_in().await;
    let page = web.get("/packs/new?addon=lucky").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains(r#"value="lucky" checked"#), "{}", page.body);
    assert!(page.body.contains(r#"value="aether" >"#) || page.body.contains(r#"value="aether">"#));
    assert!(!page.body.contains(r#"value="bad""#), "flagged is not offered");
    assert!(!page.body.contains(r#"value="odd""#), "no hash, not offered");
    assert!(page.body.contains("2.5 MB"));
    assert!(page.body.find("Aether").unwrap() < page.body.find("Lucky").unwrap(), "sorted by name");
}

#[tokio::test]
async fn creating_redirects_to_the_pack_and_errors_keep_the_choice() {
    let mut web = logged_in().await;
    Mock::given(method("POST"))
        .and(path("/v1/packs"))
        .and(body_partial_json(json!({ "name": "Mine", "addons": [{ "slug": "lucky" }, { "slug": "aether" }] })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "AbCdE12345" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/packs/new", &[("name", "Mine"), ("addon", "lucky"), ("addon", "aether")]).await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/packs/AbCdE12345"));

    Mock::given(method("POST"))
        .and(path("/v1/packs"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "error": "pack_limit", "message": "At most 10 new packs an hour, try again later" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/packs/new", &[("name", "Other"), ("addon", "lucky")]).await;
    assert_eq!(page.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(page.body.contains("At most 10 new packs an hour"));
    assert!(page.body.contains(r#"value="lucky" checked"#) && page.body.contains(r#"value="Other""#));
}

fn pack(available: bool) -> serde_json::Value {
    json!({ "id": "AbCdE12345", "name": "My UI", "created_at": "2026-10-29T10:00:00Z", "created_by": "alice", "available": available,
            "addons": [{ "slug": "lucky", "name": "Lucky", "tag": "v1", "trust": "scanned", "available": available, "folders": ["Lucky", "LuckyLib"] }],
            "file": { "status": "ready", "size": 3_100_000, "error": null } })
}

#[tokio::test]
async fn the_pack_page_is_public_and_shows_the_content_and_the_link() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET")).and(path("/v1/packs/AbCdE12345")).respond_with(ResponseTemplate::new(200).set_body_json(pack(true))).mount(&web.api).await;
    let page = web.get("/packs/AbCdE12345").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["My UI", "by alice", "Lucky", "v1", "Lucky, LuckyLib", "https://web.test/packs/AbCdE12345", "Download the pack (3.1 MB)", r#"data-copy="link""#] {
        assert!(page.body.contains(text), "{text}");
    }
    assert!(!page.body.contains("Packed on the first download"), "already built");
    assert!(page.body.contains("29 October 2026"), "date in words");
    assert!(page.body.contains("<td>Lucky, LuckyLib</td>"), "the folders read as content, not muted");

    let mut new = pack(true);
    new["file"] = json!({ "status": "none", "size": null, "error": null });
    Mock::given(method("GET")).and(path("/v1/packs/NotBuilt12")).respond_with(ResponseTemplate::new(200).set_body_json(new)).mount(&web.api).await;
    let page = web.get("/packs/NotBuilt12").await;
    assert!(page.body.contains("Packed on the first download"), "says the first download waits");

    let mut unnamed = pack(true);
    unnamed["name"] = json!("");
    unnamed["addons"].as_array_mut().unwrap().push(json!({ "slug": "odds", "name": "Odds", "tag": "v2", "trust": "verified", "available": true, "folders": ["Odds"] }));
    Mock::given(method("GET")).and(path("/v1/packs/Unnamed123")).respond_with(ResponseTemplate::new(200).set_body_json(unnamed)).mount(&web.api).await;
    let page = web.get("/packs/Unnamed123").await;
    assert!(page.body.contains("<title>Lucky and 1 more - SurvivorsForge</title>") && page.body.contains("<h1>Lucky and 1 more</h1>"), "{}", page.body);

    assert_eq!(web.get("/packs/not-an-id").await.status, StatusCode::NOT_FOUND);
    assert_eq!(web.get("/packs/AbCdE12345%2F..").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unavailable_pack_offers_no_download() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET")).and(path("/v1/packs/AbCdE12345")).respond_with(ResponseTemplate::new(200).set_body_json(pack(false))).mount(&web.api).await;
    let page = web.get("/packs/AbCdE12345").await;
    assert!(page.body.contains("no longer available") && !page.body.contains("Download the pack"));
    assert!(!page.body.contains(r#"role="alert""#), "a state of the page, not an event");

    let mut mixed = pack(false);
    mixed["addons"].as_array_mut().unwrap().push(json!({ "slug": "odds", "name": "Odds", "tag": "v2", "trust": "verified", "available": true, "folders": ["Odds"] }));
    Mock::given(method("GET")).and(path("/v1/packs/Mixed12345")).respond_with(ResponseTemplate::new(200).set_body_json(mixed)).mount(&web.api).await;
    let page = web.get("/packs/Mixed12345").await;
    assert!(page.body.contains(r#"<a href="/packs/new?addon=odds">Build a new one</a>"#), "{}", page.body);
    assert!(!page.body.contains("addon=lucky"), "the new pack leaves out what is gone");
}

#[tokio::test]
async fn download_follows_the_api_or_waits_for_the_build() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/packs/AbCdE12345/download"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "https://web.test/packs-files/abc.zip"))
        .mount(&web.api)
        .await;
    let ready = web.get("/packs/AbCdE12345/download").await;
    assert_eq!((ready.status, ready.location()), (StatusCode::SEE_OTHER, "https://web.test/packs-files/abc.zip"));

    Mock::given(method("GET"))
        .and(path("/v1/packs/Building12/download"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "status": "building" })))
        .mount(&web.api)
        .await;
    let building = web.get("/packs/Building12/download").await;
    assert_eq!(building.status, StatusCode::OK);
    assert!(building.body.contains(r#"content="3;url=/packs/Building12/download?try=1""#) && building.body.contains("Building the pack"));
    let later = web.get("/packs/Building12/download?try=7").await;
    assert!(later.body.contains("download?try=8"), "counts the tries");
    let long = web.get("/packs/Building12/download?try=40").await;
    assert!(!long.body.contains(r#"http-equiv="refresh""#) && long.body.contains("longer than usual"), "stops asking after two minutes");
    assert!(long.body.contains(r#"href="/packs/Building12/download""#), "can check again");

    Mock::given(method("GET"))
        .and(path("/v1/packs/Failed1234/download"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "build_failed", "message": "GitHub did not answer" })))
        .mount(&web.api)
        .await;
    let failed = web.get("/packs/Failed1234/download").await;
    assert_eq!(failed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!failed.body.contains(r#"http-equiv="refresh""#) && failed.body.contains("could not be built") && failed.body.contains("GitHub did not answer"));
    assert!(failed.body.contains(r#"href="/packs/Failed1234""#), "leads back to the pack");
}

#[tokio::test]
async fn the_public_pack_list() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/packs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 1, "page": 1, "per_page": 20, "packs": [
            { "id": "AbCdE12345", "name": "My UI", "created_at": "2026-10-29T10:00:00Z", "created_by": "alice", "addons": ["Lucky", "Odds"] }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/packs").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for text in ["href=\"/packs/AbCdE12345\"", "My UI", "by alice", "Lucky, Odds", "href=\"/packs/new\""] {
        assert!(page.body.contains(text), "missing {text}");
    }
    assert!(page.body.contains("<a href=\"/packs\">Packs</a>"), "in the top navigation");
}

#[tokio::test]
async fn packs_can_be_searched() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/packs"))
        .and(wiremock::matchers::query_param("q", "lucky"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 41, "page": 1, "per_page": 20, "packs": [
            { "id": "AbCdE12345", "name": "My UI", "created_at": "2026-05-01T10:00:00Z", "created_by": "alice", "addons": ["Lucky"] }
        ] })))
        .mount(&web.api)
        .await;
    let page = web.get("/packs?q=lucky").await;
    for text in [r#"<form class="search" method="get" action="/packs""#, r#"name="q" value="lucky""#, "My UI", r#"href="/packs?q=lucky&#38;page=2""#] {
        assert!(page.body.contains(text), "missing {text}: {}", page.body);
    }
}

#[tokio::test]
async fn packs_can_be_listed_when_created() {
    let mut web = logged_in().await;
    assert!(web.get("/packs/new").await.body.contains(r#"name="listed""#));
    Mock::given(method("POST"))
        .and(path("/v1/packs"))
        .and(body_partial_json(json!({ "listed": true })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "AbCdE12345" })))
        .mount(&web.api)
        .await;
    let page = web.post_form("/packs/new", &[("name", "Mine"), ("addon", "lucky"), ("listed", "1")]).await;
    assert_eq!(page.location(), "/packs/AbCdE12345");
}

fn editable_pack() -> serde_json::Value {
    json!({ "id": "AbCdE12345", "name": "My UI", "listed": false, "created_at": "2026-10-29T10:00:00Z", "created_by": "alice", "available": true,
            "addons": [
                { "slug": "lucky", "name": "Lucky", "tag": "v2", "trust": "scanned", "available": true, "latest": true, "versions": ["v2", "v1"], "folders": ["Lucky"] },
                { "slug": "odds", "name": "Odds", "tag": "v1", "trust": "scanned", "available": true, "latest": false, "versions": ["v3", "v1"], "folders": ["Odds"] }
            ],
            "file": { "status": "none", "size": null, "error": null } })
}

#[tokio::test]
async fn new_packs_follow_the_newest_versions_by_default() {
    let mut web = logged_in().await;
    let page = web.get("/packs/new").await;
    assert!(page.body.contains(r#"name="latest" value="1" checked"#), "{}", page.body);
    Mock::given(method("POST"))
        .and(path("/v1/packs"))
        .and(body_partial_json(json!({ "addons": [{ "slug": "lucky", "latest": true }] })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "AbCdE12345" })))
        .expect(1)
        .mount(&web.api)
        .await;
    web.post_form("/packs/new", &[("addon", "lucky"), ("latest", "1")]).await;
}

#[tokio::test]
async fn the_creator_edits_the_pack() {
    let mut web = logged_in().await;
    Mock::given(method("GET")).and(path("/v1/packs/AbCdE12345")).respond_with(ResponseTemplate::new(200).set_body_json(editable_pack())).mount(&web.api).await;
    let page = web.get("/packs/AbCdE12345").await;
    assert!(page.body.contains(r#"href="/packs/AbCdE12345/edit""#) && page.body.contains("v2 <span class=\"muted\">(newest)</span>"), "{}", page.body);

    let edit = web.get("/packs/AbCdE12345/edit").await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    assert!(edit.body.contains(r#"<option value="latest" selected>Newest (now v2)</option>"#), "{}", edit.body);
    assert!(edit.body.contains(r#"<option value="v1" selected>v1</option>"#), "odds stays on v1: {}", edit.body);
    assert!(edit.body.contains(r#"value="aether""#) && !edit.body.contains(r#"name="addon" value="lucky""#), "only addons not in the pack can be added");

    Mock::given(method("PATCH"))
        .and(path("/v1/packs/AbCdE12345"))
        .and(body_partial_json(json!({ "name": "Renamed", "listed": true, "addons": [{ "slug": "odds", "tag": "v3" }, { "slug": "aether", "latest": true }] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(editable_pack()))
        .expect(1)
        .mount(&web.api)
        .await;
    let saved = web
        .post_form("/packs/AbCdE12345/edit", &[("name", "Renamed"), ("listed", "1"), ("keep", "odds"), ("version.odds", "v3"), ("version.lucky", "latest"), ("addon", "aether")])
        .await;
    assert_eq!((saved.status, saved.location()), (StatusCode::SEE_OTHER, "/packs/AbCdE12345"));
}

#[tokio::test]
async fn only_the_creator_edits_the_pack() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("bob", "user")).await;
    Mock::given(method("GET")).and(path("/v1/packs/AbCdE12345")).respond_with(ResponseTemplate::new(200).set_body_json(editable_pack())).mount(&web.api).await;
    assert!(!web.get("/packs/AbCdE12345").await.body.contains("/edit\""), "no link for others");
    assert_eq!(web.get("/packs/AbCdE12345/edit").await.status, StatusCode::FORBIDDEN);
}
