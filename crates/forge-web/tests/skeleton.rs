mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestWeb, WEB, me};
use forge_web::app::{AppState, router};
use forge_web::config::Config;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn reports_api_state() {
    let down = router(AppState::new(Config::for_tests("http://127.0.0.1:1")));
    let response = down.oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let mut web = TestWeb::new().await;
    Mock::given(method("GET")).and(path("/healthz")).respond_with(ResponseTemplate::new(200)).mount(&web.api).await;
    assert_eq!(web.get("/healthz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn login_redirects_to_the_api() {
    // A page that moves on at once instead of a redirect: link previews (Discord) do not follow it,
    // so a shared link to a page behind the login shows SurvivorsForge, not GitHub's login.
    let mut web = TestWeb::new().await;
    let page = web.get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    let start = "https://api.test/v1/auth/github/start?return_to=https%3A%2F%2Fweb.test%2Fauth%2Fcallback".replace('&', "&#38;");
    assert!(page.body.contains(&format!(r#"<meta http-equiv="refresh" content="0;url={start}">"#)), "{}", page.body);
    assert!(page.body.contains(&format!(r#"href="{start}""#)), "a link when the refresh is off");
    assert!(page.body.contains(r#"<meta property="og:site_name" content="SurvivorsForge">"#));
}

#[tokio::test]
async fn callback_sets_cookie_or_shows_error() {
    let mut web = TestWeb::new().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/exchange"))
        .and(body_partial_json(json!({ "code": "bad" })))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_code", "message": "The login code is invalid or expired" })))
        .mount(&web.api)
        .await;
    web.get("/login").await;
    let page = web.get("/auth/callback?code=bad").await;
    assert_eq!(page.status, StatusCode::BAD_REQUEST);
    assert!(page.body.contains("The login code is invalid or expired"));
    assert!(!web.cookies.contains_key("forge_session"));

    let page = web.get("/auth/callback?error=banned").await;
    assert!(page.body.contains("banned"));

    web.login_as("tok-1", me("alice", "user")).await;
    let cookie = web.cookies.get("forge_session").expect("session cookie");
    assert!(!cookie.contains("tok-1"), "the token is encrypted in the cookie");
    let page = web.get("/").await;
    assert!(page.body.contains("Log out alice"), "{}", page.body);
}

#[tokio::test]
async fn a_callback_counts_only_after_a_login_started_here() {
    // Someone sends a visitor their own login code: without a login begun in this browser it is refused.
    let mut web = TestWeb::new().await;
    Mock::given(method("POST")).and(path("/v1/auth/exchange")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": "t" }))).expect(0).mount(&web.api).await;
    let page = web.get("/auth/callback?code=attacker").await;
    assert_eq!(page.status, StatusCode::BAD_REQUEST);
    assert!(!web.cookies.contains_key("forge_session"));
}

#[tokio::test]
async fn session_cookie_flags() {
    let mut web = TestWeb::new().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": "t" })))
        .mount(&web.api)
        .await;
    web.get("/login").await;
    let page = web.get("/auth/callback?code=x").await;
    let session = page.set_cookies().into_iter().find(|c| c.starts_with("forge_session=")).unwrap();
    for flag in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(session.contains(flag), "{flag} missing in {session}");
    }
}

#[tokio::test]
async fn unsafe_methods_need_token_and_origin() {
    let mut web = TestWeb::new().await;
    let token = web.csrf().await;
    let cookie = web.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    let post = |origin: Option<&str>, csrf: Option<&str>| {
        let mut r = Request::post("/logout").header("cookie", cookie.clone());
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        if let Some(t) = csrf {
            r = r.header("x-csrf-token", t);
        }
        r.body(Body::empty()).unwrap()
    };
    assert_eq!(web.request(post(Some(WEB), None)).await.status, StatusCode::FORBIDDEN, "no token");
    assert_eq!(web.request(post(Some(WEB), Some("wrong"))).await.status, StatusCode::FORBIDDEN, "wrong token");
    assert_eq!(web.request(post(Some("https://evil.test"), Some(&token))).await.status, StatusCode::FORBIDDEN, "foreign origin");
    assert_eq!(web.request(post(None, Some(&token))).await.status, StatusCode::FORBIDDEN, "no origin, no referer");
    assert_eq!(web.request(post(Some(WEB), Some(&token))).await.status, StatusCode::SEE_OTHER, "header token");
    assert_eq!(web.post_form("/logout", &[]).await.status, StatusCode::SEE_OTHER, "form field token");
}

#[tokio::test]
async fn security_headers_on_html() {
    let mut web = TestWeb::new().await;
    let page = web.get("/").await;
    assert_eq!(page.headers["content-security-policy"], forge_web::app::CSP);
    assert!(forge_web::app::CSP.contains("script-src 'self'") && !forge_web::app::CSP.contains("unsafe"));
    assert_eq!(page.headers["x-frame-options"], "DENY");
    assert_eq!(page.headers["referrer-policy"], "strict-origin-when-cross-origin");
    assert_eq!(page.headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn escapes_and_single_safe_use() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("<script>alert(1)</script>", "user")).await;
    let page = web.get("/").await;
    assert!(!page.body.contains("<script>alert(1)"), "escaped");
    assert!(page.body.contains("&#60;script&#62;") || page.body.contains("&lt;script&gt;"));

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut safe_uses = Vec::new();
    for entry in walk(&dir) {
        let text = std::fs::read_to_string(&entry).unwrap();
        for line in text.lines().filter(|l| l.contains("|safe")) {
            safe_uses.push(line.trim().to_string());
        }
    }
    assert!(safe_uses.iter().all(|l| l.contains("description_html|safe")), "{safe_uses:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() { files.extend(walk(&path)) } else { files.push(path) }
    }
    files
}

#[tokio::test]
async fn logout_clears_cookie() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", me("alice", "user")).await;
    Mock::given(method("POST")).and(path("/v1/auth/logout")).respond_with(ResponseTemplate::new(204)).expect(1).mount(&web.api).await;
    let page = web.post_form("/logout", &[]).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER);
    assert!(!web.cookies.contains_key("forge_session"));
}

#[tokio::test]
async fn api_401_forces_relogin() {
    let mut web = TestWeb::new().await;
    web.cookies.clear();
    Mock::given(method("POST"))
        .and(path("/v1/auth/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": "stale" })))
        .mount(&web.api)
        .await;
    Mock::given(method("GET")).and(path("/v1/me")).respond_with(ResponseTemplate::new(401)).mount(&web.api).await;
    web.get("/login").await;
    web.get("/auth/callback?code=x").await;
    assert!(web.cookies.contains_key("forge_session"));
    let page = web.get("/").await;
    assert_eq!((page.status, page.location()), (StatusCode::SEE_OTHER, "/login"));
    assert!(!web.cookies.contains_key("forge_session"), "the stale session is gone");
}

#[tokio::test]
async fn static_files_and_not_found() {
    let mut web = TestWeb::new().await;
    let js = web.get("/static/htmx.min.js").await;
    assert_eq!((js.status, js.headers["content-type"].to_str().unwrap()), (StatusCode::OK, "text/javascript"));
    assert_eq!(web.get("/nope").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn checkboxes_are_drawn_like_the_launcher() {
    let mut web = TestWeb::new().await;
    let css = web.get("/static/app.css").await.body;
    let rule = css.split("input[type=\"checkbox\"] {").nth(1).expect("own checkbox rule").split('}').next().unwrap().to_string();
    assert!(rule.contains("appearance: none") && rule.contains("var(--field)") && rule.contains("var(--outline)"), "{rule}");
    assert!(css.contains("input[type=\"checkbox\"]:checked"), "a gold square when ticked");
}

#[tokio::test]
async fn the_launcher_icon_is_the_favicon() {
    let mut web = TestWeb::new().await;
    let page = web.get("/").await;
    assert!(page.body.contains(r#"<link rel="icon" href="/static/icon.png" type="image/png">"#), "{}", page.body);
    assert!(page.body.contains(r#"<link rel="apple-touch-icon" href="/static/icon.png">"#));
    let png = web.get("/static/icon.png").await;
    assert_eq!((png.status, png.headers["content-type"].to_str().unwrap()), (StatusCode::OK, "image/png"));
    let ico = web.get("/favicon.ico").await;
    assert_eq!((ico.status, ico.headers["content-type"].to_str().unwrap()), (StatusCode::OK, "image/x-icon"));
}

#[tokio::test]
async fn api_docs_show_the_public_api_markdown() {
    let mut web = TestWeb::new().await;
    let page = web.get("/docs/api-docs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.body.matches("<h1>").count(), 1, "the page's own title only");
    assert!(page.body.contains("<code>GET /v1/registry</code>"), "{}", page.body);
    assert!(page.body.contains("<table>"));
    // Headings get ids for the side navigation, code keeps its language for highlighting.
    assert!(page.body.contains(r#"<h3 id="conditions">Conditions</h3>"#) && page.body.contains(r##"<a href="#conditions">Conditions</a>"##));
    assert!(page.body.contains(r##"<li class="sub"><a href="#python-standard-library">"##), "{}", page.body);
    assert!(page.body.contains(r#"<code class="language-rust">"#));
    assert!(web.get("/static/api-docs.js").await.body.contains("tok-keyword"));
    assert!(web.get("/docs").await.body.contains(r#"href="/docs/api-docs""#), "linked from the docs page");
}

#[tokio::test]
async fn creator_guide_shows_the_markdown_with_working_step_links() {
    let mut web = TestWeb::new().await;
    let page = web.get("/docs/creator-guide").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.body.matches("<h1>").count(), 1, "the page's own title only");
    assert!(page.body.contains("<title>Guide for addon creators - SurvivorsForge</title>"));
    assert!(!page.body.contains("/docs/api-docs/playground"), "no playground button here");
    // The step list at the top links to the headings by the same ids GitHub gives them.
    assert!(page.body.contains(r#"<h3 id="5-add-the-release-workflow">"#), "{}", page.body);
    assert!(page.body.contains(r##"href="#5-add-the-release-workflow""##), "{}", page.body);
    assert!(page.body.contains("name: Release\n\n# Adds a \"Run workflow\""), "the workflow stays a code block, its comment no heading");
    assert!(web.get("/docs").await.body.contains(r#"href="/docs/creator-guide""#), "linked from the docs page");
}

#[tokio::test]
async fn addon_api_shows_the_game_docs_with_their_date_and_the_discord_channel() {
    let mut web = TestWeb::new().await;
    let page = web.get("/docs/addon-api").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<title>Addon API of the game - SurvivorsForge</title>"));
    assert!(page.body.contains("27 October 2026 at"), "the version it is, for later updates");
    assert!(page.body.contains(r#"href="https://discord.com/channels/1557821819032051852/1564599899469914203""#), "{}", page.body);
    assert!(page.body.contains(r#"<h4 id="azs-getruntime-seconds-or-nil">"#), "{}", page.body);
    assert!(web.get("/docs").await.body.contains(r#"href="/docs/addon-api""#), "linked from the docs page");
    assert!(web.get("/docs/creator-guide").await.body.contains(r#"href="https://forge.rezondes.net/docs/addon-api""#), "linked from the guide");
}

#[tokio::test]
async fn playground_shows_the_answer_and_reaches_only_public_endpoints() {
    let mut web = TestWeb::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/addons/aetherhud/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "tag": "v2.3.0" }])))
        .mount(&web.api)
        .await;
    let page = web.get("/docs/api-docs/playground?endpoint=versions&slug=aetherhud").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("GET /v1/addons/aetherhud/versions"), "{}", page.body);
    assert!(page.body.contains("&#34;tag&#34;: &#34;v2.3.0&#34;") || page.body.contains("&quot;tag&quot;: &quot;v2.3.0&quot;"), "{}", page.body);

    let page = web.get("/docs/api-docs/playground?endpoint=addon&slug=..%2Fme").await;
    assert!(page.body.contains("Enter an addon id"), "{}", page.body);
    let page = web.get("/docs/api-docs/playground?endpoint=findings&slug=aetherhud&tag=v1%3Fx%3D1").await;
    assert!(page.body.contains("Enter a version tag"), "{}", page.body);
    assert!(web.get("/docs/api-docs").await.body.contains(r#"href="/docs/api-docs/playground""#));
}

#[tokio::test]
async fn docs_page_explains_each_document_before_it_is_opened() {
    let mut web = TestWeb::new().await;
    let page = web.get("/docs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<title>Documentation - SurvivorsForge</title>"), "{}", page.body);
    for (href, title) in [("/docs/api-docs", "API for launchers"), ("/docs/creator-guide", "Guide for addon creators"), ("/docs/addon-api", "Addon API of the game")] {
        assert!(page.body.contains(&format!(r#"href="{href}""#)), "{href}: {}", page.body);
        assert!(page.body.contains(title), "{title}: {}", page.body);
    }
    assert!(page.body.contains("Lua functions the game adds"), "says what the addon API is: {}", page.body);
    let footer = web.get("/").await.body;
    assert!(footer.contains(r#"<a href="/docs">Documentation</a>"#), "{footer}");
    assert!(!footer.contains(r#"href="/docs/api-docs""#) && !footer.contains(r#"href="/api-docs""#), "one footer link only");
}

#[tokio::test]
async fn old_docs_paths_redirect_to_their_new_place() {
    let mut web = TestWeb::new().await;
    for (old, new) in [
        ("/api-docs", "/docs/api-docs"),
        ("/api-docs/playground?endpoint=versions&slug=x", "/docs/api-docs/playground?endpoint=versions&slug=x"),
        ("/creator-guide", "/docs/creator-guide"),
        ("/addon-api", "/docs/addon-api"),
    ] {
        let page = web.get(old).await;
        assert_eq!(page.status, StatusCode::PERMANENT_REDIRECT, "{old}");
        assert_eq!(page.headers["location"], new, "{old}");
    }
}

#[tokio::test]
async fn the_nav_link_of_the_current_section_is_marked() {
    let mut web = TestWeb::new().await;
    web.login_as("tok", common::me("alice", "admin")).await;
    for (route, body) in [("/v1/me/addons", json!([])), ("/v1/admin/review-queue", json!([])), ("/v1/admin/reports", json!([]))] {
        Mock::given(method("GET")).and(path(route)).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&web.api).await;
    }
    // Pages whose API call fails still show the nav of their section.
    for (page, link) in [("/addons", "/addons"), ("/addons/lucky", "/addons"), ("/addons/lucky/versions", "/addons"), ("/packs", "/packs"), ("/packs/new", "/packs"), ("/me/addons", "/me/addons"), ("/admin", "/admin")] {
        let body = web.get(page).await.body;
        assert!(body.contains(&format!(r#"<a href="{link}" aria-current="page">"#)), "{page}: {body}");
        let top = body.split("</header>").next().unwrap();
        assert_eq!(top.matches("aria-current").count(), 1, "{page}: only one nav link is current: {top}");
    }
    let top = web.get("/docs").await.body;
    let top = top.split("</header>").next().unwrap();
    assert!(!top.contains("aria-current"), "no section is current on other pages: {top}");
}
