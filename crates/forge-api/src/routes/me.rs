use crate::app::AppState;
use crate::auth::{CurrentUser, bearer, hash};
use crate::error::{ApiError, ApiResult};
use crate::github::GitHubError;
use serde::Deserialize;
use crate::scan_job::MAX_DOWNLOAD;
use crate::suggest;
use serde_json::{Value, json};
use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;

/// The logged-in user and what they may do (`permissions`, from `forge_core::roles`).
pub async fn me(State(state): State<AppState>, user: CurrentUser) -> Json<Value> {
    let permissions = forge_core::roles::permissions(user.role(), &state.config.stats_roles);
    let mut me = serde_json::to_value(&user).unwrap_or_default();
    me["permissions"] = json!(permissions);
    Json(me)
}

fn github_down(e: crate::github::GitHubError) -> ApiError {
    tracing::warn!(error = %e, "GitHub request failed");
    ApiError::new(StatusCode::BAD_GATEWAY, "github_unavailable", "GitHub cannot be reached right now")
}

#[derive(Deserialize, Default)]
pub struct RepoSearch {
    owner: Option<String>,
    #[serde(default)]
    q: String,
}

/// The user's own account and the organizations they are a public member of.
async fn accounts_of(state: &AppState, user: &CurrentUser) -> ApiResult<Vec<String>> {
    let mut accounts = vec![user.login.clone()];
    accounts.extend(state.github.user_orgs(&user.login).await.map_err(github_down)?);
    Ok(accounts)
}

/// `GET /v1/me/accounts`: the accounts whose repos the user may publish from the form.
pub async fn accounts(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    Ok(Json(json!(accounts_of(&state, &user).await?)))
}

/// The account whose repos the publish form offers: one of the user's accounts; admins may name any.
async fn repo_owner(state: &AppState, user: &CurrentUser, wanted: Option<&str>) -> ApiResult<String> {
    let Some(owner) = wanted.map(str::trim).filter(|o| !o.is_empty()) else { return Ok(user.login.clone()) };
    if owner.len() > 39 || !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_owner", "Not a GitHub account name"));
    }
    let mine = owner.eq_ignore_ascii_case(&user.login) || user.is_staff() || accounts_of(state, user).await?.iter().any(|a| a.eq_ignore_ascii_case(owner));
    if !mine {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not_your_account", "You can publish repos of your account and your organizations only"));
    }
    Ok(owner.to_string())
}

/// `GET /v1/me/repos?owner=&q=`: up to ten public repos of the account whose name contains `q`,
/// without the ones that already have an addon. Read from GitHub's public list, so the App need
/// not be installed yet; publishing still needs it.
pub async fn repos(State(state): State<AppState>, user: CurrentUser, Query(search): Query<RepoSearch>) -> ApiResult<Json<Value>> {
    let owner = repo_owner(&state, &user, search.owner.as_deref()).await?;
    let repos = match state.github.user_repos(&owner).await {
        Ok(repos) => repos,
        Err(GitHubError::NotFound) => Vec::new(),
        Err(e) => return Err(github_down(e)),
    };
    let ids: Vec<i64> = repos.iter().map(|r| r.id).collect();
    let taken: Vec<i64> =
        sqlx::query_scalar("SELECT repo_id FROM addons WHERE repo_id = ANY($1) AND deleted_at IS NULL").bind(&ids).fetch_all(&state.pool).await?;
    let q = search.q.trim().to_lowercase();
    let name = |r: &crate::github::types::Repo| r.full_name.rsplit('/').next().unwrap_or_default().to_lowercase();
    let list: Vec<Value> = repos
        .iter()
        .filter(|r| !r.private && !taken.contains(&r.id) && name(r).contains(&q))
        .take(10)
        .map(|r| json!({ "full_name": r.full_name, "description": r.description }))
        .collect();
    Ok(Json(json!({ "owner": owner, "repos": list })))
}

/// `GET /v1/me/repos/{owner}/{repo}/suggestion`: the publish form filled in from the newest
/// release with a zip, else the newest tag, all read from public GitHub data.
pub async fn suggestion(State(state): State<AppState>, user: CurrentUser, Path((owner, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    repo_owner(&state, &user, Some(&owner)).await?;
    let repo = match state.github.repo_by_name(None, &format!("{owner}/{name}")).await {
        Ok(repo) if !repo.private => repo,
        Ok(_) | Err(GitHubError::NotFound) => return Err(ApiError::not_found()),
        Err(e) => return Err(github_down(e)),
    };
    let releases = state.github.releases(None, repo.id).await.map_err(github_down)?;
    let (source_type, pattern, url, read_from) = match suggest::pick_release(&releases) {
        Some((release, asset)) => (
            "release",
            suggest::asset_pattern(&asset.name, &release.tag_name),
            Some(asset.browser_download_url.clone()),
            format!("release {}, {}", release.tag_name, asset.name),
        ),
        None => match suggest::newest_tag(&state.github.tags(None, repo.id).await.map_err(github_down)?) {
            Some(tag) => ("tag", String::new(), Some(format!("https://codeload.github.com/{}/zip/{}", repo.full_name, tag.name)), format!("tag {}", tag.name)),
            None => ("release", String::new(), None, "no release or tag yet".to_string()),
        },
    };
    // Without the zip the form still gets the names; the folders are then left to the author.
    let zip = match url {
        Some(url) => state.github.download(&url, MAX_DOWNLOAD).await.ok(),
        None => None,
    };
    let mut answer = serde_json::to_value(suggest::suggest(&repo, source_type, &pattern, zip.as_deref(), read_from)).map_err(|_| ApiError::internal())?;
    answer["repo"] = json!(repo.full_name);
    Ok(Json(answer))
}

/// Ends the session of the token in the request.
pub async fn logout(State(state): State<AppState>, request: Request) -> ApiResult<StatusCode> {
    let (mut parts, _) = request.into_parts();
    CurrentUser::from_request_parts(&mut parts, &state).await?;
    if let Some(token) = bearer(&parts) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(hash(token)).execute(&state.pool).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
