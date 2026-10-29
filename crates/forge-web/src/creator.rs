//! The creator pages: my addons, publishing an addon and everything an owner or contributor
//! changes on it. The API enforces every right; these pages only show what the visitor may do.

use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::templates::{
    DeleteAddonPage, DepsPage, EditAddonPage, Errors, ImagesPage, MembersPage, MyAddonsPage, NewAddonPage, Perms, RepoField, RepoFieldFragment, RepoSuggestionsFragment, VersionEditPage,
    Visitor, render,
};
use axum::Extension;
use axum::extract::{Form, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::api_client::ApiError;

/// The logged-in visitor or the login page.
async fn me(state: &AppState, jar: &PrivateCookieJar) -> WebResult<(Visitor, String)> {
    visitor(state, jar).await?.ok_or(WebError::LoginRequired)
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Which form field an API error belongs to.
fn field_of(code: &str) -> &'static str {
    match code {
        "invalid_slug" | "slug_taken" => "slug",
        "invalid_name" => "name",
        "invalid_summary" => "summary",
        "description_too_long" => "description",
        "invalid_repo" | "app_not_installed" | "repo_private" | "not_repo_admin" | "repo_taken" | "repo_not_found" => "repo",
        "invalid_asset_pattern" | "invalid_source_type" => "asset_pattern",
        "invalid_folder" | "folder_taken" | "folder_reserved" => "folders",
        "unknown_category" => "category",
        _ => "form",
    }
}

/// An API refusal as a message on its field; anything else stays an error page.
fn form_errors(e: ApiError) -> WebResult<Errors> {
    match e {
        ApiError::Api { status, code, message } if status < 500 && status != 404 => {
            Ok(Errors(HashMap::from([(field_of(&code).to_string(), message)])))
        }
        other => Err(other.into()),
    }
}

fn folders_of(text: &str) -> Vec<String> {
    text.split([',', '\n']).map(str::trim).filter(|f| !f.is_empty()).map(str::to_string).collect()
}

async fn categories(state: &AppState) -> WebResult<Vec<crate::templates::CategoryOption>> {
    crate::app::selectable_categories(state).await
}

pub(crate) async fn perms(state: &AppState, slug: &str, token: &str) -> WebResult<Perms> {
    let p = state.api.get(&format!("/v1/addons/{slug}/permissions"), Some(token)).await?;
    Ok(Perms {
        owner: p["owner"] == true,
        edit_description: p["edit_description"] == true,
        edit_images: p["edit_images"] == true,
    })
}

fn forbidden() -> Response {
    crate::error::error_page(StatusCode::FORBIDDEN, "Not allowed", "You may not change this part of the addon.")
}

/// `?msg=` and `?error=` of a page, set by the action that redirected here.
#[derive(Deserialize, Default)]
pub struct Flash {
    msg: Option<String>,
    error: Option<String>,
}

pub async fn my_addons(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Query(flash): Query<Flash>,
) -> WebResult<Html<String>> {
    let (user, token) = me(&state, &jar).await?;
    let list = state.api.get("/v1/me/addons", Some(&token)).await?;
    let rows = list
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| crate::templates::MyAddon {
            slug: s(&a["slug"]),
            name: s(&a["name"]),
            role: s(&a["role"]),
            notice: crate::trust::notice(a["trust"].as_str().unwrap_or("unscanned"), false, false),
            has_version: !a["latest_tag"].is_null(),
            findings: a["findings"]["critical"].as_i64().unwrap_or(0) + a["findings"]["warn"].as_i64().unwrap_or(0),
            blocked: a["blocked"] == true,
            locked: a["locked"] == true,
        })
        .collect();
    let rows: Vec<crate::templates::MyAddon> = rows;
    let claimable = rows.iter().any(|r| r.role == "claimable");
    Ok(render(&MyAddonsPage { user: Some(user), csrf: csrf.0, rows, claimable, install_url: state.install_url(), msg: flash.msg, error: flash.error }))
}

#[derive(Deserialize, Default)]
pub struct ClaimForm {
    #[serde(default)]
    back: String,
}

/// `POST /addons/{slug}/claim`: an addon an admin added becomes the visitor's. The API checks on
/// GitHub that they are an admin of the repo and that the App is installed there.
pub async fn claim(State(state): State<AppState>, jar: PrivateCookieJar, Path(slug): Path<String>, Form(form): Form<ClaimForm>) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    // Only the two pages with the button, never a URL from the form.
    let page = if form.back == format!("/addons/{slug}") { form.back.as_str() } else { "/me/addons" };
    let (page, key, text) = match state.api.post(&format!("/v1/addons/{}/claim", seg(&slug)), Some(&token), &json!({})).await {
        Ok(_) => ("/me/addons", "msg", "The addon is yours now. Edit it, add screenshots or invite contributors.".to_string()),
        Err(e @ (ApiError::Unauthorized | ApiError::Down(_))) => return Err(e.into()),
        Err(e) => (page, "error", e.message()),
    };
    let query = reqwest::Url::parse_with_params("http://x/", &[(key, text)]).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
    Ok(Redirect::to(&format!("{page}?{query}")).into_response())
}

/// `POST /addons/{slug}/sync`: the owner or a contributor checks the addon for updates now.
/// The API allows it once every five minutes per user.
pub async fn sync(State(state): State<AppState>, jar: PrivateCookieJar, Path(slug): Path<String>) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    let (key, text) = match state.api.post(&format!("/v1/addons/{}/sync", seg(&slug)), Some(&token), &json!({})).await {
        Ok(_) => ("msg", "Checking GitHub for updates. New versions show up here in a minute.".to_string()),
        Err(e @ (ApiError::Unauthorized | ApiError::Down(_))) => return Err(e.into()),
        Err(e) => ("error", e.message()),
    };
    let query = reqwest::Url::parse_with_params("http://x/", &[(key, text)]).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
    Ok(Redirect::to(&format!("/me/addons?{query}")).into_response())
}

#[derive(Deserialize, Default, Clone)]
pub struct NewAddonForm {
    /// `owner/name`; the forms send `repo_owner` and `repo_name` instead.
    #[serde(default)]
    pub repo: String,
    /// Admins only: creators publish repos of their own account.
    #[serde(default)]
    pub repo_owner: String,
    #[serde(default)]
    pub repo_name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub source_type: String,
    #[serde(default)]
    pub asset_pattern: String,
    #[serde(default)]
    pub folders: String,
    #[serde(default)]
    pub description_md: String,
}

/// The repo input of the publish forms; `admin` on the admin form, where the account is editable.
pub fn repo_field(owner: &str, name: &str, admin: bool, picked: bool) -> RepoField {
    RepoField {
        owner: owner.into(),
        name: name.into(),
        editable: admin,
        accounts: Vec::new(),
        install_url: String::new(),
        page: if admin { "admin".into() } else { String::new() },
        picked,
        suggestions: Vec::new(),
        error: String::new(),
    }
}

#[derive(Deserialize, Default)]
pub struct RepoQuery {
    #[serde(default)]
    repo_owner: String,
    #[serde(default)]
    repo_name: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    page: String,
}

/// The repo field of a creator: their account and public organizations to choose from. When GitHub
/// cannot tell the organizations, their own account stays.
async fn creator_field(state: &AppState, token: &str, user: &Visitor, owner: &str, name: &str, picked: bool) -> RepoField {
    let mut field = repo_field(if owner.is_empty() { &user.login } else { owner }, name, false, picked);
    field.accounts = match state.api.get("/v1/me/accounts", Some(token)).await {
        Ok(list) => list.as_array().into_iter().flatten().map(s).collect(),
        Err(_) => vec![user.login.clone()],
    };
    field.install_url = state.install_url();
    field
}

/// The account to search: the chosen one, else the visitor's own; the API checks it is theirs.
fn account_of(user: &Visitor, wanted: &str) -> String {
    let wanted = wanted.trim();
    if wanted.is_empty() { user.login.clone() } else { wanted.to_string() }
}

/// `GET /addons/new/repos` (htmx): the public repos of the account matching the typed name.
pub async fn repo_suggestions(State(state): State<AppState>, jar: PrivateCookieJar, Query(q): Query<RepoQuery>) -> WebResult<Html<String>> {
    let (user, token) = me(&state, &jar).await?;
    let owner = account_of(&user, &q.repo_owner);
    let mut list = repo_field(&owner, "", q.page == "admin", false);
    if !owner.is_empty() {
        let query = reqwest::Url::parse_with_params("http://x/", &[("owner", owner.as_str()), ("q", q.repo_name.trim())])
            .map(|u| u.query().unwrap_or_default().to_string())
            .unwrap_or_default();
        match state.api.get(&format!("/v1/me/repos?{query}"), Some(&token)).await {
            Ok(answer) => list.suggestions = answer["repos"].as_array().into_iter().flatten().map(|r| (s(&r["full_name"]), s(&r["description"]))).collect(),
            Err(e) => list.error = e.message(),
        }
        if list.suggestions.is_empty() && list.error.is_empty() {
            list.error = format!("No public repo of {owner} matches.");
        }
    }
    Ok(render(&RepoSuggestionsFragment { repo: list }))
}

/// `GET /addons/new/repo?repo=owner/name` (htmx): the repo field with a picked suggestion.
pub async fn repo_pick(State(state): State<AppState>, jar: PrivateCookieJar, Query(q): Query<RepoQuery>) -> WebResult<Html<String>> {
    let (user, token) = me(&state, &jar).await?;
    let (owner, name) = q.repo.split_once('/').unwrap_or(("", q.repo.as_str()));
    let repo = if q.page == "admin" && user.admin {
        repo_field(owner, name, true, true)
    } else {
        creator_field(&state, &token, &user, owner, name, true).await
    };
    Ok(render(&RepoFieldFragment { repo }))
}

#[derive(Deserialize)]
pub struct NewAddonQuery {
    repo: Option<String>,
}

/// The form filled in from `repo`, as the API reads it from the newest release or tag.
pub(crate) async fn filled_in(state: &AppState, token: &str, repo: &str) -> (NewAddonForm, Result<String, String>) {
    let mut form = NewAddonForm { repo: repo.into(), source_type: "release".into(), ..Default::default() };
    let path = repo.split('/').map(seg).collect::<Vec<_>>().join("/");
    match state.api.get(&format!("/v1/me/repos/{path}/suggestion"), Some(token)).await {
        Ok(s) => {
            form.name = self::s(&s["name"]);
            form.slug = self::s(&s["slug"]);
            form.summary = self::s(&s["summary"]);
            form.source_type = self::s(&s["source_type"]);
            form.asset_pattern = self::s(&s["asset_pattern"]);
            form.folders = s["folders"].as_array().into_iter().flatten().map(self::s).collect::<Vec<_>>().join("\n");
            (form, Ok(self::s(&s["read_from"])))
        }
        Err(e) => (form, Err(e.message())),
    }
}

/// One path segment for the API.
fn seg(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub async fn new_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Query(query): Query<NewAddonQuery>,
) -> WebResult<Html<String>> {
    let (user, token) = me(&state, &jar).await?;
    let (form, filled) = match query.repo.as_deref().map(str::trim).filter(|r| r.split('/').count() == 2) {
        Some(repo) => {
            let (form, filled) = filled_in(&state, &token, repo).await;
            (form, Some(filled))
        }
        None => (NewAddonForm { source_type: "release".into(), ..Default::default() }, None),
    };
    let (owner, name) = form.repo.split_once('/').unwrap_or_default();
    let field = creator_field(&state, &token, &user, owner, name, false).await;
    Ok(render(&NewAddonPage {
        user: Some(user),
        csrf: csrf.0,
        install_url: state.install_url(),
        repo: field,
        filled,
        categories: categories(&state).await?,
        form,
        errors: Errors::default(),
    }))
}

/// The text fields of the publish form and its images as (kind, file name, bytes). Empty file
/// inputs send a part without a name; they are skipped.
async fn read_new_addon(mut multipart: Multipart) -> (NewAddonForm, Vec<(&'static str, String, Vec<u8>)>, bool) {
    let (mut fields, mut images, mut complete) = (serde_json::Map::new(), Vec::new(), true);
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(str::to_string);
        let Ok(bytes) = field.bytes().await else {
            complete = false;
            break;
        };
        match (name.as_str(), file_name) {
            ("icon" | "screenshots", Some(file)) if !file.is_empty() && !bytes.is_empty() => {
                images.push((if name == "icon" { "icon" } else { "screenshot" }, file, bytes.to_vec()));
            }
            (_, None) => {
                fields.insert(name, Value::String(String::from_utf8_lossy(&bytes).into_owned()));
            }
            _ => {}
        }
    }
    (serde_json::from_value(Value::Object(fields)).unwrap_or_default(), images, complete)
}

pub async fn create_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    multipart: Multipart,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let (mut form, images, complete) = read_new_addon(multipart).await;
    // The chosen account and the typed name; the API proves the account is the visitor's.
    if !form.repo_name.trim().is_empty() {
        form.repo = format!("{}/{}", account_of(&user, &form.repo_owner), form.repo_name.trim());
    }
    let body = json!({
        "repo": form.repo.trim(), "slug": form.slug.trim(), "name": form.name.trim(), "summary": form.summary.trim(),
        "description_md": form.description_md,
        "category": form.category, "source_type": form.source_type, "asset_pattern": form.asset_pattern.trim(),
        "folders": folders_of(&form.folders),
    });
    let result = if complete {
        state.api.post("/v1/addons", Some(&token), &body).await
    } else {
        Err(ApiError::Api { status: 413, code: "too_large".into(), message: "The images are too large together; each may be at most 5 MB.".into() })
    };
    match result {
        // The images go up once the addon exists; the images page shows what did not.
        Ok(created) => {
            let slug = s(&created["slug"]);
            let mut failed = Vec::new();
            for (kind, file, data) in images {
                let upload = reqwest::multipart::Form::new().text("kind", kind).part("file", reqwest::multipart::Part::bytes(data).file_name(file.clone()));
                if let Err(e) = state.api.post_multipart(&format!("/v1/addons/{slug}/images"), &token, upload).await {
                    failed.push(format!("{file}: {}", e.message()));
                }
            }
            let mut query = vec![("created", "1".to_string())];
            if !failed.is_empty() {
                query.push(("error", failed.join(" ")));
            }
            let url = reqwest::Url::parse_with_params("http://x/", &query).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
            Ok(Redirect::to(&format!("/addons/{slug}/images?{url}")).into_response())
        }
        Err(e) => {
            let mut errors = form_errors(e)?;
            if !images.is_empty() {
                let note = "Nothing was published. Choose the images again.";
                errors.0.entry("form".into()).and_modify(|m| *m = format!("{m} {note}")).or_insert_with(|| note.into());
            }
            let (owner, name) = form.repo.split_once('/').unwrap_or_default();
            let field = creator_field(&state, &token, &user, owner, name, false).await;
            let page = NewAddonPage {
                user: Some(user),
                csrf: csrf.0,
                install_url: state.install_url(),
                repo: field,
                filled: None,
                categories: categories(&state).await?,
                form,
                errors,
            };
            Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&page)).into_response())
        }
    }
}

#[derive(Deserialize, Default)]
pub struct EditForm {
    pub name: Option<String>,
    pub category: Option<String>,
    pub source_type: Option<String>,
    pub asset_pattern: Option<String>,
    pub folders: Option<String>,
    pub summary: Option<String>,
    pub description_md: Option<String>,
}

async fn edit_page(state: &AppState, user: Visitor, token: &str, csrf: String, slug: &str, errors: Errors, saved: bool) -> WebResult<Html<String>> {
    let p = perms(state, slug, token).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let description = s(&a["description_md"]);
    Ok(render(&EditAddonPage {
        user: Some(user),
        csrf,
        slug: slug.to_string(),
        name: s(&a["name"]),
        category: s(&a["category"]),
        categories: categories(state).await?,
        source_type: s(&a["source_type"]),
        asset_pattern: s(&a["asset_pattern"]),
        folders: a["folders"].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join("\n"),
        summary: s(&a["summary"]),
        description_html: forge_core::markdown::render(&description, &state.image_base()),
        description,
        perms: p,
        errors,
        saved,
    }))
}

pub async fn edit_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let p = perms(&state, &slug, &token).await?;
    if !p.owner && !p.edit_description && !p.edit_images {
        return Ok(forbidden());
    }
    if !p.owner && !p.edit_description {
        return Ok(Redirect::to(&format!("/addons/{slug}/images")).into_response());
    }
    Ok(edit_page(&state, user, &token, csrf.0, &slug, Errors::default(), false).await?.into_response())
}

pub async fn save_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<EditForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let mut body = serde_json::Map::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(v) = value {
            body.insert(key.into(), json!(v.trim()));
        }
    };
    put("name", &form.name);
    put("category", &form.category);
    put("source_type", &form.source_type);
    put("asset_pattern", &form.asset_pattern);
    put("summary", &form.summary);
    if let Some(d) = &form.description_md {
        body.insert("description_md".into(), json!(d));
    }
    if let Some(f) = &form.folders {
        body.insert("folders".into(), json!(folders_of(f)));
    }
    match state.api.call(Method::PATCH, &format!("/v1/addons/{slug}"), Some(&token), Some(&Value::Object(body))).await {
        Ok(_) => Ok(edit_page(&state, user, &token, csrf.0, &slug, Errors::default(), true).await?.into_response()),
        Err(ApiError::Api { status: 403, .. }) => Ok(forbidden()),
        Err(e) => {
            let errors = form_errors(e)?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, edit_page(&state, user, &token, csrf.0, &slug, errors, false).await?).into_response())
        }
    }
}

#[derive(Deserialize)]
pub struct PreviewForm {
    #[serde(default)]
    description_md: String,
}

/// The rendered description for the editor's preview; the same sanitizer as the API.
pub async fn preview(State(state): State<AppState>, jar: PrivateCookieJar, Form(form): Form<PreviewForm>) -> WebResult<Html<String>> {
    me(&state, &jar).await?;
    Ok(Html(forge_core::markdown::render(&form.description_md, &state.image_base())))
}

async fn images_page(
    state: &AppState,
    user: Visitor,
    token: &str,
    csrf: String,
    slug: &str,
    tab: &str,
    (message, error): (Option<String>, Option<String>),
) -> WebResult<Html<String>> {
    let perms = perms(state, slug, token).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let images = a["images"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| crate::templates::ImageRow { id: i["id"].as_i64().unwrap_or(0), url: s(&i["url"]), caption: s(&i["caption"]) })
        .collect();
    let tab = if tab == SCREENSHOTS { SCREENSHOTS } else { ICON }.to_string();
    Ok(render(&ImagesPage { user: Some(user), csrf, slug: slug.into(), name: s(&a["name"]), tab, icon_url: a["icon_url"].as_str().map(str::to_string), images, message, error, perms }))
}

/// The tabs of the images page; an upload stays on the tab of its kind.
const ICON: &str = "icon";
const SCREENSHOTS: &str = "screenshots";

fn tab_of(kind: &str) -> &'static str {
    if kind == "screenshot" { SCREENSHOTS } else { ICON }
}

#[derive(Deserialize)]
pub struct ImagesQuery {
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    tab: String,
    /// Images of the publish form that did not go up.
    #[serde(default)]
    error: Option<String>,
}

pub async fn images(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(query): Query<ImagesQuery>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    if !perms(&state, &slug, &token).await?.edit_images {
        return Ok(forbidden());
    }
    let message = query.created.map(|_| "Your addon is published. Add or change screenshots and the icon here, now or later under Edit.".to_string());
    Ok(images_page(&state, user, &token, csrf.0, &slug, &query.tab, (message, query.error)).await?.into_response())
}

pub async fn upload_image(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    mut multipart: Multipart,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let (mut files, mut kind, mut caption) = (Vec::new(), String::new(), String::new());
    // The square picked for an icon, in pixels of the picked image; empty without script.
    let mut crop: Vec<(String, String)> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().unwrap_or("upload").to_string();
        let Ok(bytes) = field.bytes().await else {
            return Ok(images_page(&state, user, &token, csrf.0, &slug, tab_of(&kind), (None, Some("Images may be at most 5 MB each.".into()))).await?.into_response());
        };
        match name.as_str() {
            "file" if !bytes.is_empty() => files.push((file_name, bytes.to_vec())),
            "kind" => kind = String::from_utf8_lossy(&bytes).into_owned(),
            "caption" => caption = String::from_utf8_lossy(&bytes).into_owned(),
            "crop_x" | "crop_y" | "crop_size" if !bytes.is_empty() => crop.push((name, String::from_utf8_lossy(&bytes).into_owned())),
            _ => {}
        }
    }
    // There is one icon: of several picked files only the first becomes it.
    if kind == "icon" {
        files.truncate(1);
    }
    // One API upload per file, so one bad file does not stop the others.
    let (mut uploaded, mut notes, mut errors) = (0, Vec::new(), Vec::new());
    let several = files.len() > 1;
    for (file_name, data) in files {
        let mut form = reqwest::multipart::Form::new().text("kind", kind.clone()).text("caption", caption.clone());
        if kind == "icon" && crop.len() == 3 {
            for (name, value) in &crop {
                form = form.text(name.clone(), value.clone());
            }
        }
        let form = form.part("file", reqwest::multipart::Part::bytes(data).file_name(file_name.clone()));
        let named = |text: String| if several { format!("{file_name}: {text}") } else { text };
        match state.api.post_multipart(&format!("/v1/addons/{slug}/images"), &token, form).await {
            Ok(answer) => {
                uploaded += 1;
                if answer["notice"] == "resized" {
                    notes.push(named(format!(
                        "Image was scaled down from {} x {} to {} x {} px.",
                        answer["original"]["width"], answer["original"]["height"], answer["stored"]["width"], answer["stored"]["height"]
                    )));
                }
            }
            Err(ApiError::Api { message, .. }) => errors.push(named(message)),
            Err(e) => return Err(e.into()),
        }
    }
    let message = match uploaded {
        0 => None,
        1 => Some(std::iter::once("Image uploaded.".to_string()).chain(notes).collect::<Vec<_>>().join(" ")),
        n => Some(std::iter::once(format!("{n} images uploaded.")).chain(notes).collect::<Vec<_>>().join(" ")),
    };
    let error = (!errors.is_empty()).then(|| errors.join(" "));
    Ok(images_page(&state, user, &token, csrf.0, &slug, tab_of(&kind), (message, error)).await?.into_response())
}

#[derive(Deserialize)]
pub struct MoveForm {
    direction: String,
}

/// Swaps an image with its neighbour; positions are the order the API returns them in.
pub async fn move_image(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((slug, id)): Path<(String, i64)>,
    Form(form): Form<MoveForm>,
) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let ids: Vec<i64> = a["images"].as_array().into_iter().flatten().filter_map(|i| i["id"].as_i64()).collect();
    if let Some(at) = ids.iter().position(|i| *i == id) {
        let other = if form.direction == "up" { at.checked_sub(1) } else { Some(at + 1).filter(|o| *o < ids.len()) };
        if let Some(other) = other {
            let mut order = ids.clone();
            order.swap(at, other);
            save_order(&state, &token, &slug, &order).await?;
        }
    }
    // Back at the moved screenshot instead of the top of the page.
    Ok(Redirect::to(&format!("/addons/{slug}/images?tab=screenshots#shot-{id}")).into_response())
}

async fn save_order(state: &AppState, token: &str, slug: &str, order: &[i64]) -> WebResult<()> {
    for (position, image) in order.iter().enumerate() {
        state.api.call(Method::PATCH, &format!("/v1/addons/{slug}/images/{image}"), Some(token), Some(&json!({ "position": position }))).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct OrderForm {
    /// Image ids, comma-separated, in the new order.
    #[serde(default)]
    order: String,
}

/// The order after dragging: the given ids that belong to the addon, then any it left out.
pub async fn order_images(State(state): State<AppState>, jar: PrivateCookieJar, Path(slug): Path<String>, Form(form): Form<OrderForm>) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let ids: Vec<i64> = a["images"].as_array().into_iter().flatten().filter_map(|i| i["id"].as_i64()).collect();
    let mut order: Vec<i64> = form.order.split(',').filter_map(|i| i.trim().parse().ok()).filter(|i| ids.contains(i)).collect();
    order.dedup();
    order.extend(ids.iter().filter(|i| !order.contains(i)).collect::<Vec<_>>());
    save_order(&state, &token, &slug, &order).await?;
    Ok(Redirect::to(&format!("/addons/{slug}/images?tab=screenshots")).into_response())
}

#[derive(Deserialize)]
pub struct CaptionForm {
    #[serde(default)]
    caption: String,
}

/// Changes the caption of an uploaded screenshot.
pub async fn caption_image(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, id)): Path<(String, i64)>,
    Form(form): Form<CaptionForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let body = json!({ "caption": form.caption });
    match state.api.call(Method::PATCH, &format!("/v1/addons/{slug}/images/{id}"), Some(&token), Some(&body)).await {
        // Back at the screenshot instead of the top of the page.
        Ok(_) => Ok(Redirect::to(&format!("/addons/{slug}/images?tab=screenshots#shot-{id}")).into_response()),
        Err(ApiError::Api { message, status, .. }) if status != 403 && status != 404 => {
            let page = images_page(&state, user, &token, csrf.0, &slug, SCREENSHOTS, (None, Some(message))).await?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, page).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// Removes the icon; the addon shows its initial again.
pub async fn delete_icon(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(slug): Path<String>) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let message = match state.api.call(Method::DELETE, &format!("/v1/addons/{slug}/icon"), Some(&token), None).await {
        Ok(_) | Err(ApiError::Api { status: 404, .. }) => "Icon deleted.".to_string(),
        Err(e) => return Err(e.into()),
    };
    Ok(images_page(&state, user, &token, csrf.0, &slug, ICON, (Some(message), None)).await?.into_response())
}

pub async fn delete_image(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug, id)): Path<(String, i64)>) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    state.api.call(Method::DELETE, &format!("/v1/addons/{slug}/images/{id}"), Some(&token), None).await?;
    Ok(Redirect::to(&format!("/addons/{slug}/images?tab=screenshots")).into_response())
}

async fn members_page(state: &AppState, user: Visitor, token: &str, csrf: String, slug: &str, error: Option<String>) -> WebResult<Html<String>> {
    let list = state.api.get(&format!("/v1/addons/{slug}/members"), Some(token)).await?;
    let members = list
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| crate::templates::MemberRow {
            github_id: m["github_id"].as_i64().unwrap_or(0),
            login: s(&m["login"]),
            description: m["can_edit_description"] == true,
            images: m["can_edit_images"] == true,
        })
        .collect();
    let perms = perms(state, slug, token).await?;
    Ok(render(&MembersPage { user: Some(user), csrf, slug: slug.into(), members, error, perms }))
}

pub async fn members(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(slug): Path<String>) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    if !perms(&state, &slug, &token).await?.owner {
        return Ok(forbidden());
    }
    Ok(members_page(&state, user, &token, csrf.0, &slug, None).await?.into_response())
}

#[derive(Deserialize)]
pub struct MemberForm {
    #[serde(default)]
    login: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    images: Option<String>,
}

pub async fn add_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<MemberForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let body = json!({ "login": form.login.trim(), "can_edit_description": form.description.is_some(), "can_edit_images": form.images.is_some() });
    match state.api.post(&format!("/v1/addons/{slug}/members"), Some(&token), &body).await {
        Ok(_) => Ok(Redirect::to(&format!("/addons/{slug}/members")).into_response()),
        Err(ApiError::Api { message, status, .. }) if status != 403 => Ok(members_page(&state, user, &token, csrf.0, &slug, Some(message)).await?.into_response()),
        Err(e) => Err(e.into()),
    }
}

pub async fn update_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((slug, github_id)): Path<(String, i64)>,
    Form(form): Form<MemberForm>,
) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    let body = json!({ "can_edit_description": form.description.is_some(), "can_edit_images": form.images.is_some() });
    state.api.call(Method::PATCH, &format!("/v1/addons/{slug}/members/{github_id}"), Some(&token), Some(&body)).await?;
    Ok(Redirect::to(&format!("/addons/{slug}/members")).into_response())
}

pub async fn remove_member(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug, github_id)): Path<(String, i64)>) -> WebResult<Response> {
    let (_, token) = me(&state, &jar).await?;
    state.api.call(Method::DELETE, &format!("/v1/addons/{slug}/members/{github_id}"), Some(&token), None).await?;
    Ok(Redirect::to(&format!("/addons/{slug}/members")).into_response())
}

fn current_deps(addon: &Value) -> Vec<(String, String, bool)> {
    addon["dependencies"].as_array().into_iter().flatten().map(|d| (s(&d["slug"]), s(&d["name"]), d["required"] == true)).collect()
}

async fn deps_page(state: &AppState, user: Visitor, token: &str, csrf: String, slug: &str, error: Option<String>) -> WebResult<Html<String>> {
    let perms = perms(state, slug, token).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    Ok(render(&DepsPage { user: Some(user), csrf, slug: slug.into(), deps: current_deps(&a), error, perms }))
}

pub async fn deps(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(slug): Path<String>) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    if !perms(&state, &slug, &token).await?.owner {
        return Ok(forbidden());
    }
    Ok(deps_page(&state, user, &token, csrf.0, &slug, None).await?.into_response())
}

#[derive(Deserialize)]
pub struct DepForm {
    #[serde(default)]
    dependency: String,
    #[serde(default)]
    required: Option<String>,
    #[serde(default)]
    remove: Option<String>,
}

pub async fn save_deps(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<DepForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let a = state.api.get(&format!("/v1/addons/{slug}"), None).await?;
    let mut list: Vec<Value> = current_deps(&a)
        .into_iter()
        .filter(|(d, _, _)| Some(d) != form.remove.as_ref())
        .map(|(d, _, required)| json!({ "slug": d, "required": required }))
        .collect();
    if !form.dependency.trim().is_empty() {
        list.push(json!({ "slug": form.dependency.trim(), "required": form.required.is_some() }));
    }
    match state.api.call(Method::PUT, &format!("/v1/addons/{slug}/dependencies"), Some(&token), Some(&Value::Array(list))).await {
        Ok(_) => Ok(Redirect::to(&format!("/addons/{slug}/dependencies")).into_response()),
        Err(ApiError::Api { message, status, .. }) if status != 403 => Ok(deps_page(&state, user, &token, csrf.0, &slug, Some(message)).await?.into_response()),
        Err(e) => Err(e.into()),
    }
}

pub async fn version_edit(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, tag)): Path<(String, String)>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let perms = perms(&state, &slug, &token).await?;
    if !perms.owner {
        return Ok(forbidden());
    }
    let versions = state.api.get(&format!("/v1/addons/{slug}/versions"), None).await?;
    let v = versions.as_array().into_iter().flatten().find(|v| v["tag"] == tag.as_str()).ok_or(WebError::NotFound)?.clone();
    let games: Vec<String> = state.api.get("/v1/game-versions", None).await?.as_array().into_iter().flatten().map(|g| s(&g["name"])).collect();
    Ok(render(&VersionEditPage { user: Some(user), csrf: csrf.0, slug, tag, game_min: s(&v["game_min"]), game_max: s(&v["game_max"]), games, error: None, perms })
        .into_response())
}

#[derive(Deserialize)]
pub struct RangeForm {
    #[serde(default)]
    game_min: String,
    #[serde(default)]
    game_max: String,
}

pub async fn save_version(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, tag)): Path<(String, String)>,
    Form(form): Form<RangeForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let opt = |v: &str| if v.is_empty() { Value::Null } else { json!(v) };
    let body = json!({ "game_min": opt(&form.game_min), "game_max": opt(&form.game_max) });
    match state.api.call(Method::PATCH, &format!("/v1/addons/{slug}/versions/{tag}"), Some(&token), Some(&body)).await {
        Ok(_) => Ok(Redirect::to(&format!("/addons/{slug}/versions")).into_response()),
        Err(ApiError::Api { message, status, .. }) if status != 403 && status != 404 => {
            let games: Vec<String> = state.api.get("/v1/game-versions", None).await?.as_array().into_iter().flatten().map(|g| s(&g["name"])).collect();
            let perms = perms(&state, &slug, &token).await?;
            let page = VersionEditPage { user: Some(user), csrf: csrf.0, slug, tag, game_min: form.game_min, game_max: form.game_max, games, error: Some(message), perms };
            Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&page)).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn delete_page(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(slug): Path<String>) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    let perms = perms(&state, &slug, &token).await?;
    if !perms.owner {
        return Ok(forbidden());
    }
    Ok(render(&DeleteAddonPage { user: Some(user), csrf: csrf.0, slug, error: None, perms }).into_response())
}

#[derive(Deserialize)]
pub struct DeleteForm {
    #[serde(default)]
    confirm: String,
}

pub async fn delete_addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<DeleteForm>,
) -> WebResult<Response> {
    let (user, token) = me(&state, &jar).await?;
    if form.confirm.trim() != slug {
        let perms = perms(&state, &slug, &token).await?;
        let page = DeleteAddonPage { user: Some(user), csrf: csrf.0, slug, error: Some("Type the slug exactly to confirm.".into()), perms };
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&page)).into_response());
    }
    state.api.call(Method::DELETE, &format!("/v1/addons/{slug}"), Some(&token), None).await?;
    Ok(Redirect::to("/me/addons").into_response())
}
