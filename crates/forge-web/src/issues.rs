//! Issues on the website: an addon's (`/addons/{slug}/issues`, the addon page's Issues tab) and
//! SurvivorsForge's own (`/issues`). They are the GitHub issues of the repo, read through the API
//! and written as the visitor with the GitHub token from their login (`session::GITHUB_COOKIE`).

use crate::api_client::ApiError;
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::public::seg;
use crate::session;
use crate::templates::{Pager, Visitor, render};
use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use chrono::{DateTime, Duration, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Must match the API's page size.
pub const PER_PAGE: i64 = 20;

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// `2026-11-01T09:30:00Z` -> `2026-11-01`.
fn day(v: &Value) -> String {
    s(v).chars().take(10).collect()
}

/// Whose issues a page shows: an addon's or the platform's.
pub struct Place {
    /// The addon's slug; empty for the platform.
    pub slug: String,
    pub name: String,
    /// Where the pages live: `/addons/{slug}`, or nothing for the platform.
    pub base: String,
    /// The API's path before `/issues`.
    pub api: String,
    /// The addon's owner turned the Issues tab on; always for the platform.
    pub enabled: bool,
}

impl Place {
    pub fn addon(&self) -> bool {
        !self.slug.is_empty()
    }
}

/// `slug` in the path: that addon; none: SurvivorsForge itself.
async fn place(state: &AppState, path: &HashMap<String, String>) -> WebResult<Place> {
    Ok(match path.get("slug") {
        Some(slug) => {
            let a = state.api.get(&format!("/v1/addons/{}", seg(slug)), None).await?;
            let enabled = a["issues_enabled"] == true;
            Place { slug: slug.clone(), name: s(&a["name"]), base: format!("/addons/{slug}"), api: format!("/v1/addons/{}", seg(slug)), enabled }
        }
        None => Place { slug: String::new(), name: "SurvivorsForge".into(), base: String::new(), api: "/v1/platform".into(), enabled: true },
    })
}

/// A number from the path; anything else is not a page here.
fn number(path: &HashMap<String, String>, key: &str) -> WebResult<i64> {
    path.get(key).and_then(|n| n.parse().ok()).ok_or(WebError::Api(ApiError::Api { status: 404, code: "not_found".into(), message: "Not found".into() }))
}

/// What the visitor can do here, in the order the page checks it.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// The addon's owner did not turn the Issues tab on: only GitHub's own form.
    Off,
    /// The repo has issues switched off.
    Disabled,
    /// The App may not write issues there: GitHub's own form.
    OnGitHub,
    Login,
    /// Logged in before issues came, or the GitHub token ran out for good.
    Relogin,
    Ok,
}

pub struct Frame {
    pub repo: String,
    pub issues_url: String,
    pub new_issue_url: String,
    pub write: Write,
}

impl Frame {
    fn new(answer: &Value, user: Option<&Visitor>, has_github: bool) -> Frame {
        let write = if answer["issues_here"] == false {
            Write::Off
        } else if answer["issues_enabled"] == false {
            Write::Disabled
        } else if answer["can_write"] != true {
            Write::OnGitHub
        } else if user.is_none() {
            Write::Login
        } else if !has_github {
            Write::Relogin
        } else {
            Write::Ok
        };
        Frame { repo: s(&answer["repo"]), issues_url: s(&answer["issues_url"]), new_issue_url: s(&answer["new_issue_url"]), write }
    }
}

pub struct Person {
    pub login: String,
    pub avatar_url: String,
}

impl Person {
    fn from(v: &Value) -> Person {
        Person { login: s(&v["login"]), avatar_url: s(&v["avatar_url"]) }
    }
}

/// "Bug report", "Feature request", "Question" or nothing.
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "bug" => "Bug report",
        "feature" => "Feature request",
        "question" => "Question",
        _ => "",
    }
}

/// The badge of a status: (class, text).
pub fn status_badge(status: &str) -> (&'static str, &'static str) {
    match status {
        "open" => ("ok", "Open"),
        "in_progress" => ("warn", "In progress"),
        "not_planned" => ("", "Not planned"),
        _ => ("", "Closed"),
    }
}

pub struct IssueRow {
    pub number: i64,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub author: Person,
    pub comments: i64,
    pub updated: String,
}

impl IssueRow {
    pub fn kind_label(&self) -> &'static str {
        kind_label(&self.kind)
    }

    pub fn badge(&self) -> (&'static str, &'static str) {
        status_badge(&self.status)
    }

    fn from(v: &Value) -> IssueRow {
        IssueRow {
            number: v["number"].as_i64().unwrap_or(0),
            title: s(&v["title"]),
            kind: s(&v["kind"]),
            status: s(&v["status"]),
            author: Person::from(&v["author"]),
            comments: v["comments"].as_i64().unwrap_or(0),
            updated: day(&v["updated_at"]),
        }
    }
}

#[derive(Template)]
#[template(path = "issues.html")]
pub struct IssuesPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub place: Place,
    pub frame: Frame,
    pub state: String,
    pub q: String,
    /// open, closed, all
    pub counts: (i64, i64, i64),
    pub matching: i64,
    pub rows: Vec<IssueRow>,
    pub pager: Option<Pager>,
    /// GitHub listed 100: there may be older ones only GitHub shows.
    pub truncated: bool,
}

pub struct Reply {
    pub id: i64,
    pub author: Person,
    pub created: String,
    pub body: String,
    /// Sanitized by the API (forge_core::markdown).
    pub body_html: String,
    pub edited: bool,
    pub html_url: String,
    pub mine: bool,
}

pub struct IssueView {
    pub number: i64,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub open: bool,
    /// Locked on GitHub: only the repo's team replies.
    pub locked: bool,
    pub author: Person,
    pub created: String,
    pub body: String,
    /// Sanitized by the API (forge_core::markdown).
    pub body_html: String,
    pub edited: bool,
    pub html_url: String,
    pub mine: bool,
}

impl IssueView {
    pub fn kind_label(&self) -> &'static str {
        kind_label(&self.kind)
    }

    pub fn badge(&self) -> (&'static str, &'static str) {
        status_badge(&self.status)
    }
}

#[derive(Template)]
#[template(path = "issue.html")]
pub struct IssuePage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub place: Place,
    pub frame: Frame,
    pub issue: IssueView,
    pub replies: Vec<Reply>,
    /// The reply typed before a refusal, so it is not lost.
    pub reply: String,
    pub error: Option<String>,
    pub done: Option<String>,
}

#[derive(Template)]
#[template(path = "issue_new.html")]
pub struct NewIssuePage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub place: Place,
    pub frame: Frame,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub error: Option<String>,
}

/// The visitor's GitHub access token, renewed through the API when it runs out within five
/// minutes. A token that cannot be renewed leaves the jar.
async fn github_access(state: &AppState, jar: PrivateCookieJar, session_token: &str) -> (Option<String>, PrivateCookieJar) {
    let Some(github) = session::github_token(&jar) else { return (None, jar) };
    let expires = github["expires_at"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc));
    if expires.is_none_or(|e| e - Duration::minutes(5) > Utc::now()) {
        return (Some(s(&github["access_token"])), jar);
    }
    let body = json!({ "refresh_token": s(&github["refresh_token"]) });
    match state.api.post("/v1/auth/github/refresh", Some(session_token), &body).await {
        Ok(renewed) => {
            let access = s(&renewed["access_token"]);
            (Some(access), jar.add(session::cookie(session::GITHUB_COOKIE, renewed.to_string(), state.secure_cookies())))
        }
        Err(_) => (None, jar.remove(session::removal(session::GITHUB_COOKIE))),
    }
}

/// Query string of `pairs`, encoded.
pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    reqwest::Url::parse_with_params("http://x/", pairs).expect("valid").query().unwrap_or_default().to_string()
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    state: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: Option<i64>,
}

/// `GET /addons/{slug}/issues` and `GET /issues`.
pub async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(path): Path<HashMap<String, String>>,
    Query(q): Query<ListQuery>,
) -> WebResult<Response> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let place = place(&state, &path).await?;
    let filter = match q.state.as_str() {
        "closed" | "all" => q.state.clone(),
        _ => "open".to_string(),
    };
    let page = q.page.unwrap_or(1).to_string();
    let answer = state.api.get(&format!("{}/issues?{}", place.api, query(&[("state", &filter), ("q", &q.q), ("page", &page)])), None).await?;
    let count = |k: &str| answer["counts"][k].as_i64().unwrap_or(0);
    let matching = answer["matching"].as_i64().unwrap_or(0);
    let base = place.base.clone();
    let link = |n: i64| format!("{base}/issues?{}", query(&[("state", &filter), ("q", &q.q), ("page", &n.to_string())]));
    let page = IssuesPage {
        frame: Frame::new(&answer, user.as_ref(), session::github_token(&jar).is_some()),
        csrf: csrf.0,
        state: filter.clone(),
        q: q.q.clone(),
        counts: (count("open"), count("closed"), count("all")),
        matching,
        rows: answer["issues"].as_array().into_iter().flatten().map(IssueRow::from).collect(),
        pager: Pager::new(answer["page"].as_i64().unwrap_or(1), PER_PAGE, matching, "issues", link),
        truncated: answer["truncated"] == true,
        place,
        user,
    };
    Ok(render(&page).into_response())
}

async fn issue_page(state: &AppState, jar: &PrivateCookieJar, csrf: String, user: Option<Visitor>, place: Place, number: i64) -> WebResult<IssuePage> {
    let answer = state.api.get(&format!("{}/issues/{number}", place.api), None).await?;
    let mine = |author: &Value| user.as_ref().is_some_and(|u| u.login.eq_ignore_ascii_case(author["login"].as_str().unwrap_or_default()));
    let i = &answer["issue"];
    let issue = IssueView {
        number,
        title: s(&i["title"]),
        kind: s(&i["kind"]),
        status: s(&i["status"]),
        open: i["state"] == "open",
        locked: i["locked"] == true,
        author: Person::from(&i["author"]),
        created: day(&i["created_at"]),
        body: s(&i["body"]),
        body_html: s(&i["body_html"]),
        edited: i["edited"] == true,
        html_url: s(&i["html_url"]),
        mine: mine(&i["author"]),
    };
    let replies = answer["replies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Reply {
            id: r["id"].as_i64().unwrap_or(0),
            author: Person::from(&r["author"]),
            created: day(&r["created_at"]),
            body: s(&r["body"]),
            body_html: s(&r["body_html"]),
            edited: r["edited"] == true,
            html_url: s(&r["html_url"]),
            mine: mine(&r["author"]),
        })
        .collect();
    Ok(IssuePage {
        frame: Frame::new(&answer, user.as_ref(), session::github_token(jar).is_some()),
        user,
        csrf,
        place,
        issue,
        replies,
        reply: String::new(),
        error: None,
        done: None,
    })
}

#[derive(Deserialize, Default)]
pub struct Flash {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    done: Option<String>,
}

/// `GET .../issues/{number}`; `error`/`done` say how the last action went.
pub async fn show(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(path): Path<HashMap<String, String>>,
    Query(flash): Query<Flash>,
) -> WebResult<Response> {
    let number = number(&path, "number")?;
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let place = place(&state, &path).await?;
    if !place.enabled {
        return Ok(Redirect::to(&format!("{}/issues", place.base)).into_response());
    }
    let mut page = issue_page(&state, &jar, csrf.0, user, place, number).await?;
    page.error = flash.error.filter(|e| !e.is_empty());
    page.done = flash.done.filter(|d| !d.is_empty());
    Ok(render(&page).into_response())
}

/// What a refused write tells the visitor; a dead GitHub token also leaves the jar.
fn refused(e: &ApiError, jar: PrivateCookieJar) -> (String, PrivateCookieJar) {
    match e.code() {
        "github_reauth" => ("Log in again to post on GitHub.".into(), jar.remove(session::removal(session::GITHUB_COOKIE))),
        _ => (e.message(), jar),
    }
}

fn issue_url(base: &str, number: i64, key: &str, message: &str) -> String {
    format!("{base}/issues/{number}?{}", query(&[(key, message)]))
}

fn no_github_token() -> ApiError {
    ApiError::Api { status: 403, code: "github_reauth".into(), message: String::new() }
}

#[derive(Deserialize, Default)]
pub struct NewIssueForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn new_page(state: &AppState, jar: &PrivateCookieJar, csrf: String, user: Visitor, place: Place, form: NewIssueForm, error: Option<String>) -> WebResult<NewIssuePage> {
    let answer = state.api.get(&format!("{}/issues?state=open", place.api), None).await?;
    Ok(NewIssuePage {
        frame: Frame::new(&answer, Some(&user), session::github_token(jar).is_some()),
        user: Some(user),
        csrf,
        place,
        kind: if form.kind.is_empty() { "bug".into() } else { form.kind },
        title: form.title,
        body: form.body,
        error,
    })
}

#[derive(Deserialize, Default)]
pub struct NewQuery {
    #[serde(default)]
    kind: String,
}

/// `GET .../issues/new`, behind every "Report a bug": the form here when the App is installed on
/// the repo with issue rights, else GitHub's own form, which needs no login here.
pub async fn new_form(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(path): Path<HashMap<String, String>>,
    Query(q): Query<NewQuery>,
) -> WebResult<Response> {
    let place = place(&state, &path).await?;
    let answer = state.api.get(&format!("{}/issues?state=open", place.api), None).await?;
    if answer["issues_here"] == false || (answer["issues_enabled"] != false && answer["can_write"] != true) {
        return Ok(Redirect::to(&s(&answer["new_issue_url"])).into_response());
    }
    let (user, _) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let form = NewIssueForm { kind: q.kind, ..NewIssueForm::default() };
    Ok(render(&new_page(&state, &jar, csrf.0, user, place, form, None).await?).into_response())
}

/// `POST .../issues`: opens the issue on GitHub as the visitor; a refused one shows the form again
/// with everything typed.
pub async fn create(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(path): Path<HashMap<String, String>>,
    Form(form): Form<NewIssueForm>,
) -> WebResult<Response> {
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let place = place(&state, &path).await?;
    let (github, jar) = github_access(&state, jar, &token).await;
    let body = json!({ "kind": form.kind, "title": form.title, "body": form.body });
    let result = match &github {
        Some(github) => state.api.call_as(Method::POST, &format!("{}/issues", place.api), Some(&token), Some(github), Some(&body)).await,
        None => Err(no_github_token()),
    };
    match result {
        Ok(created) => {
            let number = created["number"].as_i64().unwrap_or(0);
            Ok((jar, Redirect::to(&issue_url(&place.base, number, "done", "Your issue is on GitHub now."))).into_response())
        }
        Err(ApiError::Unauthorized) => Err(WebError::LoginRequired),
        Err(e) => {
            let (message, jar) = refused(&e, jar);
            let page = new_page(&state, &jar, csrf.0, user, place, form, Some(message)).await?;
            Ok((jar, render(&page)).into_response())
        }
    }
}

/// One write of the API below `.../issues/{number}`: method, path after it and body.
type Call = (Method, String, Option<Value>);

/// Runs one write as the visitor and goes back to the issue with how it went.
async fn write_and_return(state: &AppState, jar: PrivateCookieJar, path: &HashMap<String, String>, (method, rest, body): Call, done: &str) -> WebResult<Response> {
    let number = number(path, "number")?;
    let (_, token) = visitor(state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let place = place(state, path).await?;
    let (github, jar) = github_access(state, jar, &token).await;
    let Some(github) = github else {
        let jar = jar.remove(session::removal(session::GITHUB_COOKIE));
        return Ok((jar, Redirect::to(&issue_url(&place.base, number, "error", "Log in again to post on GitHub."))).into_response());
    };
    let api_path = format!("{}/issues/{number}{rest}", place.api);
    match state.api.call_as(method, &api_path, Some(&token), Some(&github), body.as_ref()).await {
        Ok(_) => Ok((jar, Redirect::to(&issue_url(&place.base, number, "done", done))).into_response()),
        Err(ApiError::Unauthorized) => Err(WebError::LoginRequired),
        Err(e) => {
            let (message, jar) = refused(&e, jar);
            Ok((jar, Redirect::to(&issue_url(&place.base, number, "error", &message))).into_response())
        }
    }
}

#[derive(Deserialize, Default)]
pub struct TextForm {
    #[serde(default)]
    body: String,
}

/// `POST .../issues/{number}/reply`: a refused reply stays on the page.
pub async fn reply(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(path): Path<HashMap<String, String>>,
    Form(form): Form<TextForm>,
) -> WebResult<Response> {
    let number = number(&path, "number")?;
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let place = place(&state, &path).await?;
    let (github, jar) = github_access(&state, jar, &token).await;
    let api_path = format!("{}/issues/{number}/comments", place.api);
    let result = match &github {
        Some(github) => state.api.call_as(Method::POST, &api_path, Some(&token), Some(github), Some(&json!({ "body": form.body }))).await,
        None => Err(no_github_token()),
    };
    match result {
        Ok(_) => Ok((jar, Redirect::to(&issue_url(&place.base, number, "done", "Your reply is on GitHub now."))).into_response()),
        Err(ApiError::Unauthorized) => Err(WebError::LoginRequired),
        Err(e) => {
            let (message, jar) = refused(&e, jar);
            let mut page = issue_page(&state, &jar, csrf.0, Some(user), place, number).await?;
            page.reply = form.body;
            page.error = Some(message);
            Ok((jar, render(&page)).into_response())
        }
    }
}

#[derive(Deserialize, Default)]
pub struct IssueForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

/// `POST .../issues/{number}/edit`: the author's title and text.
pub async fn edit(State(state): State<AppState>, jar: PrivateCookieJar, Path(path): Path<HashMap<String, String>>, Form(form): Form<IssueForm>) -> WebResult<Response> {
    let call = (Method::PATCH, String::new(), Some(json!({ "title": form.title, "body": form.body })));
    write_and_return(&state, jar, &path, call, "Saved on GitHub.").await
}

#[derive(Deserialize, Default)]
pub struct StateForm {
    #[serde(default)]
    state: String,
}

/// `POST .../issues/{number}/state`: close or reopen.
pub async fn set_state(State(state): State<AppState>, jar: PrivateCookieJar, Path(path): Path<HashMap<String, String>>, Form(form): Form<StateForm>) -> WebResult<Response> {
    let done = if form.state == "closed" { "Issue closed." } else { "Issue reopened." };
    write_and_return(&state, jar, &path, (Method::PATCH, String::new(), Some(json!({ "state": form.state }))), done).await
}

/// `POST .../issues/{number}/replies/{id}/edit`.
pub async fn edit_reply(State(state): State<AppState>, jar: PrivateCookieJar, Path(path): Path<HashMap<String, String>>, Form(form): Form<TextForm>) -> WebResult<Response> {
    let id = number(&path, "id")?;
    write_and_return(&state, jar, &path, (Method::PATCH, format!("/comments/{id}"), Some(json!({ "body": form.body }))), "Reply saved on GitHub.").await
}

/// `POST .../issues/{number}/replies/{id}/delete`.
pub async fn delete_reply(State(state): State<AppState>, jar: PrivateCookieJar, Path(path): Path<HashMap<String, String>>) -> WebResult<Response> {
    let id = number(&path, "id")?;
    write_and_return(&state, jar, &path, (Method::DELETE, format!("/comments/{id}"), None), "Reply deleted on GitHub.").await
}
