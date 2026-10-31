//! The admin pages. Every action asks for a note where the API needs one, and every result comes
//! back to the page it started from as a message.

use crate::api_client::ApiError;
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::templates::{
    AdminAddAddonPage, AdminAddonPage, AdminAuditPage, AdminIndexPage, AdminListPage, AdminReportsPage, AdminReviewPage, AdminUsersPage, Permission, Visitor, render,
};
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The logged-in moderator or admin, or `403` for everyone else.
async fn admin(state: &AppState, jar: &PrivateCookieJar) -> WebResult<(Visitor, String)> {
    let (user, token) = visitor(state, jar).await?.ok_or(WebError::LoginRequired)?;
    if !user.staff {
        return Err(WebError::Forbidden);
    }
    Ok((user, token))
}

/// `GET /auth/grafana`, asked by Caddy's `forward_auth` before every request to `/grafana/`:
/// `200` with `X-Grafana-User` for whoever may see Grafana, the login for nobody, `403` for the rest.
/// Grafana trusts that header from Caddy only, so it is checked again on each request and a taken
/// right ends access at once.
pub async fn grafana_auth(State(state): State<AppState>, jar: PrivateCookieJar) -> WebResult<Response> {
    let (user, _) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    if !user.can(Permission::ViewGrafana) {
        return Err(WebError::Forbidden);
    }
    Ok(([("x-grafana-user", user.login)], axum::http::StatusCode::OK).into_response())
}

/// `?msg=` and `?error=` of a page, set by the action that redirected here.
#[derive(Deserialize, Default)]
pub struct Flash {
    msg: Option<String>,
    error: Option<String>,
    #[serde(flatten)]
    rest: HashMap<String, String>,
}

fn back(page: &str, result: Result<Value, ApiError>, done: &str) -> WebResult<Response> {
    let (key, text) = match result {
        Ok(_) => ("msg", done.to_string()),
        Err(ApiError::Unauthorized) => return Err(WebError::Api(ApiError::Unauthorized)),
        Err(ApiError::Down(e)) => return Err(WebError::Api(ApiError::Down(e))),
        Err(e) => ("error", e.message()),
    };
    let sep = if page.contains('?') { '&' } else { '?' };
    let url = reqwest::Url::parse_with_params("http://x/", &[(key, text)]).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
    Ok(Redirect::to(&format!("{page}{sep}{url}")).into_response())
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[derive(Deserialize, Default)]
pub struct NoteForm {
    #[serde(default)]
    note: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    login: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    sort_key: String,
    /// Admin page to return to after a version action.
    #[serde(default)]
    back: String,
    /// Categories only: the top-level category, empty for none.
    #[serde(default)]
    parent: String,
}

pub async fn index(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let queue = state.api.get("/v1/admin/review-queue", Some(&token)).await?;
    let reports = state.api.get("/v1/admin/reports", Some(&token)).await?;
    Ok(render(&AdminIndexPage {
        stats: user.can(Permission::ViewAdminStats),
        grafana: user.can(Permission::ViewGrafana),
        user: Some(user),
        csrf: csrf.0,
        queue: queue.as_array().map(Vec::len).unwrap_or(0),
        reports: reports.as_array().map(Vec::len).unwrap_or(0),
    }))
}

/// The review queue, oldest version first.
async fn review_queue(state: &AppState, token: &str) -> WebResult<Vec<crate::templates::ReviewItem>> {
    let queue = state.api.get("/v1/admin/review-queue", Some(token)).await?;
    Ok(queue
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| crate::templates::ReviewItem {
            version_id: q["version_id"].as_i64().unwrap_or(0),
            slug: s(&q["slug"]),
            name: s(&q["name"]),
            tag: s(&q["tag"]),
            trust: s(&q["trust"]),
            compare_url: q["compare_url"].as_str().map(str::to_string),
            findings: q["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| crate::templates::FindingRow {
                    severity: s(&f["severity"]),
                    rule: s(&f["rule"]),
                    place: match f["line"].as_i64() {
                        Some(line) => format!("{}:{line}", s(&f["file"])),
                        None => s(&f["file"]),
                    },
                    message: s(&f["message"]),
                    snippet: s(&f["snippet"]),
                    hint: forge_scan::hint(&s(&f["rule"]), &s(&f["snippet"])).unwrap_or_default().into(),
                })
                .collect(),
        })
        .collect())
}

/// `GET /admin/review`: the addons with versions to review, in queue order.
pub async fn review(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Query(flash): Query<Flash>) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let mut addons: Vec<crate::templates::ReviewAddon> = Vec::new();
    for item in review_queue(&state, &token).await? {
        match addons.iter_mut().find(|a| a.slug == item.slug) {
            Some(a) => {
                a.versions += 1;
                if !a.trusts.contains(&item.trust) {
                    a.trusts.push(item.trust);
                }
            }
            None => addons.push(crate::templates::ReviewAddon { slug: item.slug, name: item.name, versions: 1, trusts: vec![item.trust] }),
        }
    }
    Ok(render(&AdminReviewPage { user: Some(user), csrf: csrf.0, addons, msg: flash.msg, error: flash.error }))
}

/// `GET /admin/review/{slug}`: the versions of one addon to review.
pub async fn review_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(flash): Query<Flash>,
) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let items: Vec<_> = review_queue(&state, &token).await?.into_iter().filter(|i| i.slug == slug).collect();
    let name = items.first().map_or_else(|| slug.clone(), |i| i.name.clone());
    Ok(render(&crate::templates::AdminReviewAddonPage { user: Some(user), csrf: csrf.0, slug, name, items, msg: flash.msg, error: flash.error }))
}

/// `POST /admin/review/{slug}/{verify|skip|block}`: one action for every queued version of an
/// addon. Verifying a changed file accepts the new one.
pub async fn review_bulk(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((slug, action)): Path<(String, String)>,
    Form(form): Form<NoteForm>,
) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let done = match action.as_str() {
        "verify" => "verified",
        "skip" => "skipped",
        "block" => "blocked",
        _ => return Err(WebError::NotFound),
    };
    let items: Vec<_> = review_queue(&state, &token).await?.into_iter().filter(|i| i.slug == slug).collect();
    let body = json!({ "note": form.note });
    let mut failed = Vec::new();
    for item in &items {
        let id = item.version_id;
        let route = match action.as_str() {
            "verify" if item.trust == "source_changed" => format!("/v1/admin/versions/{id}/accept-source"),
            "verify" => format!("/v1/admin/versions/{id}/verification"),
            _ => format!("/v1/admin/versions/{id}/{action}"),
        };
        match state.api.post(&route, Some(&token), &body).await {
            Ok(_) => {}
            Err(e @ (ApiError::Unauthorized | ApiError::Down(_))) => return Err(e.into()),
            Err(e) => failed.push(format!("{}: {}", item.tag, e.message())),
        }
    }
    let name = items.first().map_or(slug.as_str(), |i| i.name.as_str());
    let versions = if items.len() == 1 { "version" } else { "versions" };
    let result = if failed.is_empty() {
        Ok(Value::Null)
    } else {
        let message = format!("{} of {} {versions} of {name} {done}. {}", items.len() - failed.len(), items.len(), failed.join(" "));
        Err(ApiError::Api { status: 422, code: "partly_done".into(), message })
    };
    back("/admin/review", result, &format!("{} {versions} of {name} {done}.", items.len()))
}

pub async fn version_action(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, action)): Path<(i64, String)>,
    Form(form): Form<NoteForm>,
) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let body = json!({ "note": form.note });
    let (method, route, done) = match action.as_str() {
        "verify" => (Method::POST, format!("/v1/admin/versions/{id}/verification"), "Version verified."),
        "skip" => (Method::POST, format!("/v1/admin/versions/{id}/skip"), "Version skipped: it stays as checked, without verification."),
        "unverify" => (Method::DELETE, format!("/v1/admin/versions/{id}/verification"), "Verification removed."),
        "block" => (Method::POST, format!("/v1/admin/versions/{id}/block"), "Version blocked."),
        "unblock" => (Method::DELETE, format!("/v1/admin/versions/{id}/block"), "Version unblocked."),
        "accept-source" => (Method::POST, format!("/v1/admin/versions/{id}/accept-source"), "New file accepted."),
        _ => return Err(WebError::NotFound),
    };
    // Only our own admin pages, never a URL from the form.
    let own = form.back.starts_with("/admin/addons/") || form.back.starts_with("/admin/review/");
    let page = if own && !form.back.contains("//") { form.back.as_str() } else { "/admin/review" };
    back(page, state.api.call(method, &route, Some(&token), Some(&body)).await, done)
}

pub async fn reports(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Query(flash): Query<Flash>) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let list = state.api.get("/v1/admin/reports", Some(&token)).await?;
    let rows = list
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| crate::templates::ReportRow {
            id: r["id"].as_i64().unwrap_or(0),
            target: format!("{} {}", s(&r["target_type"]), s(&r["target_id"])),
            addon: s(&r["addon"]),
            reason: s(&r["reason"]),
            text: s(&r["text"]),
            reporter: s(&r["reporter"]),
            created: s(&r["created_at"]).chars().take(16).collect(),
        })
        .collect();
    Ok(render(&AdminReportsPage { user: Some(user), csrf: csrf.0, rows, msg: flash.msg, error: flash.error }))
}

pub async fn resolve_report(State(state): State<AppState>, jar: PrivateCookieJar, Path(id): Path<i64>, Form(form): Form<NoteForm>) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let body = json!({ "status": form.status, "note": form.note });
    back("/admin/reports", state.api.post(&format!("/v1/admin/reports/{id}/resolve"), Some(&token), &body).await, "Report closed.")
}

pub async fn users(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Query(flash): Query<Flash>) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let login = flash.rest.get("login").cloned().unwrap_or_default();
    let found = if login.trim().is_empty() {
        None
    } else {
        match state.api.get(&format!("/v1/admin/users/{}", login.trim()), Some(&token)).await {
            Ok(u) => Some(crate::templates::UserRow {
                github_id: u["github_id"].as_i64().unwrap_or(0),
                login: s(&u["login"]),
                role: s(&u["role"]),
                banned: u["banned"] == true,
                ban_reason: s(&u["ban_reason"]),
                addons: u["addons"].as_i64().unwrap_or(0),
            }),
            Err(ApiError::Api { status: 404, .. }) => None,
            Err(e) => return Err(e.into()),
        }
    };
    let not_found = !login.trim().is_empty() && found.is_none();
    let admins = state
        .api
        .get("/v1/admin/admins", Some(&token))
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| crate::templates::AdminRow { github_id: a["github_id"].as_i64().unwrap_or(0), login: s(&a["login"]), role: s(&a["role"]), by_config: a["by_config"] == true })
        .collect();
    let manage_roles = user.can(Permission::ManageRoles);
    Ok(render(&AdminUsersPage { user: Some(user), csrf: csrf.0, admins, manage_roles, login, found, not_found, msg: flash.msg, error: flash.error }))
}

/// `ban`, `unban` and `role` (the form's `role`) for one user.
pub async fn ban(State(state): State<AppState>, jar: PrivateCookieJar, Path((github_id, action)): Path<(i64, String)>, Form(form): Form<NoteForm>) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let (result, done) = match action.as_str() {
        "ban" | "unban" => {
            let method = if action == "unban" { Method::DELETE } else { Method::POST };
            let result = state.api.call(method, &format!("/v1/admin/users/{github_id}/ban"), Some(&token), Some(&json!({ "note": form.note }))).await;
            (result, if action == "unban" { "Ban lifted." } else { "User banned." })
        }
        "role" => {
            let body = json!({ "role": form.role, "note": form.note });
            let result = state.api.call(Method::PUT, &format!("/v1/admin/users/{github_id}/role"), Some(&token), Some(&body)).await;
            (result, "Role changed.")
        }
        _ => return Err(WebError::NotFound),
    };
    let page = if form.login.is_empty() { "/admin/users".to_string() } else { format!("/admin/users?login={}", form.login) };
    back(&page, result, done)
}

#[derive(Deserialize)]
pub struct AddonSearch {
    #[serde(default)]
    q: String,
    msg: Option<String>,
    error: Option<String>,
}

/// `GET /admin/addons?q=`: find an addon by name to manage it.
pub async fn addons(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Query(search): Query<AddonSearch>) -> WebResult<Html<String>> {
    let (user, _) = admin(&state, &jar).await?;
    let q = search.q.trim().to_string();
    let query = reqwest::Url::parse_with_params("http://x/v1/addons", &[("q", q.as_str()), ("sort", if q.is_empty() { "name" } else { "relevance" })])
        .map(|u| u.query().unwrap_or_default().to_string())
        .unwrap_or_default();
    let list = state.api.get(&format!("/v1/addons?{query}"), None).await?;
    let found = list["addons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| (s(&a["slug"]), s(&a["name"]), a["trust"].as_str().unwrap_or("none").to_string()))
        .collect();
    Ok(render(&crate::templates::AdminAddonsPage { user: Some(user), csrf: csrf.0, q, found, msg: search.msg, error: search.error }))
}

/// `POST /admin/addons/sync`: checks every addon for updates now, in the background.
pub async fn sync_all(State(state): State<AppState>, jar: PrivateCookieJar) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let result = state.api.post("/v1/admin/addons/sync", Some(&token), &serde_json::json!({})).await;
    back("/admin/addons", result, "Checking every addon for updates in the background.")
}

pub async fn addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(flash): Query<Flash>,
) -> WebResult<Html<String>> {
    let (user, _) = admin(&state, &jar).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let versions = state
        .api
        .get(&format!("/v1/addons/{slug}/versions"), None)
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| crate::templates::AdminVersionRow {
            id: v["id"].as_i64().unwrap_or(0),
            tag: s(&v["tag"]),
            published: s(&v["published_at"]).chars().take(10).collect(),
            trust: s(&v["trust"]),
            critical: v["findings"]["critical"].as_i64().unwrap_or(0),
            warn: v["findings"]["warn"].as_i64().unwrap_or(0),
        })
        .collect();
    Ok(render(&AdminAddonPage {
        user: Some(user),
        csrf: csrf.0,
        versions,
        slug,
        name: s(&a["name"]),
        owner: if a["owner"].is_object() { s(&a["owner"]["login"]) } else { String::new() },
        blocked: a["blocked"] == true,
        block_reason: s(&a["block_reason"]),
        msg: flash.msg,
        error: flash.error,
    }))
}

pub async fn addon_action(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((slug, action)): Path<(String, String)>,
    Form(form): Form<NoteForm>,
) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let (method, route, body, done) = match action.as_str() {
        "block" => (Method::POST, format!("/v1/admin/addons/{slug}/block"), json!({ "note": form.note }), "Addon blocked."),
        "unblock" => (Method::DELETE, format!("/v1/admin/addons/{slug}/block"), json!({ "note": form.note }), "Addon unblocked."),
        "owner" => (Method::POST, format!("/v1/admin/addons/{slug}/owner"), json!({ "login": form.login, "note": form.note }), "Owner changed."),
        _ => return Err(WebError::NotFound),
    };
    back(&format!("/admin/addons/{slug}"), state.api.call(method, &route, Some(&token), Some(&body)).await, done)
}

#[derive(Deserialize, Default, Clone)]
pub struct AdminAddonForm {
    #[serde(flatten)]
    pub base: crate::creator::NewAddonForm,
    #[serde(default)]
    pub author_name: String,
}

#[derive(Deserialize, Default)]
pub struct AddAddonQuery {
    repo: Option<String>,
}

/// `GET /admin/addons/new?repo=owner/name`: the form, filled in from the repo when one is given.
pub async fn add_addon_page(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Query(query): Query<AddAddonQuery>,
) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let categories = crate::app::selectable_categories(&state).await?;
    let mut form = AdminAddonForm::default();
    form.base.source_type = "release".into();
    let mut filled = None;
    if let Some(repo) = query.repo.as_deref().map(str::trim).filter(|r| r.split('/').count() == 2) {
        let (base, result) = crate::creator::filled_in(&state, &token, repo).await;
        form = AdminAddonForm { author_name: repo.split('/').next().unwrap_or_default().to_string(), base };
        filled = Some(result);
    }
    let (owner, name) = form.base.repo.split_once('/').unwrap_or_default();
    let repo = crate::creator::repo_field(owner, name, true, false);
    Ok(render(&AdminAddAddonPage { user: Some(user), csrf: csrf.0, repo, filled, categories, form, error: None }))
}

pub async fn add_addon(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Form(form): Form<AdminAddonForm>) -> WebResult<Response> {
    let (user, token) = admin(&state, &jar).await?;
    let mut form = form;
    if !form.base.repo_name.trim().is_empty() {
        form.base.repo = format!("{}/{}", form.base.repo_owner.trim(), form.base.repo_name.trim());
    }
    let f = &form.base;
    let folders: Vec<String> = f.folders.split([',', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect();
    let body = json!({
        "repo": f.repo.trim(), "slug": f.slug.trim(), "name": f.name.trim(), "summary": f.summary.trim(), "category": f.category,
        "source_type": f.source_type, "asset_pattern": f.asset_pattern.trim(), "folders": folders, "author_name": form.author_name.trim(),
    });
    match state.api.post("/v1/admin/addons", Some(&token), &body).await {
        Ok(created) => Ok(Redirect::to(&format!("/addons/{}", s(&created["slug"]))).into_response()),
        Err(ApiError::Api { message, status, .. }) if status < 500 && status != 403 => {
            let categories = crate::app::selectable_categories(&state).await?;
            let (owner, name) = form.base.repo.split_once('/').unwrap_or_default();
            let repo = crate::creator::repo_field(owner, name, true, false);
            Ok(render(&AdminAddAddonPage { user: Some(user), csrf: csrf.0, repo, filled: None, categories, form, error: Some(message) }).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// Game versions and categories share one list page: name, sort key, rename, delete.
pub async fn list_page(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(kind): Path<String>,
    Query(flash): Query<Flash>,
) -> WebResult<Html<String>> {
    let (user, _) = admin(&state, &jar).await?;
    let (title, rows, parents) = match kind.as_str() {
        "game-versions" => {
            let list = state.api.get("/v1/game-versions", None).await?;
            let rows = list.as_array().into_iter().flatten().map(|g| (s(&g["id"]), s(&g["name"]), s(&g["sort_key"]), String::new())).collect();
            ("Game versions", rows, Vec::new())
        }
        "categories" => {
            let list = crate::app::categories(&state).await?;
            let parents = list.iter().filter(|c| c.parent.is_empty()).map(|c| c.name.clone()).collect();
            ("Categories", list.into_iter().map(|c| (c.name.clone(), c.name, c.sort_key, c.parent)).collect(), parents)
        }
        _ => return Err(WebError::NotFound),
    };
    Ok(render(&AdminListPage { user: Some(user), csrf: csrf.0, kind, title: title.into(), rows, parents, msg: flash.msg, error: flash.error }))
}

pub async fn list_create(State(state): State<AppState>, jar: PrivateCookieJar, Path(kind): Path<String>, Form(form): Form<NoteForm>) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let sort_key: Option<i32> = form.sort_key.trim().parse().ok();
    let route = match kind.as_str() {
        "game-versions" => "/v1/admin/game-versions",
        "categories" => "/v1/admin/categories",
        _ => return Err(WebError::NotFound),
    };
    let mut body = json!({ "name": form.name.trim(), "sort_key": sort_key });
    if kind == "categories" {
        body["parent"] = json!(form.parent.trim());
    }
    let result = state.api.post(route, Some(&token), &body).await;
    back(&format!("/admin/list/{kind}"), result, "Added.")
}

pub async fn list_update(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((kind, key, action)): Path<(String, String, String)>,
    Form(form): Form<NoteForm>,
) -> WebResult<Response> {
    let (_, token) = admin(&state, &jar).await?;
    let base = match kind.as_str() {
        "game-versions" => "/v1/admin/game-versions",
        "categories" => "/v1/admin/categories",
        _ => return Err(WebError::NotFound),
    };
    let route = format!("{base}/{key}");
    let result = if action == "delete" {
        state.api.call(Method::DELETE, &route, Some(&token), None).await
    } else {
        let name = Some(form.name.trim()).filter(|n| !n.is_empty());
        let mut body = json!({ "name": name, "sort_key": form.sort_key.trim().parse::<i32>().ok() });
        if kind == "categories" {
            body["parent"] = json!(form.parent.trim());
        }
        state.api.call(Method::PATCH, &route, Some(&token), Some(&body)).await
    };
    back(&format!("/admin/list/{kind}"), result, if action == "delete" { "Deleted." } else { "Saved." })
}

#[derive(Deserialize, Default)]
pub struct AuditFilter {
    #[serde(default)]
    admin: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    target_type: String,
    #[serde(default)]
    target_id: String,
    #[serde(default)]
    page: Option<i64>,
}

pub async fn audit(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Query(f): Query<AuditFilter>) -> WebResult<Html<String>> {
    let (user, token) = admin(&state, &jar).await?;
    let page = f.page.unwrap_or(1).max(1);
    let filters = [("admin", f.admin.clone()), ("action", f.action.clone()), ("target_type", f.target_type.clone()), ("target_id", f.target_id.clone())];
    let mut query: Vec<(&str, String)> = filters.iter().filter(|(_, v)| !v.is_empty()).cloned().collect();
    query.push(("page", page.to_string()));
    let qs = reqwest::Url::parse_with_params("http://x/", &query).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
    let data = state.api.get(&format!("/v1/admin/audit?{qs}"), Some(&token)).await?;
    let entries = data["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| crate::templates::AuditRow {
            when: s(&e["created_at"]).chars().take(16).collect(),
            admin: s(&e["admin"]),
            action: s(&e["action"]),
            target: format!("{} {}", s(&e["target_type"]), s(&e["target_id"])),
            note: s(&e["note"]),
        })
        .collect();
    let total = data["total"].as_i64().unwrap_or(0);
    let link = |p: i64| {
        let mut q: Vec<(&str, String)> = filters.iter().filter(|(_, v)| !v.is_empty()).cloned().collect();
        q.push(("page", p.to_string()));
        reqwest::Url::parse_with_params("http://x/admin/audit", &q).map(|u| format!("/admin/audit?{}", u.query().unwrap_or_default())).unwrap_or_default()
    };
    Ok(render(&AdminAuditPage {
        user: Some(user),
        csrf: csrf.0,
        filter_admin: f.admin,
        filter_action: f.action,
        filter_target_type: f.target_type,
        filter_target_id: f.target_id,
        entries,
        prev: (page > 1).then(|| link(page - 1)),
        next: (page * 50 < total).then(|| link(page + 1)),
    }))
}
