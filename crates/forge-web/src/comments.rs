//! Comments under an addon (`/addons/{slug}/comments`, the addon page's Comments tab): a discussion
//! kept on the platform, apart from the GitHub issues. Replies sit under the comment they answer;
//! every change goes through the API as the visitor and comes back here with a notice.

use crate::api_client::ApiError;
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::issues::query;
use crate::public::seg;
use crate::templates::{Permission, Visitor, render};
use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Must match the API's page size; the API sends one more when older comments exist.
pub const PER_PAGE: usize = 30;
/// Replies deeper than this are drawn at this depth, so a long thread stays readable on a phone.
pub const MAX_DEPTH: usize = 3;

/// The reasons of `/v1/reports` that fit a comment, with what the form says.
pub const REASONS: &[(&str, &str)] = &[("spam", "Spam"), ("inappropriate", "Inappropriate or hurtful"), ("other", "Something else")];

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

pub struct Comment {
    pub id: i64,
    pub author: String,
    pub avatar_url: String,
    pub body: String,
    pub created: String,
    pub edited: bool,
    pub deleted: bool,
    /// 0 for a comment of its own, up to `MAX_DEPTH` for replies.
    pub depth: usize,
    pub parent_id: Option<i64>,
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_report: bool,
}

#[derive(Template)]
#[template(path = "comments.html")]
pub struct CommentsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub head: crate::templates::AddonHead,
    pub comments: Vec<Comment>,
    /// The owner has not turned the comments on.
    pub off: bool,
    /// The link to older comments.
    pub older: Option<String>,
    pub reasons: &'static [(&'static str, &'static str)],
    pub done: Option<String>,
    pub error: Option<String>,
    /// Text the API refused, back in its field with the reason.
    pub draft: Option<Draft>,
}

/// A refused comment: `form` is `new`, `reply` (to comment `id`) or `edit` (of comment `id`).
pub struct Draft {
    pub form: &'static str,
    pub id: i64,
    pub body: String,
    pub error: String,
}

impl CommentsPage {
    fn draft_of(&self, form: &str, id: i64) -> Option<&Draft> {
        self.draft.as_ref().filter(|d| d.form == form && d.id == id)
    }

    /// The refused text of this form, or `fallback`.
    pub fn draft_body<'a>(&'a self, form: &str, id: impl std::borrow::Borrow<i64>, fallback: &'a str) -> &'a str {
        self.draft_of(form, *id.borrow()).map_or(fallback, |d| d.body.as_str())
    }

    /// Why the API refused this form's text; empty when it did not.
    pub fn draft_error(&self, form: &str, id: impl std::borrow::Borrow<i64>) -> &str {
        self.draft_of(form, *id.borrow()).map_or("", |d| d.error.as_str())
    }
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    before: Option<i64>,
    done: Option<String>,
    error: Option<String>,
}

/// Comments of one page, newest first, each followed by its replies, oldest first. A reply whose
/// comment is on an older page stands on its own.
pub fn threads(comments: Vec<Comment>) -> Vec<Comment> {
    let on_page: HashMap<i64, usize> = comments.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let mut replies: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for (i, c) in comments.iter().enumerate() {
        match c.parent_id.filter(|p| on_page.contains_key(p)) {
            Some(parent) => replies.entry(parent).or_default().push(i),
            None => roots.push(i),
        }
    }
    // The page comes newest first, replies read better in the order they were written.
    let mut order = Vec::with_capacity(comments.len());
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        order.push((i, depth));
        if let Some(children) = replies.get(&comments[i].id) {
            stack.extend(children.iter().map(|&c| (c, depth + 1)));
        }
    }
    let mut slots: Vec<Option<Comment>> = comments.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|(i, depth)| slots[i].take().map(|c| Comment { depth: depth.min(MAX_DEPTH), ..c }))
        .collect()
}

pub async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(q): Query<ListQuery>,
) -> WebResult<Html<String>> {
    page(&state, &jar, csrf, slug, q, None).await
}

async fn page(state: &AppState, jar: &PrivateCookieJar, csrf: CsrfToken, slug: String, q: ListQuery, draft: Option<Draft>) -> WebResult<Html<String>> {
    let session = visitor(state, jar).await?;
    let addon = state.api.get(&format!("/v1/addons/{}", seg(&slug)), None).await?;
    if addon["blocked"] == true {
        return Err(WebError::NotFound);
    }
    let page = |user, comments, older| CommentsPage {
        user,
        csrf: csrf.0.clone(),
        slug: slug.clone(),
        name: s(&addon["name"]),
        head: crate::templates::AddonHead::from_api(&addon),
        comments,
        off: addon["comments_enabled"] != true,
        older,
        reasons: REASONS,
        done: q.done.clone(),
        error: q.error.clone(),
        draft: None,
    };
    if addon["comments_enabled"] != true {
        return Ok(render(&page(session.map(|(u, _)| u), Vec::new(), None)));
    }
    let before = q.before.map(|n| format!("?before={n}")).unwrap_or_default();
    let list = state.api.get(&format!("/v1/addons/{}/comments{before}", seg(&slug)), None).await?;
    let items = list.as_array().cloned().unwrap_or_default();
    // The addon's team and moderators may delete comments; only the author changes one.
    let team = match &session {
        Some((_, token)) => {
            let p = state.api.get(&format!("/v1/addons/{}/permissions", seg(&slug)), Some(token)).await.ok();
            p.is_some_and(|p| p["owner"] == true || p["admin"] == true || p["edit_description"] == true)
        }
        None => false,
    };
    let user = session.map(|(u, _)| u);
    let moderator = user.as_ref().is_some_and(|u| u.can(Permission::Moderate));
    let older = (items.len() > PER_PAGE).then(|| format!("/addons/{slug}/comments?before={}", items[PER_PAGE - 1]["id"]));
    let comments = items
        .iter()
        .take(PER_PAGE)
        .map(|c| {
            let author_id = c["author_id"].as_i64().unwrap_or_default();
            let own = user.as_ref().is_some_and(|u| u.id > 0 && u.id == author_id);
            let deleted = c["deleted"] == true;
            Comment {
                id: c["id"].as_i64().unwrap_or_default(),
                author: s(&c["author"]),
                avatar_url: s(&c["avatar_url"]),
                body: s(&c["body"]),
                created: s(&c["created_at"]).chars().take(10).collect(),
                edited: c["edited_at"].is_string(),
                deleted,
                depth: 0,
                parent_id: c["parent_id"].as_i64(),
                can_edit: !deleted && own,
                can_delete: !deleted && (own || moderator || team),
                can_report: !deleted && user.is_some() && !own,
            }
        })
        .collect();
    Ok(render(&CommentsPage { draft, ..page(user, threads(comments), older) }))
}

#[derive(Deserialize, Default)]
pub struct CommentForm {
    #[serde(default)]
    body: String,
    #[serde(default)]
    parent_id: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ReportForm {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    text: String,
}

/// Back to the discussion with a notice: `done` on success, the API's refusal as `error`.
fn back(slug: &str, anchor: Option<i64>, result: Result<Value, ApiError>, done: &str) -> WebResult<Redirect> {
    let notice = match result {
        Ok(_) => ("done", done.to_string()),
        Err(ApiError::Unauthorized) => return Err(WebError::LoginRequired),
        Err(ApiError::Api { message, .. }) => ("error", message),
        Err(e) => return Err(e.into()),
    };
    let anchor = anchor.map(|id| format!("#comment-{id}")).unwrap_or_default();
    Ok(Redirect::to(&format!("/addons/{slug}/comments?{}{anchor}", query(&[(notice.0, &notice.1)]))))
}

async fn token(state: &AppState, jar: &PrivateCookieJar) -> WebResult<String> {
    Ok(visitor(state, jar).await?.ok_or(WebError::LoginRequired)?.1)
}

/// A refused comment or change: the page again, the text in its field and the reason under it,
/// instead of a redirect that would lose what was typed.
async fn refused(state: &AppState, jar: &PrivateCookieJar, csrf: CsrfToken, slug: String, draft: Draft) -> WebResult<Response> {
    let page = page(state, jar, csrf, slug, ListQuery::default(), Some(draft)).await?;
    Ok((StatusCode::UNPROCESSABLE_ENTITY, page).into_response())
}

pub async fn create(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<CommentForm>,
) -> WebResult<Response> {
    let token = token(&state, &jar).await?;
    let body = json!({ "body": form.body, "parent_id": form.parent_id });
    let result = state.api.post(&format!("/v1/addons/{}/comments", seg(&slug)), Some(&token), &body).await;
    if let Err(ApiError::Api { message, .. }) = &result {
        let (kind, id) = form.parent_id.map_or(("new", 0), |p| ("reply", p));
        let draft = Draft { form: kind, id, body: form.body, error: message.clone() };
        return refused(&state, &jar, csrf, slug, draft).await;
    }
    let anchor = result.as_ref().ok().and_then(|r| r["id"].as_i64());
    Ok(back(&slug, anchor, result, "Your comment is posted.")?.into_response())
}

pub async fn edit(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, id)): Path<(String, i64)>,
    Form(form): Form<CommentForm>,
) -> WebResult<Response> {
    let token = token(&state, &jar).await?;
    let result = state.api.call(Method::PATCH, &format!("/v1/addons/{}/comments/{id}", seg(&slug)), Some(&token), Some(&json!({ "body": form.body }))).await;
    if let Err(ApiError::Api { message, .. }) = &result {
        let draft = Draft { form: "edit", id, body: form.body, error: message.clone() };
        return refused(&state, &jar, csrf, slug, draft).await;
    }
    Ok(back(&slug, Some(id), result, "The comment is changed.")?.into_response())
}

pub async fn delete(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug, id)): Path<(String, i64)>) -> WebResult<Redirect> {
    let token = token(&state, &jar).await?;
    let result = state.api.call(Method::DELETE, &format!("/v1/addons/{}/comments/{id}", seg(&slug)), Some(&token), None).await;
    back(&slug, Some(id), result, "The comment is deleted.")
}

pub async fn report(State(state): State<AppState>, jar: PrivateCookieJar, Path((slug, id)): Path<(String, i64)>, Form(form): Form<ReportForm>) -> WebResult<Redirect> {
    let token = token(&state, &jar).await?;
    let body = json!({ "target_type": "comment", "target": id.to_string(), "reason": form.reason, "text": form.text.trim() });
    let result = state.api.post("/v1/reports", Some(&token), &body).await;
    back(&slug, Some(id), result, "Report sent. An admin will check this comment.")
}

#[cfg(test)]
mod tests {
    use super::{Comment, MAX_DEPTH, threads};

    fn comment(id: i64, parent: Option<i64>) -> Comment {
        Comment {
            id,
            author: format!("u{id}"),
            avatar_url: String::new(),
            body: format!("text {id}"),
            created: String::new(),
            edited: false,
            deleted: false,
            depth: 0,
            parent_id: parent,
            can_edit: false,
            can_delete: false,
            can_report: false,
        }
    }

    fn shape(comments: &[Comment]) -> Vec<(i64, usize)> {
        comments.iter().map(|c| (c.id, c.depth)).collect()
    }

    #[test]
    fn replies_follow_their_comment_oldest_first_under_the_newest_comments() {
        // As the API sends them: newest first.
        let page = vec![comment(6, Some(1)), comment(5, None), comment(4, Some(2)), comment(3, Some(1)), comment(2, Some(1)), comment(1, None)];
        assert_eq!(shape(&threads(page)), vec![(5, 0), (1, 0), (2, 1), (4, 2), (3, 1), (6, 1)]);
    }

    #[test]
    fn a_reply_to_a_comment_on_an_older_page_stands_on_its_own() {
        assert_eq!(shape(&threads(vec![comment(9, Some(1)), comment(8, None)])), vec![(9, 0), (8, 0)]);
    }

    #[test]
    fn deep_threads_stop_indenting() {
        let page: Vec<Comment> = (1..=6).rev().map(|id| comment(id, (id > 1).then(|| id - 1))).collect();
        assert_eq!(threads(page).iter().map(|c| c.depth).max(), Some(MAX_DEPTH));
    }
}
