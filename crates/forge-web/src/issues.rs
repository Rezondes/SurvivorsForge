//! Server-rendered community issues, proxied through forge-api.
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::templates::{IssuesPage, IssuePage, IssueItem, IssueComment, render};
use axum::{Extension, extract::{Path, Form, Query, State}, response::{Html, Redirect}};
use axum_extra::extract::PrivateCookieJar;
use serde::Deserialize;
use serde_json::{Value, json};

fn s(v: &Value) -> String { v.as_str().unwrap_or_default().to_owned() }
fn segment(v: &str) -> String {
    v.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
        (b as char).to_string()
    } else { format!("%{b:02X}") }).collect()
}
#[derive(Deserialize)]
pub struct IssueForm { title: String, body: String, kind: String }
#[derive(Deserialize)]
pub struct CommentForm { body: String }
#[derive(Deserialize)]
pub struct StatusForm { status: String }
#[derive(Deserialize)]
pub struct IssuePageQuery { before: Option<i64> }

pub async fn list(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>, Query(page): Query<IssuePageQuery>) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v,_)|v);
    let a = state.api.get(&format!("/v1/addons/{}",segment(&slug)),None).await?;
    if a["blocked"] == true { return Err(WebError::NotFound); }
    let before = page.before.filter(|n| *n > 0);
    let path = format!("/v1/addons/{}/issues{}", segment(&slug), before.map(|n| format!("?before={n}")).unwrap_or_default());
    let data = state.api.get(&path,None).await?;
    let next = data.as_array().and_then(|rows| (rows.len() > 30).then(|| rows[29]["id"].as_i64()).flatten());
    let issues = data.as_array().into_iter().flatten().take(30).map(|i| IssueItem {
        id: i["id"].as_i64().unwrap_or(0), title:s(&i["title"]), kind:s(&i["kind"]),
        status:s(&i["status"]), author:s(&i["author"]),
    }).collect();
    Ok(render(&IssuesPage { user, csrf:csrf.0, slug, name:s(&a["name"]), issues, next }))
}
pub async fn create(State(state): State<AppState>, jar: PrivateCookieJar, Path(slug): Path<String>,
    Form(form): Form<IssueForm>) -> WebResult<Redirect> {
    let (_,token) = visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    let result = state.api.post(&format!("/v1/addons/{}/issues",segment(&slug)),Some(&token),
        &json!({"title":form.title,"body":form.body,"kind":form.kind})).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{}",segment(&slug),result["id"].as_i64().unwrap_or(0))))
}
pub async fn show(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>,
    Path((slug,id)): Path<(String,i64)>) -> WebResult<Html<String>> {
    let session = visitor(&state,&jar).await?;
    let user = session.as_ref().map(|(u,_)|u.clone());
    let a = state.api.get(&format!("/v1/addons/{}",segment(&slug)),None).await?;
    if a["blocked"] == true { return Err(WebError::NotFound); }
    let i = state.api.get(&format!("/v1/addons/{}/issues/{id}",segment(&slug)),None).await?;
    let permissions = if let Some((_,token)) = &session {
        state.api.get(&format!("/v1/addons/{}/permissions",segment(&slug)),Some(token)).await.ok()
    } else { None };
    let can_manage = permissions.as_ref().is_some_and(|p| p["owner"]==true || p["admin"]==true || p["edit_description"]==true);
    let actor_id = user.as_ref().map(|u| u.id);
    let moderator = user.as_ref().is_some_and(|u| u.can(crate::templates::Permission::Moderate));
    let author = i["author_id"].as_i64();
    let unacknowledged = i["acknowledged_at"].is_null();
    let addon_owner = permissions.as_ref().is_some_and(|p| p["owner"]==true);
    let can_edit_issue = moderator || addon_owner || (actor_id.is_some() && actor_id == author && unacknowledged);
    let can_delete_issue = moderator || (actor_id.is_some() && actor_id == author && unacknowledged);
    let comments = i["comments"].as_array().into_iter().flatten().map(|c| IssueComment {
        id:c["id"].as_i64().unwrap_or(0),
        can_edit: !c["deleted"].as_bool().unwrap_or(false) && (moderator || (actor_id.is_some() && actor_id == c["author_id"].as_i64())),
        author:s(&c["author"]), body:s(&c["body"])
    }).collect();
    Ok(render(&IssuePage { user, csrf:csrf.0, slug, name:s(&a["name"]), id,
        title:s(&i["title"]), body:s(&i["body"]), kind:s(&i["kind"]), status:s(&i["status"]),
        author:s(&i["author"]), comments, can_manage, can_edit_issue, can_delete_issue }))
}
pub async fn comment(State(state): State<AppState>, jar: PrivateCookieJar,
    Path((slug,id)): Path<(String,i64)>, Form(form): Form<CommentForm>) -> WebResult<Redirect> {
    let (_,token) = visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.post(&format!("/v1/addons/{}/issues/{id}/comments",segment(&slug)),Some(&token),
        &json!({"body":form.body})).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{id}",segment(&slug))))
}
pub async fn status(State(state): State<AppState>, jar: PrivateCookieJar,
    Path((slug,id)): Path<(String,i64)>, Form(form): Form<StatusForm>) -> WebResult<Redirect> {
    let (_,token) = visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.call(reqwest::Method::PATCH,
        &format!("/v1/addons/{}/issues/{id}",segment(&slug)),Some(&token),
        Some(&json!({"status":form.status}))).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{id}",segment(&slug))))
}

// Moderation and author editing actions.
#[derive(Deserialize)]
pub struct EditIssueForm { title: String, body: String, kind: String }
pub async fn edit(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug,id)): Path<(String,i64)>,
    Form(form): Form<EditIssueForm>) -> WebResult<Redirect> {
    let (_,token)=visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.call(reqwest::Method::PATCH,&format!("/v1/addons/{}/issues/{id}/edit",segment(&slug)),Some(&token),
        Some(&json!({"title":form.title,"body":form.body,"kind":form.kind}))).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{id}",segment(&slug))))
}
pub async fn delete(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug,id)): Path<(String,i64)>)
    -> WebResult<Redirect> {
    let (_,token)=visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.call(reqwest::Method::DELETE,&format!("/v1/addons/{}/issues/{id}",segment(&slug)),Some(&token),None).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues",segment(&slug))))
}
pub async fn edit_comment(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug,id,cid)): Path<(String,i64,i64)>,
    Form(form): Form<CommentForm>) -> WebResult<Redirect> {
    let (_,token)=visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.call(reqwest::Method::PATCH,&format!("/v1/addons/{}/issues/{id}/comments/{cid}",segment(&slug)),Some(&token),
        Some(&json!({"body":form.body}))).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{id}",segment(&slug))))
}
pub async fn delete_comment(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug,id,cid)): Path<(String,i64,i64)>)
    -> WebResult<Redirect> {
    let (_,token)=visitor(&state,&jar).await?.ok_or(WebError::LoginRequired)?;
    state.api.call(reqwest::Method::DELETE,&format!("/v1/addons/{}/issues/{id}/comments/{cid}",segment(&slug)),Some(&token),None).await?;
    Ok(Redirect::to(&format!("/addons/{}/issues/{id}",segment(&slug))))
}
