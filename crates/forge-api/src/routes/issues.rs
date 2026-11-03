//! The issues of an addon's GitHub repo, read live from GitHub and written as the logged-in user.
//! Nothing is stored here: an issue opened on GitHub shows at once (the ETag cache asks GitHub on
//! every read, and its `304` costs no request of the limit).
//!
//! Writing needs the App installed on the repo with `Issues: Read & write`, which every owner of an
//! installation confirms on GitHub. Without it the answer names GitHub's own form (`new_issue_url`).
//!
//! `/v1/addons/{slug}/issues[/{number}[/comments[/{id}]]]` for an addon's repo, and the same under
//! `/v1/platform/issues` for SurvivorsForge's own repo (`LAUNCHER_REPO`).

use super::addons::{AddonRow, github_error, load_addon, not_blocked};
use crate::app::AppState;
use crate::auth::GitHubUser;
use crate::error::{ApiError, ApiResult};
use crate::github::GitHubError;
use crate::github::types::{Issue, IssueComment, User};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

pub const PER_PAGE: usize = 20;
pub const MAX_TITLE: usize = 256;
pub const MAX_BODY: usize = 20_000;
pub const ISSUES_PER_HOUR: usize = 5;
pub const REPLIES_PER_HOUR: usize = 30;

/// The label of each kind; GitHub's default labels.
fn label(kind: &str) -> Option<&'static str> {
    match kind {
        "bug" => Some("bug"),
        "feature" => Some("enhancement"),
        "question" => Some("question"),
        _ => None,
    }
}

/// `bug`, `feature`, `question` or `other`, from the labels.
pub fn kind(issue: &Issue) -> &'static str {
    let has = |name: &str| issue.labels.iter().any(|l| l.name.eq_ignore_ascii_case(name));
    if has("bug") {
        "bug"
    } else if has("enhancement") || has("feature") {
        "feature"
    } else if has("question") {
        "question"
    } else {
        "other"
    }
}

/// `open`, `in_progress` (label `in progress`), `closed` or `not_planned`.
pub fn status(issue: &Issue) -> &'static str {
    let in_progress = issue.labels.iter().any(|l| l.name.eq_ignore_ascii_case("in progress"));
    match (issue.state.as_str(), issue.state_reason.as_deref()) {
        ("open", _) if in_progress => "in_progress",
        ("open", _) => "open",
        (_, Some("not_planned")) => "not_planned",
        _ => "closed",
    }
}

fn author(user: &Option<User>) -> Value {
    match user {
        Some(u) => json!({ "login": u.login, "avatar_url": u.avatar_url }),
        // GitHub's name for deleted accounts.
        None => json!({ "login": "ghost", "avatar_url": "" }),
    }
}

/// GitHub marks an edit only by a later `updated_at`; a second of slack for the write itself.
fn edited(created: DateTime<Utc>, updated: DateTime<Utc>) -> bool {
    updated - created > Duration::seconds(1)
}

fn summary(issue: &Issue) -> Value {
    json!({
        "number": issue.number,
        "title": issue.title,
        "kind": kind(issue),
        "status": status(issue),
        "author": author(&issue.user),
        "comments": issue.comments,
        "created_at": issue.created_at,
        "updated_at": issue.updated_at,
        "html_url": issue.html_url,
    })
}

fn markdown(body: &Option<String>) -> String {
    forge_core::markdown::render_with_images(body.as_deref().unwrap_or_default(), forge_core::markdown::GITHUB_ATTACHMENTS)
}

fn reply_json(c: &IssueComment) -> Value {
    json!({
        "id": c.id,
        "author": author(&c.user),
        "body": c.body.clone().unwrap_or_default(),
        "body_html": markdown(&c.body),
        "created_at": c.created_at,
        "edited": edited(c.created_at, c.updated_at),
        "html_url": c.html_url,
    })
}

pub fn new_issue_url(repo: &str) -> String {
    format!("https://github.com/{repo}/issues/new")
}

/// The repo whose issues a route works on: an addon's, or the platform's own.
pub struct Target {
    pub repo: String,
    pub installation: Option<i64>,
    /// The addon's owner turned the Issues tab on; always for the platform.
    pub enabled: bool,
}

/// The App installation on the platform's repo, asked of GitHub at most once a minute.
// ponytail: per-process; one API instance.
/// The installation (or none) and when GitHub was asked.
type Asked = Option<(Option<i64>, Instant)>;
static PLATFORM_INSTALLATION: LazyLock<Mutex<Asked>> = LazyLock::new(Default::default);

async fn platform_installation(state: &AppState) -> Option<i64> {
    let cached = *PLATFORM_INSTALLATION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((installation, at)) = cached
        && at.elapsed() < std::time::Duration::from_secs(60)
    {
        return installation;
    }
    let installation = state.github.installation_for_repo(&state.config.launcher_repo).await.ok().flatten();
    *PLATFORM_INSTALLATION.lock().unwrap_or_else(|e| e.into_inner()) = Some((installation, Instant::now()));
    installation
}

/// `slug` in the path: that addon's repo, unless the addon is blocked; none: the platform's.
async fn target(state: &AppState, path: &HashMap<String, String>) -> ApiResult<Target> {
    match path.get("slug") {
        Some(slug) => {
            let addon: AddonRow = load_addon(&state.pool, slug).await?;
            not_blocked(&addon)?;
            Ok(Target { repo: addon.repo_full_name, installation: addon.installation_id, enabled: addon.issues_enabled })
        }
        None => Ok(Target { repo: state.config.launcher_repo.clone(), installation: platform_installation(state).await, enabled: true }),
    }
}

/// Like `target`, for everything but the list: a tab the owner did not turn on reads and writes
/// nothing on GitHub.
async fn open_target(state: &AppState, path: &HashMap<String, String>) -> ApiResult<Target> {
    let target = target(state, path).await?;
    if target.enabled {
        Ok(target)
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "issues_off", "The creator did not turn on issues on SurvivorsForge"))
    }
}

/// A number from the path; anything else is not a page here.
fn number(path: &HashMap<String, String>, key: &str) -> ApiResult<i64> {
    path.get(key).and_then(|n| n.parse().ok()).ok_or_else(ApiError::not_found)
}

/// Whether issues written here reach GitHub: App installed with `Issues: Read & write`.
async fn writable(state: &AppState, target: &Target) -> bool {
    match target.installation {
        Some(installation) => state.github.issues_writable(installation).await.unwrap_or(false),
        None => false,
    }
}

/// What the pages need besides the issues themselves.
async fn frame(state: &AppState, target: &Target) -> ApiResult<Value> {
    let repo = state.github.repo_by_name(target.installation, &target.repo).await.map_err(github_error)?;
    let can_write = repo.has_issues && writable(state, target).await;
    Ok(json!({
        "repo": target.repo,
        "issues_here": true,
        "ready": can_write,
        "issues_enabled": repo.has_issues,
        "can_write": can_write,
        "issues_url": format!("https://github.com/{}/issues", target.repo),
        "new_issue_url": new_issue_url(&target.repo),
    }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    page: Option<usize>,
}

/// Matches the title, the author or `#number`.
fn matches(issue: &Issue, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    if let Some(n) = query.strip_prefix('#') {
        return n.parse() == Ok(issue.number);
    }
    issue.title.to_lowercase().contains(&query) || issue.user.as_ref().is_some_and(|u| u.login.to_lowercase().contains(&query))
}

/// A tab the owner did not turn on: only the way to GitHub, without asking GitHub anything.
fn off(target: &Target) -> Value {
    json!({
        "repo": target.repo,
        "issues_here": false,
        "ready": false,
        "issues_url": format!("https://github.com/{}/issues", target.repo),
        "new_issue_url": new_issue_url(&target.repo),
    })
}

/// `GET .../issues/frame`: whether bugs are reported here (`ready`) or on GitHub, without the list,
/// for the launcher and the edit page.
pub async fn frame_only(State(state): State<AppState>, Path(path): Path<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let target = target(&state, &path).await?;
    if !target.enabled {
        return Ok(Json(off(&target)));
    }
    Ok(Json(frame(&state, &target).await?))
}

pub async fn list(State(state): State<AppState>, Path(path): Path<HashMap<String, String>>, Query(query): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let target = target(&state, &path).await?;
    if !target.enabled {
        return Ok(Json(off(&target)));
    }
    let mut answer = frame(&state, &target).await?;
    if answer["issues_enabled"] == false {
        return Ok(Json(answer));
    }
    let all = state.github.issues(target.installation, &target.repo).await.map_err(github_error)?;
    let truncated = all.len() >= 100;
    let issues: Vec<Issue> = all.into_iter().filter(|i| i.pull_request.is_none()).collect();
    let open = issues.iter().filter(|i| i.state == "open").count();
    let state_filter = match query.state.as_deref() {
        Some("closed") => "closed",
        Some("all") => "all",
        _ => "open",
    };
    let shown: Vec<&Issue> = issues
        .iter()
        .filter(|i| state_filter == "all" || i.state == state_filter)
        .filter(|i| matches(i, query.q.as_deref().unwrap_or_default()))
        .collect();
    let pages = shown.len().div_ceil(PER_PAGE).max(1);
    let page = query.page.unwrap_or(1).clamp(1, pages);
    answer["state"] = json!(state_filter);
    answer["counts"] = json!({ "open": open, "closed": issues.len() - open, "all": issues.len() });
    answer["matching"] = json!(shown.len());
    answer["page"] = json!(page);
    answer["pages"] = json!(pages);
    answer["truncated"] = json!(truncated);
    answer["issues"] = shown.iter().skip((page - 1) * PER_PAGE).take(PER_PAGE).map(|i| summary(i)).collect();
    Ok(Json(answer))
}

pub async fn show(State(state): State<AppState>, Path(path): Path<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let number = number(&path, "number")?;
    let target = open_target(&state, &path).await?;
    let issue = state
        .github
        .issue(target.installation, &target.repo, number)
        .await
        .map_err(github_error)?
        .filter(|i| i.pull_request.is_none())
        .ok_or_else(ApiError::not_found)?;
    let replies = if issue.comments > 0 {
        state.github.issue_comments(target.installation, &target.repo, number).await.map_err(github_error)?
    } else {
        Vec::new()
    };
    let mut answer = frame(&state, &target).await?;
    let mut item = summary(&issue);
    item["body"] = json!(issue.body.clone().unwrap_or_default());
    item["body_html"] = json!(markdown(&issue.body));
    item["state"] = json!(issue.state);
    item["locked"] = json!(issue.locked);
    item["edited"] = json!(edited(issue.created_at, issue.updated_at) && issue.comments == 0);
    answer["issue"] = item;
    answer["replies"] = replies.iter().map(reply_json).collect();
    Ok(Json(answer))
}

/// Writes per user and kind within the last hour.
// ponytail: per-process; the API runs once. GitHub limits content creation on its side too.
type Writes = HashMap<(i64, &'static str), Vec<DateTime<Utc>>>;
static WRITES: LazyLock<Mutex<Writes>> = LazyLock::new(Default::default);

fn within_limit(user_id: i64, what: &'static str, max: usize, now: DateTime<Utc>) -> ApiResult<()> {
    let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let times = writes.entry((user_id, what)).or_default();
    times.retain(|t| now - *t < Duration::hours(1));
    if times.len() >= max {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many", format!("At most {max} per hour, try again later")));
    }
    times.push(now);
    Ok(())
}

fn text(value: &str, field: &str, max: usize, required: bool) -> ApiResult<String> {
    let value = value.trim();
    if required && value.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", format!("The {field} is empty")));
    }
    if value.chars().count() > max {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", format!("The {field} is longer than {max} characters")));
    }
    Ok(value.to_string())
}

/// What GitHub's refusal of a write means here; `denied` for a `403`/`404` on what exists.
fn write_error(e: GitHubError, denied: ApiError) -> ApiError {
    match e {
        GitHubError::Http(401) => ApiError::new(StatusCode::FORBIDDEN, "github_reauth", "Log in again to post on GitHub"),
        GitHubError::Http(403 | 404 | 410) => denied,
        GitHubError::Http(422) => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "GitHub did not accept this text"),
        GitHubError::RateLimited { .. } => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "github_rate_limited", "GitHub asks to slow down, try again in a few minutes"),
        e => github_error(e),
    }
}

fn not_enabled(target: &Target) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "issues_not_enabled",
        format!("Issues cannot be written here for this repo yet; open one on GitHub: {}", new_issue_url(&target.repo)),
    )
}

fn not_allowed() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", "GitHub allows this only to the author and the repo's team")
}

#[derive(Deserialize)]
pub struct NewIssue {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

pub async fn create(State(state): State<AppState>, github: GitHubUser, Path(path): Path<HashMap<String, String>>, Json(body): Json<NewIssue>) -> ApiResult<(StatusCode, Json<Value>)> {
    let target = open_target(&state, &path).await?;
    let label = label(&body.kind).ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "Choose a bug report, a feature request or a question"))?;
    let title = text(&body.title, "title", MAX_TITLE, true)?;
    let text_body = text(&body.body, "description", MAX_BODY, true)?;
    let installation = target.installation.ok_or_else(|| not_enabled(&target))?;
    within_limit(github.user.id, "issue", ISSUES_PER_HOUR, (state.clock)())?;
    let issue = state.github.create_issue(&github.token, &target.repo, &title, &text_body).await.map_err(|e| write_error(e, not_enabled(&target)))?;
    // The issue stands without its label; the repo's team can still add it.
    if let Err(e) = state.github.add_labels(installation, &target.repo, issue.number, &[label]).await {
        tracing::warn!(error = %e, issue = issue.number, "cannot label the issue");
    }
    Ok((StatusCode::CREATED, Json(json!({ "number": issue.number, "html_url": issue.html_url }))))
}

#[derive(Deserialize)]
pub struct IssueChanges {
    title: Option<String>,
    body: Option<String>,
    /// `open` or `closed`.
    state: Option<String>,
}

pub async fn update(State(state): State<AppState>, github: GitHubUser, Path(path): Path<HashMap<String, String>>, Json(body): Json<IssueChanges>) -> ApiResult<Json<Value>> {
    let number = number(&path, "number")?;
    let target = open_target(&state, &path).await?;
    let mut changes = serde_json::Map::new();
    if let Some(title) = &body.title {
        changes.insert("title".into(), json!(text(title, "title", MAX_TITLE, true)?));
    }
    if let Some(text_body) = &body.body {
        changes.insert("body".into(), json!(text(text_body, "description", MAX_BODY, true)?));
    }
    match body.state.as_deref() {
        Some(open_or_closed @ ("open" | "closed")) => {
            changes.insert("state".into(), json!(open_or_closed));
        }
        Some(_) => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "state is open or closed")),
        None => {}
    }
    if changes.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "Nothing to change"));
    }
    within_limit(github.user.id, "reply", REPLIES_PER_HOUR, (state.clock)())?;
    let issue = state.github.update_issue(&github.token, &target.repo, number, &Value::Object(changes)).await.map_err(|e| write_error(e, not_allowed()))?;
    Ok(Json(summary(&issue)))
}

#[derive(Deserialize)]
pub struct ReplyBody {
    #[serde(default)]
    body: String,
}

pub async fn reply(State(state): State<AppState>, github: GitHubUser, Path(path): Path<HashMap<String, String>>, Json(body): Json<ReplyBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let number = number(&path, "number")?;
    let target = open_target(&state, &path).await?;
    let text_body = text(&body.body, "reply", MAX_BODY, true)?;
    if target.installation.is_none() {
        return Err(not_enabled(&target));
    }
    within_limit(github.user.id, "reply", REPLIES_PER_HOUR, (state.clock)())?;
    let reply = state.github.create_comment(&github.token, &target.repo, number, &text_body).await.map_err(|e| write_error(e, not_enabled(&target)))?;
    Ok((StatusCode::CREATED, Json(reply_json(&reply))))
}

pub async fn edit_reply(State(state): State<AppState>, github: GitHubUser, Path(path): Path<HashMap<String, String>>, Json(body): Json<ReplyBody>) -> ApiResult<Json<Value>> {
    let id = number(&path, "id")?;
    let target = open_target(&state, &path).await?;
    let text_body = text(&body.body, "reply", MAX_BODY, true)?;
    within_limit(github.user.id, "reply", REPLIES_PER_HOUR, (state.clock)())?;
    let reply = state.github.update_comment(&github.token, &target.repo, id, &text_body).await.map_err(|e| write_error(e, not_allowed()))?;
    Ok(Json(reply_json(&reply)))
}

pub async fn delete_reply(State(state): State<AppState>, github: GitHubUser, Path(path): Path<HashMap<String, String>>) -> ApiResult<StatusCode> {
    let id = number(&path, "id")?;
    let target = open_target(&state, &path).await?;
    state.github.delete_comment(&github.token, &target.repo, id).await.map_err(|e| write_error(e, not_allowed()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(state: &str, reason: Option<&str>, labels: &[&str]) -> Issue {
        serde_json::from_value(json!({
            "number": 1, "title": "Crash", "state": state, "state_reason": reason,
            "labels": labels.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>(),
            "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-01T00:00:00Z", "html_url": "https://github.com/o/r/issues/1",
        }))
        .unwrap()
    }

    #[test]
    fn kind_and_status_come_from_labels_and_state() {
        assert_eq!((kind(&issue("open", None, &["bug"])), status(&issue("open", None, &["bug"]))), ("bug", "open"));
        assert_eq!(kind(&issue("open", None, &["Enhancement"])), "feature");
        assert_eq!(kind(&issue("open", None, &["question"])), "question");
        assert_eq!(kind(&issue("open", None, &["question", "bug"])), "bug", "a question that turned out a bug");
        assert_eq!(kind(&issue("open", None, &[])), "other");
        assert_eq!(status(&issue("open", None, &["in progress"])), "in_progress");
        assert_eq!(status(&issue("closed", Some("completed"), &["in progress"])), "closed");
        assert_eq!(status(&issue("closed", Some("not_planned"), &[])), "not_planned");
    }

    #[test]
    fn search_matches_title_author_and_number() {
        let mut i = issue("open", None, &[]);
        i.user = Some(User { id: 1, login: "Maiden".into(), avatar_url: String::new() });
        assert!(matches(&i, "crash") && matches(&i, "maid") && matches(&i, "#1") && matches(&i, " "));
        assert!(!matches(&i, "#2") && !matches(&i, "freeze"));
    }

    #[test]
    fn writes_are_limited_per_user_and_hour() {
        let now = Utc::now();
        for _ in 0..ISSUES_PER_HOUR {
            within_limit(-7, "issue", ISSUES_PER_HOUR, now).unwrap();
        }
        assert_eq!(within_limit(-7, "issue", ISSUES_PER_HOUR, now).unwrap_err().code, "too_many");
        within_limit(-7, "reply", REPLIES_PER_HOUR, now).unwrap();
        within_limit(-7, "issue", ISSUES_PER_HOUR, now + Duration::hours(1)).unwrap();
    }
}
