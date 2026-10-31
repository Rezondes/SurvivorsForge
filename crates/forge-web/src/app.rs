//! The router of the website.

use crate::api_client::ApiClient;
use crate::config::Config;
use crate::csrf::{self, CsrfToken};
use crate::error::{WebError, WebResult, error_page};
use crate::session::{self, SESSION_COOKIE};
use crate::templates::{ApiDocsPage, ApiPlaygroundPage, IndexPage, LoginPage, TocEntry, Visitor, render};
use axum::extract::{FromRef, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router, middleware};
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::Key;
use forge_core::rate_limit::RateLimiter;
use serde::Deserialize;
use serde_json::json;
use sha2::Digest;
use std::sync::Arc;
use tower_http::set_header::SetResponseHeaderLayer;

pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' blob: https://avatars.githubusercontent.com; \
                       form-action 'self'; frame-ancestors 'none'; base-uri 'none'; connect-src 'self'";

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub api: ApiClient,
    pub key: Key,
}

impl AppState {
    pub fn new(config: Config) -> AppState {
        // Key wants 64 bytes; SHA-512 stretches COOKIE_KEY to exactly that.
        let key = Key::from(&sha2::Sha512::digest(config.cookie_key.as_bytes()));
        AppState { api: ApiClient::new(&config.api_internal_url), config: Arc::new(config), key }
    }

    pub fn install_url(&self) -> String {
        format!("https://github.com/apps/{}/installations/new", self.config.github_app_slug)
    }

    /// Where uploaded images live; descriptions may only show images from here.
    pub fn image_base(&self) -> String {
        format!("{}/img", self.config.public_web_url)
    }

    pub fn secure_cookies(&self) -> bool {
        self.config.public_web_url.starts_with("https://")
    }
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Key {
        state.key.clone()
    }
}

/// Every category in display order.
pub async fn categories(state: &AppState) -> WebResult<Vec<crate::templates::CategoryOption>> {
    Ok(crate::templates::CategoryOption::list(&state.api.get("/v1/categories", None).await?))
}

/// The categories an addon can go into.
pub async fn selectable_categories(state: &AppState) -> WebResult<Vec<crate::templates::CategoryOption>> {
    Ok(categories(state).await?.into_iter().filter(|c| c.selectable).collect())
}

/// The logged-in visitor and their API token, if there is a session.
pub async fn visitor(state: &AppState, jar: &PrivateCookieJar) -> WebResult<Option<(Visitor, String)>> {
    let Some(token) = session::token(jar) else { return Ok(None) };
    let me = state.api.get("/v1/me", Some(&token)).await?;
    Ok(Some((Visitor::from_me(&me), token)))
}

/// The site's own address, for the absolute links that link previews need (`og:image`).
static PUBLIC_URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn public_url() -> &'static str {
    PUBLIC_URL.get().map_or("", String::as_str)
}

pub fn router(state: AppState) -> Router {
    let _ = PUBLIC_URL.set(state.config.public_web_url.clone());
    let pages = Router::new()
        .route("/", get(index))
        .route("/api-docs", get(api_docs))
        .route("/api-docs/playground", get(api_playground))
        .route("/addons", get(crate::public::addons))
        .route("/addons/{slug}", get(crate::public::addon))
        .route("/addons/{slug}/versions", get(crate::public::versions))
        .route("/addons/{slug}/versions/{tag}", get(crate::public::version))
        .route("/addons/{slug}/versions/{tag}/download", get(crate::public::download))
        .route("/stats", get(crate::stats::stats))
        .route("/packs", get(crate::packs::packs))
        .route("/packs/new", get(crate::packs::new_pack).post(crate::packs::create_pack))
        .route("/packs/{id}", get(crate::packs::pack))
        .route("/packs/{id}/edit", get(crate::packs::edit_pack).post(crate::packs::save_pack))
        .route("/packs/{id}/download", get(crate::packs::download))
        .route("/me/addons", get(crate::creator::my_addons))
        .route(
            "/scan",
            get(crate::creator::scan_page).post(crate::creator::scan).layer(axum::extract::DefaultBodyLimit::max(crate::csrf::MAX_UPLOAD_FORM)),
        )
        .route(
            "/addons/new",
            get(crate::creator::new_addon)
                .post(crate::creator::create_addon)
                .layer(axum::extract::DefaultBodyLimit::max(crate::csrf::MAX_UPLOAD_FORM)),
        )
        .route("/addons/new/repos", get(crate::creator::repo_suggestions))
        .route("/addons/new/repo", get(crate::creator::repo_pick))
        .route("/addons/{slug}/edit", get(crate::creator::edit_addon).post(crate::creator::save_addon))
        .route("/addons/{slug}/preview", post(crate::creator::preview))
        .route("/addons/{slug}/claim", post(crate::creator::claim))
        .route("/addons/{slug}/sync", post(crate::creator::sync))
        .route(
            "/addons/{slug}/images",
            get(crate::creator::images)
                .post(crate::creator::upload_image)
                .layer(axum::extract::DefaultBodyLimit::max(crate::csrf::MAX_UPLOAD_FORM)),
        )
        .route("/addons/{slug}/images/order", post(crate::creator::order_images))
        .route("/addons/{slug}/icon/delete", post(crate::creator::delete_icon))
        .route("/addons/{slug}/images/{id}/move", post(crate::creator::move_image))
        .route("/addons/{slug}/images/{id}/delete", post(crate::creator::delete_image))
        .route("/addons/{slug}/images/{id}/caption", post(crate::creator::caption_image))
        .route("/addons/{slug}/members", get(crate::creator::members).post(crate::creator::add_member))
        .route("/addons/{slug}/members/{github_id}", post(crate::creator::update_member))
        .route("/addons/{slug}/members/{github_id}/delete", post(crate::creator::remove_member))
        .route("/addons/{slug}/dependencies", get(crate::creator::deps).post(crate::creator::save_deps))
        .route("/addons/{slug}/versions/{tag}/edit", get(crate::creator::version_edit).post(crate::creator::save_version))
        .route("/addons/{slug}/delete", get(crate::creator::delete_page).post(crate::creator::delete_addon))
        .route("/admin", get(crate::admin::index))
        .route("/admin/review", get(crate::admin::review))
        .route("/admin/review/{slug}", get(crate::admin::review_addon))
        .route("/admin/review/{slug}/{action}", post(crate::admin::review_bulk))
        .route("/admin/versions/{id}/{action}", post(crate::admin::version_action))
        .route("/admin/reports", get(crate::admin::reports))
        .route("/admin/reports/{id}/resolve", post(crate::admin::resolve_report))
        .route("/admin/users", get(crate::admin::users))
        .route("/admin/users/{id}/{action}", post(crate::admin::ban))
        .route("/admin/addons", get(crate::admin::addons))
        .route("/admin/addons/new", get(crate::admin::add_addon_page).post(crate::admin::add_addon))
        .route("/admin/addons/sync", post(crate::admin::sync_all))
        .route("/admin/addons/{slug}", get(crate::admin::addon))
        .route("/admin/addons/{slug}/{action}", post(crate::admin::addon_action))
        .route("/admin/audit", get(crate::admin::audit))
        .route("/admin/stats", get(crate::stats::admin_stats))
        .route("/admin/list/{kind}", get(crate::admin::list_page).post(crate::admin::list_create))
        .route("/admin/list/{kind}/{key}/{action}", post(crate::admin::list_update))
        .route("/login", get(login))
        .route("/auth/grafana", get(crate::admin::grafana_auth))
        .route("/auth/callback", get(callback))
        .route("/logout", post(logout))
        .route("/static/htmx.min.js", get(|| async { asset("text/javascript", HTMX_JS) }))
        .route("/static/app.css", get(|| async { asset("text/css", APP_CSS) }))
        .route("/static/view.js", get(|| async { asset("text/javascript", VIEW_JS) }))
        .route("/static/api-docs.js", get(|| async { asset("text/javascript", API_DOCS_JS) }))
        // The launcher's fonts (SIL OFL, licenses next to them in ui/fonts).
        // The launcher's icon, as tab icon and home screen icon.
        .route("/static/icon.png", get(|| async { binary("image/png", include_bytes!("../../../assets/icon.png")) }))
        .route("/favicon.ico", get(|| async { binary("image/x-icon", include_bytes!("../../../assets/icon.ico")) }))
        .route("/static/fonts/marcellus.ttf", get(|| async { font(include_bytes!("../../../ui/fonts/Marcellus-Regular.ttf")) }))
        .route("/static/fonts/morpheus.ttf", get(|| async { font(include_bytes!("../../../ui/fonts/Morpheus.ttf")) }))
        .fallback(|| async { WebError::NotFound })
        .layer(middleware::from_fn_with_state(state.clone(), csrf::middleware));
    let limiter = Arc::new(RateLimiter::new(state.config.rate_limit_per_minute, state.config.trusted_proxies.clone()));
    Router::new()
        .route("/healthz", get(healthz))
        .merge(pages)
        .layer(middleware::from_fn_with_state(state.clone(), crate::visits::middleware))
        .with_state(state)
        .layer(middleware::from_fn_with_state(limiter, crate::rate_limit::middleware))
        .layer(middleware::from_fn(crate::metrics::middleware))
        // Pages may show a logged-in visitor's data: no browser or proxy keeps them.
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .layer(SetResponseHeaderLayer::if_not_present(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP)))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin")))
}

fn binary(content_type: &'static str, body: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "public, max-age=86400")], body).into_response()
}

fn font(body: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, "font/ttf"), (header::CACHE_CONTROL, "public, max-age=604800, immutable")], body).into_response()
}

const HTMX_JS: &str = include_str!("../static/htmx.min.js");
const APP_CSS: &str = include_str!("../static/app.css");
const VIEW_JS: &str = include_str!("../static/view.js");
const API_DOCS_JS: &str = include_str!("../static/api-docs.js");

/// Goes into the links to the stylesheet and scripts (`?v=`) and changes with their content, so a
/// deploy reaches browsers that still cache the old files.
pub fn asset_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let hash = sha2::Sha256::new().chain_update(HTMX_JS).chain_update(APP_CSS).chain_update(VIEW_JS).chain_update(API_DOCS_JS).finalize();
        hash[..6].iter().map(|b| format!("{b:02x}")).collect()
    });
    &VERSION
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "public, max-age=86400")], body).into_response()
}

async fn healthz(State(state): State<AppState>) -> Response {
    if state.api.healthy().await {
        Json(json!({ "status": "ok", "api": "ok" })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "degraded", "api": "down" }))).into_response()
    }
}

async fn index(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    // A teaser only: without the API the page still loads, without it.
    let popular = match state.api.get("/v1/addons?sort=downloads&per_page=6", None).await {
        Ok(list) => list["addons"].as_array().into_iter().flatten().map(crate::public::card).collect(),
        Err(_) => Vec::new(),
    };
    Ok(render(&IndexPage { user, csrf: csrf.0, popular }))
}

/// The public API for other launchers, from the same file as `docs/public-api.md` on GitHub.
async fn api_docs(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let md = include_str!("../../../docs/public-api.md");
    // The page has its own h1; the Markdown's title would come out as a second heading.
    let (description_html, toc) = heading_ids(&forge_core::markdown::render(md.split_once('\n').map_or(md, |(_, rest)| rest), ""));
    Ok(render(&ApiDocsPage { user, csrf: csrf.0, description_html, toc }))
}

/// Gives every `h3` and `h4` of rendered Markdown an id from its text, and lists them for the
/// table of contents. The sanitizer drops ids, so they are added after it.
fn heading_ids(html: &str) -> (String, Vec<TocEntry>) {
    let (mut out, mut toc, mut rest) = (String::new(), Vec::<TocEntry>::new(), html);
    while let Some(start) = ["<h3>", "<h4>"].iter().filter_map(|tag| rest.find(tag)).min() {
        let level = &rest[start + 2..start + 3];
        let close = format!("</h{level}>");
        let Some(end) = rest[start..].find(&close).map(|e| start + e) else { break };
        let inner = &rest[start + 4..end];
        let text = strip_tags(inner).replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
        let base = text.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
        let taken = toc.iter().filter(|t| t.id == base || t.id.starts_with(&format!("{base}-"))).count();
        let id = if taken == 0 { base } else { format!("{base}-{}", taken + 1) };
        out.push_str(&rest[..start]);
        out.push_str(&format!("<h{level} id=\"{id}\">{inner}</h{level}>"));
        toc.push(TocEntry { id, text, sub: level == "4" });
        rest = &rest[end + close.len()..];
    }
    out.push_str(rest);
    (out, toc)
}

fn strip_tags(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text
}

#[derive(Deserialize, Default)]
struct PlaygroundQuery {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    tag: String,
}

/// The API path for a playground form, or what is wrong with it. Slugs and tags are checked so
/// that the form reaches nothing but the four public endpoints.
fn playground_path(q: &PlaygroundQuery) -> Result<String, &'static str> {
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c));
    let (slug, tag) = (q.slug.trim(), q.tag.trim());
    let slug_ok = || if plain(slug) { Ok(()) } else { Err("Enter an addon id, e.g. aetherhud") };
    match q.endpoint.as_str() {
        "registry" => Ok("/v1/registry".into()),
        "addon" => slug_ok().map(|_| format!("/v1/addons/{slug}")),
        "versions" => slug_ok().map(|_| format!("/v1/addons/{slug}/versions")),
        "findings" => {
            slug_ok()?;
            if plain(tag) { Ok(format!("/v1/addons/{slug}/versions/{tag}/findings")) } else { Err("Enter a version tag, e.g. v2.3.0") }
        }
        _ => Err("Pick an endpoint"),
    }
}

/// Tries the public API from the browser: the website asks the API and shows its JSON.
async fn api_playground(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Query(q): Query<PlaygroundQuery>,
) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let (mut path, mut error, mut result) = (String::new(), String::new(), String::new());
    if !q.endpoint.is_empty() {
        match playground_path(&q) {
            Ok(p) => {
                let answer = state.api.get(&p, None).await.unwrap_or_else(|e| json!({ "error": e.code(), "message": e.message() }));
                result = serde_json::to_string_pretty(&answer).unwrap_or_default();
                path = p;
            }
            Err(e) => error = e.into(),
        }
    }
    let endpoint = if q.endpoint.is_empty() { "registry".into() } else { q.endpoint };
    let api_url = format!("{}{path}", state.config.public_api_url);
    Ok(render(&ApiPlaygroundPage { user, csrf: csrf.0, endpoint, slug: q.slug, tag: q.tag, path, api_url, error, result }))
}

async fn login(State(state): State<AppState>, jar: PrivateCookieJar) -> Response {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/v1/auth/github/start", state.config.public_api_url),
        &[("return_to", state.config.callback_url())],
    )
    .expect("valid API URL");
    // Lives until the callback removes it or the browser closes.
    let started = session::cookie(session::LOGIN_COOKIE, "1".into(), state.secure_cookies());
    // A page that moves on at once rather than a redirect: link previews stop here and show this
    // site instead of following through to GitHub's login.
    (jar.add(started), render(&LoginPage { user: None, csrf: String::new(), start: url.to_string() })).into_response()
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
}

/// The end of a GitHub login. Counts only when the login started in this browser (`LOGIN_COOKIE`).
async fn callback(State(state): State<AppState>, jar: PrivateCookieJar, Query(query): Query<CallbackQuery>) -> Response {
    let started = jar.get(session::LOGIN_COOKIE).is_some();
    let jar = jar.remove(session::removal(session::LOGIN_COOKIE));
    let message = match (query.code, query.error.as_deref()) {
        (Some(_), None) if !started => "This login was not started here. Please log in again.".into(),
        (Some(code), None) => {
            let body = json!({ "code": code, "return_to": state.config.callback_url() });
            match state.api.post("/v1/auth/exchange", None, &body).await {
                Ok(answer) => {
                    let token = answer["token"].as_str().unwrap_or_default().to_string();
                    let jar = jar.add(session::cookie(SESSION_COOKIE, token, state.secure_cookies()));
                    return (jar, Redirect::to("/")).into_response();
                }
                Err(e) => e.message(),
            }
        }
        (_, Some("banned")) => "This account is banned.".into(),
        (_, Some("denied")) => "The login was cancelled on GitHub.".into(),
        _ => "The login did not work. Please try again.".into(),
    };
    (jar, error_page(StatusCode::BAD_REQUEST, "Login failed", &message)).into_response()
}

async fn logout(State(state): State<AppState>, jar: PrivateCookieJar) -> Response {
    if let Some(token) = session::token(&jar) {
        let _ = state.api.post("/v1/auth/logout", Some(&token), &json!({})).await;
    }
    (jar.remove(session::removal(SESSION_COOKIE)), Redirect::to("/")).into_response()
}
