//! The public pages: addon list and search, addon page, version history, version page and the
//! download confirmation for risky versions.

use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult, error_page};
use crate::templates::{
    AddonPage, AddonsPage, BlockedPage, Card, Dep, DownloadPage, FindingRow, LatestBlock, Shot, VersionPage, VersionRow, VersionsPage, render,
};
use crate::trust::{Notice, notice};
use axum::Extension;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use serde::Deserialize;
use serde_json::Value;

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn date(v: &Value) -> String {
    s(v).chars().take(10).collect()
}

/// Escapes a path segment for the API.
fn seg(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn range(min: &Value, max: &Value) -> String {
    match (min.as_str(), max.as_str()) {
        (Some(a), Some(b)) => format!("{a} to {b}"),
        (Some(a), None) => format!("{a} and later"),
        (None, Some(b)) => format!("up to {b}"),
        (None, None) => "not stated by the author".into(),
    }
}

fn version_row(v: &Value, verified_author: bool) -> VersionRow {
    let tag = s(&v["tag"]);
    VersionRow {
        tag_short: if tag.chars().count() > 20 { format!("{}…", tag.chars().take(18).collect::<String>()) } else { tag.clone() },
        tag,
        name: s(&v["name"]),
        published: date(&v["published_at"]),
        prerelease: v["prerelease"] == true,
        deleted: v["deleted_on_github"] == true,
        notice: notice(v["trust"].as_str().unwrap_or("unscanned"), verified_author, v["source"] == "missing"),
        sha256: s(&v["sha256"]),
        asset_url: s(&v["asset_url"]),
        game: range(&v["game_min"], &v["game_max"]),
        critical: v["findings"]["critical"].as_i64().unwrap_or(0),
        warn: v["findings"]["warn"].as_i64().unwrap_or(0),
        downloads: v["downloads"].as_i64().filter(|n| *n > 0).map(grouped),
    }
}

/// Where a download button points: GitHub directly, or the confirmation page first.
/// Every download goes through the website once, so it is counted; a version that needs a
/// confirmation shows the warning there first.
fn download_href(slug: &str, row: &VersionRow) -> String {
    format!("/addons/{slug}/versions/{}/download", seg(&row.tag))
}

#[derive(Deserialize, Default)]
pub struct DownloadQuery {
    /// Set by the button of the warning page.
    #[serde(default)]
    confirmed: Option<String>,
}

/// Counts the download for `/stats`; a slow or failing API never holds the download up.
async fn count_download(state: &AppState, slug: &str) {
    let (path, body) = (format!("/v1/addons/{}/downloads", seg(slug)), serde_json::json!({}));
    if let Ok(Err(e)) = tokio::time::timeout(std::time::Duration::from_secs(2), state.api.post(&path, None, &body)).await {
        tracing::warn!(error = ?e, "cannot count a download");
    }
}

async fn findings(state: &AppState, slug: &str, tag: &str) -> WebResult<Vec<FindingRow>> {
    let report = state.api.get(&format!("/v1/addons/{}/versions/{}/findings", seg(slug), seg(tag)), None).await?;
    Ok(report["scan"]["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["severity"] != "info")
        .map(|f| FindingRow {
            severity: s(&f["severity"]),
            rule: s(&f["rule"]),
            place: match f["line"].as_i64() {
                Some(line) => format!("{}:{line}", s(&f["file"])),
                None => s(&f["file"]),
            },
            message: s(&f["message"]),
            snippet: s(&f["snippet"]),
        })
        .collect())
}

/// One addon of the API's list as a card (the addon list and the home page).
pub(crate) fn card(a: &Value) -> Card {
    Card {
        slug: s(&a["slug"]),
        name: s(&a["name"]),
        summary: s(&a["summary"]),
        author: if a["owner"].is_object() { s(&a["owner"]["login"]) } else { s(&a["author"]) },
        icon_url: a["icon_url"].as_str().map(str::to_string),
        category: s(&a["category"]),
        notice: match a["trust"].as_str() {
            Some(trust) => notice(trust, a["verified_author"] == true, false),
            None => Notice { class: "", badge: "No release", text: "", downloadable: false, confirm: false },
        },
        downloads: a["downloads"].as_i64().map(grouped),
        updated: s(&a["updated_at"]).chars().take(10).collect(),
    }
}

pub async fn addons(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, RawQuery(raw): RawQuery) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let raw = raw.unwrap_or_default();
    let params: Vec<(String, String)> =
        reqwest::Url::parse(&format!("http://x/?{raw}")).map(|u| u.query_pairs().into_owned().collect()).unwrap_or_default();
    let param = |k: &str| params.iter().find(|(key, v)| key == k && !v.is_empty()).map(|(_, v)| v.clone());
    let query: Vec<(&str, String)> =
        ["q", "category", "trust", "sort", "author", "page"].iter().filter_map(|k| param(k).map(|v| (*k, v))).collect();
    let api_query = reqwest::Url::parse_with_params("http://x/v1/addons", &query).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
    let list = state.api.get(&format!("/v1/addons?{api_query}"), None).await?;

    let cards: Vec<Card> = list["addons"].as_array().into_iter().flatten().map(card).collect();
    let page = param("page").and_then(|p| p.parse().ok()).unwrap_or(1i64).max(1);
    let total = list["total"].as_i64().unwrap_or(0);
    let per_page = list["per_page"].as_i64().unwrap_or(20).max(1);
    let link = |p: i64| {
        let mut q: Vec<(&str, String)> = query.iter().filter(|(k, _)| *k != "page").cloned().collect();
        q.push(("page", p.to_string()));
        reqwest::Url::parse_with_params("http://x/addons", &q).map(|u| format!("/addons?{}", u.query().unwrap_or_default())).unwrap_or_default()
    };
    Ok(render(&AddonsPage {
        user,
        csrf: csrf.0,
        checks: crate::stats::checks(&state).await,
        q: param("q").unwrap_or_default(),
        category: param("category").unwrap_or_default(),
        trust: param("trust").unwrap_or_default(),
        sort: param("sort").unwrap_or_default(),
        author: param("author").unwrap_or_default(),
        total,
        categories: crate::templates::CategoryOption::groups(crate::app::categories(&state).await?),
        cards,
        prev: (page > 1).then(|| link(page - 1)),
        next: (page * per_page < total).then(|| link(page + 1)),
    }))
}

/// `24365` as `24,365`.
pub(crate) fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[derive(Deserialize, Default)]
pub struct ClaimError {
    error: Option<String>,
}

pub async fn addon(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(slug): Path<String>,
    Query(claim): Query<ClaimError>,
) -> WebResult<Response> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let a = state.api.get(&format!("/v1/addons/{}", seg(&slug)), None).await?;
    if a["blocked"] == true {
        let page = BlockedPage { user, csrf: csrf.0, name: s(&a["name"]), reason: s(&a["block_reason"]) };
        return Ok((StatusCode::OK, render(&page)).into_response());
    }
    let verified_author = a["verified_author"] == true;
    let versions = state.api.get(&format!("/v1/addons/{}/versions", seg(&slug)), None).await?;
    let latest = match a["latest_tag"].as_str() {
        Some(tag) => {
            let v = versions.as_array().into_iter().flatten().find(|v| v["tag"] == tag);
            match v {
                Some(v) => {
                    let row = version_row(v, verified_author);
                    let found = if row.critical + row.warn > 0 { findings(&state, &slug, tag).await? } else { Vec::new() };
                    Some(LatestBlock { href: download_href(&slug, &row), row, findings: found })
                }
                None => None,
            }
        }
        None => None,
    };
    // GitHub logins ignore case; the claim itself checks the repo rights on GitHub.
    let repo_owner = s(&a["repo"]).split('/').next().unwrap_or_default().to_string();
    let can_claim = a["claimed"] == false && user.as_ref().is_some_and(|u| u.login.eq_ignore_ascii_case(&repo_owner));
    let page = AddonPage {
        checks: crate::stats::checks(&state).await,
        can_claim,
        install_url: state.install_url(),
        error: claim.error,
        user,
        csrf: csrf.0,
        canonical: format!("{}/addons/{slug}", state.config.public_web_url),
        slug: slug.clone(),
        name: s(&a["name"]),
        summary: s(&a["summary"]),
        author: if a["owner"].is_object() { s(&a["owner"]["login"]) } else { s(&a["author"]) },
        claimed: a["claimed"] != false,
        category: s(&a["category"]),
        repo: s(&a["repo"]),
        description_html: s(&a["description_html"]),
        icon_url: a["icon_url"].as_str().map(str::to_string),
        shots: a["images"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| Shot { url: s(&i["url"]), caption: s(&i["caption"]), width: i["width"].as_i64().unwrap_or(0), height: i["height"].as_i64().unwrap_or(0) })
            .collect(),
        contributors: a["contributors"].as_array().into_iter().flatten().map(|c| s(&c["login"])).collect(),
        deps: a["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| Dep {
                slug: s(&d["slug"]),
                name: s(&d["name"]),
                required: d["required"] == true,
                available: d["available"] == true,
                notice: notice(d["trust"].as_str().unwrap_or("unscanned"), false, false),
            })
            .collect(),
        latest,
    };
    Ok(render(&page).into_response())
}

pub async fn versions(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(slug): Path<String>) -> WebResult<Html<String>> {
    let visitor = visitor(&state, &jar).await?;
    let a = state.api.get(&format!("/v1/addons/{}", seg(&slug)), None).await?;
    if a["blocked"] == true {
        return Err(WebError::NotFound);
    }
    // Owners and contributors come here from the edit toolbar; keep it for them.
    let perms = match &visitor {
        Some((_, token)) => crate::creator::perms(&state, &slug, token).await.ok().filter(|p| p.owner || p.edit_description || p.edit_images),
        None => None,
    };
    let user = visitor.map(|(v, _)| v);
    let list = state.api.get(&format!("/v1/addons/{}/versions", seg(&slug)), None).await?;
    let verified_author = a["verified_author"] == true;
    let rows = list.as_array().into_iter().flatten().map(|v| version_row(v, verified_author)).collect::<Vec<_>>();
    let rows = rows.into_iter().map(|r| (download_href(&slug, &r), r)).collect();
    Ok(render(&VersionsPage { user, csrf: csrf.0, slug, name: s(&a["name"]), rows, perms }))
}

async fn version_data(state: &AppState, slug: &str, tag: &str) -> WebResult<(Value, VersionRow)> {
    let a = state.api.get(&format!("/v1/addons/{}", seg(slug)), None).await?;
    if a["blocked"] == true {
        return Err(WebError::NotFound);
    }
    let list = state.api.get(&format!("/v1/addons/{}/versions", seg(slug)), None).await?;
    let v = list.as_array().into_iter().flatten().find(|v| v["tag"] == tag).ok_or(WebError::NotFound)?;
    let row = version_row(v, a["verified_author"] == true);
    Ok((a, row))
}

pub async fn version(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, tag)): Path<(String, String)>,
) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let (a, row) = version_data(&state, &slug, &tag).await?;
    let found = findings(&state, &slug, &tag).await?;
    Ok(render(&VersionPage { user, csrf: csrf.0, href: download_href(&slug, &row), slug, name: s(&a["name"]), row, findings: found }))
}

pub async fn download(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path((slug, tag)): Path<(String, String)>,
    Query(query): Query<DownloadQuery>,
) -> WebResult<Response> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let (a, row) = version_data(&state, &slug, &tag).await?;
    if !row.notice.downloadable {
        return Ok(error_page(StatusCode::FORBIDDEN, "Not downloadable", row.notice.text));
    }
    if !row.notice.confirm || query.confirmed.is_some() {
        count_download(&state, &slug).await;
        return Ok(Redirect::to(&row.asset_url).into_response());
    }
    let found = findings(&state, &slug, &tag).await?;
    Ok(render(&DownloadPage { user, csrf: csrf.0, slug, name: s(&a["name"]), row, findings: found }).into_response())
}
