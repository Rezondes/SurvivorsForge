//! Reporting an addon or one of its versions to the admins (`POST /v1/reports`). The launcher has
//! no login, so its "Report" button opens this page too.

use crate::api_client::ApiError;
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::templates::{ReportDonePage, ReportPage, render};
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, response::Html};
use axum_extra::extract::PrivateCookieJar;
use serde::Deserialize;
use serde_json::{Value, json};

/// The reasons the API accepts, with what the form says.
pub const REASONS: &[(&str, &str)] = &[
    ("malware", "Harmful code (steals data, damages the game or the computer)"),
    ("stolen", "Not the work of the person who published it"),
    ("broken", "Does not work"),
    ("inappropriate", "Inappropriate content"),
    ("spam", "Spam"),
    ("other", "Something else"),
];

#[derive(Deserialize, Default)]
pub struct ReportQuery {
    #[serde(default)]
    version: String,
}

#[derive(Deserialize, Default)]
pub struct ReportForm {
    /// A version's tag; empty for the whole addon.
    #[serde(default)]
    version: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    text: String,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// The addon's name and its versions as (tag, id), newest first.
async fn addon(state: &AppState, slug: &str) -> WebResult<(String, Vec<(String, i64)>)> {
    let a = state.api.get(&format!("/v1/addons/{}", crate::public::seg(slug)), None).await?;
    let versions = state.api.get(&format!("/v1/addons/{}/versions", crate::public::seg(slug)), None).await?;
    let tags = versions.as_array().into_iter().flatten().map(|v| (s(&v["tag"]), v["id"].as_i64().unwrap_or(0))).collect();
    Ok((s(&a["name"]), tags))
}

/// `GET /addons/{slug}/report?version=`: the form; `version` picks a version in it.
pub async fn report_page(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(q): Query<ReportQuery>,
) -> WebResult<Html<String>> {
    let (user, _) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let (name, versions) = addon(&state, &slug).await?;
    let form = ReportForm { version: q.version, ..ReportForm::default() };
    Ok(render(&page(user, csrf.0, slug, name, versions, form, None)))
}

/// `POST /addons/{slug}/report`: sends the report; a refused one shows the form again as it was.
pub async fn send_report(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Form(form): Form<ReportForm>,
) -> WebResult<Response> {
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let (name, versions) = addon(&state, &slug).await?;
    let body = match versions.iter().find(|(tag, _)| !form.version.is_empty() && *tag == form.version) {
        Some((_, id)) => json!({ "target_type": "version", "target": id.to_string(), "reason": form.reason, "text": form.text.trim() }),
        None => json!({ "target_type": "addon", "target": slug, "reason": form.reason, "text": form.text.trim() }),
    };
    match state.api.post("/v1/reports", Some(&token), &body).await {
        Ok(_) => Ok(render(&ReportDonePage { user: Some(user), csrf: csrf.0, slug, name }).into_response()),
        Err(ApiError::Api { status, message, .. }) if matches!(status, 409 | 422 | 429) => {
            let page = page(user, csrf.0, slug, name, versions, form, Some(message));
            Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), render(&page)).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

fn page(user: crate::templates::Visitor, csrf: String, slug: String, name: String, versions: Vec<(String, i64)>, form: ReportForm, error: Option<String>) -> ReportPage {
    ReportPage {
        user: Some(user),
        csrf,
        slug,
        name,
        versions: versions.into_iter().map(|(tag, _)| tag).collect(),
        reasons: REASONS,
        version: form.version,
        reason: form.reason,
        text: form.text,
        error,
    }
}
