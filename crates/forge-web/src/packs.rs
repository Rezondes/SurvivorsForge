//! Addon packs: pick several addons, get one zip and a link to share. For players of the browser
//! version, who upload a zip of addons into the game.

use crate::api_client::ApiError;
use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::templates::{NewPackPage, PackBuildingPage, PackChoice, PackEditPage, PackEntry, PackListRow, PackPage, PacksPage, Visitor, render};
use crate::trust::notice;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Extension;
use axum_extra::extract::PrivateCookieJar;
use serde_json::{Value, json};

/// Trust levels the API puts into packs (`forge_api::routes::packs::PACKABLE`).
const PACKABLE: &[&str] = &["scanned", "verified"];

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn megabytes(bytes: i64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// `2026-10-29T10:00:00Z` as `29 October 2026`; anything else as it came.
fn long_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let parts: Vec<&str> = iso.get(..10).unwrap_or(iso).split('-').collect();
    match parts[..] {
        [y, m, d] => match (m.parse::<usize>(), d.parse::<u32>()) {
            (Ok(m @ 1..=12), Ok(d)) => format!("{d} {} {y}", MONTHS[m - 1]),
            _ => iso.into(),
        },
        _ => iso.into(),
    }
}

/// Every addon whose current version can go into a pack, ticked when in `picked`.
async fn choices(state: &AppState, picked: &[String]) -> WebResult<Vec<PackChoice>> {
    let registry = state.api.get("/v1/registry", None).await?;
    let mut choices: Vec<PackChoice> = registry["addons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| PACKABLE.contains(&a["latest"]["trust"].as_str().unwrap_or_default()) && a["latest"]["sha256"].is_string())
        .map(|a| {
            let slug = s(&a["id"]);
            PackChoice {
                checked: picked.iter().any(|p| p.eq_ignore_ascii_case(&slug)),
                name: s(&a["name"]),
                tag: s(&a["latest"]["tag"]),
                size: a["latest"]["size"].as_i64().map(megabytes).unwrap_or_default(),
                notice: notice(a["latest"]["trust"].as_str().unwrap_or_default(), a["verified_author"] == true, false),
                slug,
            }
        })
        .collect();
    choices.sort_by_key(|c| c.name.to_lowercase());
    Ok(choices)
}

/// Every value of `field` in a query string or form body.
fn values(raw: Option<&str>, field: &str) -> Vec<String> {
    raw.and_then(|q| reqwest::Url::parse(&format!("http://x/?{q}")).ok())
        .map(|u| u.query_pairs().filter(|(k, _)| k == field).map(|(_, v)| v.into_owned()).collect())
        .unwrap_or_default()
}

/// `GET /packs/new?addon=a&addon=b`: the picker, with the given addons ticked.
pub async fn new_pack(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, RawQuery(raw): RawQuery) -> WebResult<Html<String>> {
    let (user, _) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let choices = choices(&state, &values(raw.as_deref(), "addon")).await?;
    Ok(render(&NewPackPage { user: Some(user), csrf: csrf.0, name: String::new(), choices, error: None }))
}

/// `POST /packs/new`: the ticked boxes arrive as repeated `addon` fields.
pub async fn create_pack(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, body: String) -> WebResult<Response> {
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let picked = values(Some(&body), "addon");
    let name = values(Some(&body), "name").pop().unwrap_or_default();
    let listed = !values(Some(&body), "listed").is_empty();
    let latest = !values(Some(&body), "latest").is_empty();
    let addons: Vec<Value> = picked.iter().map(|slug| json!({ "slug": slug, "latest": latest })).collect();
    match state.api.post("/v1/packs", Some(&token), &json!({ "name": name.trim(), "listed": listed, "addons": addons })).await {
        Ok(created) => Ok(Redirect::to(&format!("/packs/{}", s(&created["id"]))).into_response()),
        Err(ApiError::Api { status, message, .. }) if status == 422 || status == 429 => {
            let choices = choices(&state, &picked).await?;
            let page = NewPackPage { user: Some(user), csrf: csrf.0, name, choices, error: Some(message) };
            Ok((axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::BAD_REQUEST), render(&page)).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// `GET /packs?page=`: the packs their authors chose to show, newest first.
pub async fn packs(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, RawQuery(raw): RawQuery) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let page: i64 = values(raw.as_deref(), "page").pop().and_then(|p| p.parse().ok()).unwrap_or(1).max(1);
    let q = values(raw.as_deref(), "q").pop().unwrap_or_default().trim().to_string();
    // The search and a page as a query string, for the API and the pager.
    let query = |page: i64| {
        let mut pairs = vec![("page", page.to_string())];
        if !q.is_empty() {
            pairs.insert(0, ("q", q.clone()));
        }
        reqwest::Url::parse_with_params("http://x/", &pairs).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default()
    };
    let list = state.api.get(&format!("/v1/packs?{}", query(page)), None).await?;
    let rows = list["packs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| PackListRow {
            id: s(&p["id"]),
            name: Some(s(&p["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| "Addon pack".into()),
            created_by: s(&p["created_by"]),
            created: s(&p["created_at"]).chars().take(10).collect(),
            addons: p["addons"].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join(", "),
        })
        .collect();
    let total = list["total"].as_i64().unwrap_or(0);
    let per_page = list["per_page"].as_i64().unwrap_or(20).max(1);
    Ok(render(&PacksPage {
        user,
        csrf: csrf.0,
        rows,
        total,
        prev: (page > 1).then(|| format!("/packs?{}", query(page - 1))),
        next: (page * per_page < total).then(|| format!("/packs?{}", query(page + 1))),
        q,
    }))
}

/// Pack ids are 10 letters and digits; anything else never reaches the API.
fn valid_id(id: &str) -> WebResult<()> {
    if id.len() == 10 && id.bytes().all(|b| b.is_ascii_alphanumeric()) { Ok(()) } else { Err(WebError::NotFound) }
}

fn entries(p: &Value) -> Vec<PackEntry> {
    p["addons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| PackEntry {
            slug: s(&a["slug"]),
            name: s(&a["name"]),
            tag: s(&a["tag"]),
            available: a["available"] == true,
            notice: notice(a["trust"].as_str().unwrap_or_default(), false, false),
            folders: a["folders"].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join(", "),
            latest: a["latest"] == true,
            kept: a["kept"] != false,
            versions: a["versions"].as_array().into_iter().flatten().map(s).collect(),
        })
        .collect()
}

/// The creator of the pack or an admin; the API checks it again.
fn may_edit(user: Option<&Visitor>, p: &Value) -> bool {
    user.is_some_and(|u| u.staff || (!u.login.is_empty() && u.login.eq_ignore_ascii_case(p["created_by"].as_str().unwrap_or_default())))
}

/// `GET /packs/{id}?msg=`: what is in the pack, the link to share and the download.
pub async fn pack(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> WebResult<Html<String>> {
    valid_id(&id)?;
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let p = state.api.get(&format!("/v1/packs/{id}"), None).await?;
    let entries = entries(&p);
    let kept: Vec<String> = entries.iter().filter(|e| e.available).map(|e| format!("addon={}", e.slug)).collect();
    let rebuild = if kept.is_empty() { "/packs/new".into() } else { format!("/packs/new?{}", kept.join("&")) };
    // A pack without a name is called after its addons, so several open packs can be told apart.
    let name = match (s(&p["name"]), entries.as_slice()) {
        (name, _) if !name.is_empty() => name,
        (_, [only]) => only.name.clone(),
        (_, [first, rest @ ..]) => format!("{} and {} more", first.name, rest.len()),
        (_, []) => String::new(),
    };
    Ok(render(&PackPage {
        rebuild,
        can_edit: may_edit(user.as_ref(), &p),
        msg: values(raw.as_deref(), "msg").pop(),
        user,
        csrf: csrf.0,
        link: format!("{}/packs/{id}", state.config.public_web_url),
        name,
        created_by: s(&p["created_by"]),
        created: long_date(&s(&p["created_at"])),
        available: p["available"] == true,
        size: p["file"]["size"].as_i64().map(megabytes),
        entries,
        id,
    }))
}

/// The edit page of pack `id` as the API's `p` describes it; `added` are ticked among the others.
async fn edit_page(state: &AppState, user: Visitor, csrf: String, id: String, p: &Value, added: &[String], error: Option<String>) -> WebResult<PackEditPage> {
    let entries = entries(p);
    let mut others = choices(state, added).await?;
    others.retain(|c| !entries.iter().any(|e| e.slug.eq_ignore_ascii_case(&c.slug)));
    Ok(PackEditPage { user: Some(user), csrf, id, name: s(&p["name"]), listed: p["listed"] == true, entries, others, error })
}

/// `GET /packs/{id}/edit`: its creator renames the pack, picks versions and adds or removes addons.
pub async fn edit_pack(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(id): Path<String>) -> WebResult<Html<String>> {
    valid_id(&id)?;
    let (user, _) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let p = state.api.get(&format!("/v1/packs/{id}"), None).await?;
    if !may_edit(Some(&user), &p) {
        return Err(WebError::Forbidden);
    }
    Ok(render(&edit_page(&state, user, csrf.0, id, &p, &[], None).await?))
}

/// `POST /packs/{id}/edit`: `keep` lists the addons that stay, each with its `version.<slug>`
/// (`latest` or a tag); `addon` adds others on their newest version.
pub async fn save_pack(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>, Path(id): Path<String>, body: String) -> WebResult<Response> {
    valid_id(&id)?;
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    let field = |name: &str| values(Some(&body), name);
    let mut addons: Vec<Value> = field("keep")
        .iter()
        .map(|slug| match field(&format!("version.{slug}")).pop().filter(|v| v != "latest") {
            Some(tag) => json!({ "slug": slug, "tag": tag }),
            None => json!({ "slug": slug, "latest": true }),
        })
        .collect();
    addons.extend(field("addon").iter().map(|slug| json!({ "slug": slug, "latest": true })));
    let change = json!({ "name": field("name").pop().unwrap_or_default().trim(), "listed": !field("listed").is_empty(), "addons": addons });
    match state.api.call(reqwest::Method::PATCH, &format!("/v1/packs/{id}"), Some(&token), Some(&change)).await {
        Ok(_) => {
            let msg = "The pack is saved. Its link stays the same; the next download gets the new content.";
            let query = reqwest::Url::parse_with_params("http://x/", &[("msg", msg)]).map(|u| u.query().unwrap_or_default().to_string()).unwrap_or_default();
            Ok(Redirect::to(&format!("/packs/{id}?{query}")).into_response())
        }
        Err(ApiError::Api { status, message, .. }) if status == 422 || status == 403 => {
            // The pack as it is, with what the visitor entered on top, so nothing is lost.
            let mut p = state.api.get(&format!("/v1/packs/{id}"), None).await?;
            p["name"] = change["name"].clone();
            p["listed"] = change["listed"].clone();
            let kept = field("keep");
            for a in p["addons"].as_array_mut().into_iter().flatten() {
                let slug = s(&a["slug"]);
                a["kept"] = json!(kept.contains(&slug));
                match field(&format!("version.{slug}")).pop() {
                    Some(v) if v == "latest" => a["latest"] = json!(true),
                    Some(tag) => (a["latest"], a["tag"]) = (json!(false), json!(tag)),
                    None => {}
                }
            }
            let page = edit_page(&state, user, csrf.0, id, &p, &field("addon"), Some(message)).await?;
            Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), render(&page)).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// How often the building page asks again (every 3 seconds) before it stops: about two minutes.
const MAX_TRIES: u32 = 40;

/// `GET /packs/{id}/download?try=`: to the zip when it is built, else a page that asks again, until
/// the build fails or takes longer than `MAX_TRIES`.
pub async fn download(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> WebResult<Response> {
    valid_id(&id)?;
    let tries: u32 = values(raw.as_deref(), "try").first().and_then(|t| t.parse().ok()).unwrap_or(0);
    let (status, failed) = match state.api.get_or_redirect(&format!("/v1/packs/{id}/download")).await {
        Ok(Err(location)) => return Ok(Redirect::to(&location).into_response()),
        Ok(Ok(_)) => (StatusCode::OK, None),
        Err(ApiError::Api { code, message, .. }) if code == "build_failed" => (StatusCode::UNPROCESSABLE_ENTITY, Some(message)),
        Err(e) => return Err(e.into()),
    };
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let next_try = (failed.is_none() && tries < MAX_TRIES).then_some(tries + 1);
    Ok((status, render(&PackBuildingPage { user, csrf: csrf.0, id, failed, next_try })).into_response())
}

#[cfg(test)]
mod tests {
    use super::long_date;

    #[test]
    fn dates_in_words() {
        assert_eq!(long_date("2026-10-29T10:00:00Z"), "29 October 2026");
        assert_eq!(long_date("2026-01-05"), "5 January 2026");
        assert_eq!(long_date("2026-13-05"), "2026-13-05", "no such month");
        assert_eq!(long_date(""), "");
    }
}
