//! The API talks to GitHub as a GitHub App: App JWT for `/app` and `/repos/.../installation`,
//! installation tokens (5000 requests/h per installation) for everything else. Repos without the
//! App (imported addons nobody claimed yet) are read without a token, like the launcher does.
//!
//! `GitHubApi` is the seam for tests; `GitHubClient` is the real thing.

pub mod app_auth;
pub mod types;

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use jsonwebtoken::EncodingKey;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH, USER_AGENT};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use types::{Installation, InstallationToken, Owner, Permission, Release, Repo, Tag, User};

#[derive(Debug, Clone, PartialEq)]
pub enum GitHubError {
    NotFound,
    RateLimited { reset_at: Option<DateTime<Utc>> },
    TooLarge,
    Http(u16),
    Network(String),
    Parse(String),
    Auth(String),
    /// A download URL or redirect outside `DOWNLOAD_HOSTS`.
    ForbiddenHost(String),
}

/// The only hosts addon files are downloaded from, redirects included.
pub const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// `url` is https (or http for a listed test host) on one of `hosts`.
pub fn download_allowed(url: &str, hosts: &[String]) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        let host = u.host_str().unwrap_or_default();
        hosts.iter().any(|h| h == host) && (u.scheme() == "https" || !DOWNLOAD_HOSTS.contains(&host))
    })
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubError::NotFound => write!(f, "not found on GitHub"),
            GitHubError::RateLimited { reset_at } => write!(f, "GitHub rate limit reached (reset {reset_at:?})"),
            GitHubError::TooLarge => write!(f, "the download is larger than allowed"),
            GitHubError::Http(code) => write!(f, "GitHub answered with {code}"),
            GitHubError::Network(e) => write!(f, "cannot reach GitHub: {e}"),
            GitHubError::Parse(e) => write!(f, "unexpected answer from GitHub: {e}"),
            GitHubError::Auth(e) => write!(f, "GitHub App authentication failed: {e}"),
            GitHubError::ForbiddenHost(url) => write!(f, "downloads from {url} are not allowed"),
        }
    }
}

pub type GitHubResult<T> = Result<T, GitHubError>;

#[async_trait]
pub trait GitHubApi: Send + Sync {
    /// The installation of the App on `owner/name`; `None` when the App is not installed there.
    async fn installation_for_repo(&self, full_name: &str) -> GitHubResult<Option<i64>>;
    /// The public repos of an account, newest push first; no App installation needed.
    async fn user_repos(&self, login: &str) -> GitHubResult<Vec<Repo>>;
    /// The organizations the user is a public member of.
    async fn user_orgs(&self, login: &str) -> GitHubResult<Vec<String>>;
    async fn repo(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Repo>;
    async fn repo_by_name(&self, installation: Option<i64>, full_name: &str) -> GitHubResult<Repo>;
    /// `admin`, `maintain`, `write`, `triage`, `read` or `none`.
    async fn user_permission(&self, installation: i64, repo_id: i64, login: &str) -> GitHubResult<String>;
    async fn user_by_login(&self, installation: Option<i64>, login: &str) -> GitHubResult<Option<User>>;
    /// Newest first.
    async fn releases(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Release>>;
    async fn release(&self, installation: Option<i64>, repo_id: i64, release_id: i64) -> GitHubResult<Option<Release>>;
    /// Newest first.
    async fn tags(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Tag>>;
    /// Finishes a user's OAuth login: trades the code for a user token, reads the user with it and
    /// drops the token.
    async fn oauth_user(&self, code: &str, verifier: &str, redirect_uri: &str) -> GitHubResult<User>;
    /// Reads at most `max_bytes`; more is `TooLarge`, whatever `Content-Length` says.
    async fn download(&self, url: &str, max_bytes: u64) -> GitHubResult<Vec<u8>>;
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct GitHubClient {
    http: reqwest::Client,
    /// Follows redirects only to `download_hosts`.
    downloads: reqwest::Client,
    download_hosts: Vec<String>,
    api_url: String,
    app_id: String,
    key: EncodingKey,
    clock: Clock,
    tokens: Mutex<HashMap<i64, InstallationToken>>,
    /// URL -> (ETag, body) of the last `200`, for conditional requests.
    etags: Mutex<HashMap<String, (String, String)>>,
    oauth: Option<OAuthApp>,
    /// Sent with requests outside an installation: 5000 instead of 60 requests per hour.
    read_token: Option<String>,
}

/// The OAuth side of the GitHub App, for user logins.
pub struct OAuthApp {
    pub oauth_url: String,
    pub client_id: String,
    pub client_secret: String,
}

/// Installation tokens are renewed this long before they expire.
const TOKEN_MARGIN: Duration = Duration::minutes(5);

impl GitHubClient {
    pub fn new(api_url: &str, app_id: &str, key: EncodingKey, clock: Clock) -> GitHubClient {
        let download_hosts: Vec<String> = DOWNLOAD_HOSTS.iter().map(|h| h.to_string()).collect();
        GitHubClient {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("HTTP client"),
            downloads: download_client(download_hosts.clone()),
            download_hosts,
            api_url: api_url.trim_end_matches('/').to_string(),
            app_id: app_id.to_string(),
            key,
            clock,
            tokens: Mutex::new(HashMap::new()),
            etags: Mutex::new(HashMap::new()),
            oauth: None,
            read_token: None,
        }
    }

    /// Other download hosts than GitHub's, for tests against a local server.
    /// A token for public reads outside an installation (`GITHUB_READ_TOKEN`).
    pub fn with_read_token(mut self, token: Option<String>) -> GitHubClient {
        self.read_token = token.filter(|t| !t.trim().is_empty());
        self
    }

    pub fn with_download_hosts(mut self, hosts: &[&str]) -> GitHubClient {
        self.download_hosts = hosts.iter().map(|h| h.to_string()).collect();
        self.downloads = download_client(self.download_hosts.clone());
        self
    }

    pub fn with_oauth(mut self, oauth: OAuthApp) -> GitHubClient {
        self.oauth = Some(oauth);
        self
    }

    fn app_jwt(&self) -> GitHubResult<String> {
        app_auth::app_jwt(&self.app_id, &self.key, (self.clock)()).map_err(|e| GitHubError::Auth(e.to_string()))
    }

    async fn installation_token(&self, installation: i64) -> GitHubResult<String> {
        let now = (self.clock)();
        if let Some(token) = self.tokens.lock().unwrap().get(&installation)
            && token.expires_at - TOKEN_MARGIN > now
        {
            return Ok(token.token.clone());
        }
        let url = format!("{}/app/installations/{installation}/access_tokens", self.api_url);
        let response = self.request(self.http.post(&url), &self.app_jwt()?).send().await.map_err(network)?;
        let token: InstallationToken = parse(check(response).await?).await?;
        self.tokens.lock().unwrap().insert(installation, token.clone());
        Ok(token.token)
    }

    fn request(&self, builder: reqwest::RequestBuilder, bearer: &str) -> reqwest::RequestBuilder {
        builder
            .bearer_auth(bearer)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(USER_AGENT, "SurvivorsForge")
    }

    /// GET with an installation token and `If-None-Match`; a `304` answers from the cache.
    // Without an installation GitHub allows 60 requests per hour and IP, or 5000 with `read_token`.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, installation: Option<i64>, path: &str) -> GitHubResult<T> {
        let url = format!("{}{path}", self.api_url);
        let mut builder = match installation {
            Some(installation) => self.request(self.http.get(&url), &self.installation_token(installation).await?),
            None => {
                let builder = self
                    .http
                    .get(&url)
                    .header(ACCEPT, "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .header(USER_AGENT, "SurvivorsForge");
                match &self.read_token {
                    Some(token) => builder.bearer_auth(token),
                    None => builder,
                }
            }
        };
        let cached = self.etags.lock().unwrap().get(&url).cloned();
        if let Some((etag, _)) = &cached {
            builder = builder.header(IF_NONE_MATCH, etag);
        }
        let response = builder.send().await.map_err(network)?;
        let body = if let Some((_, body)) = cached.filter(|_| response.status() == StatusCode::NOT_MODIFIED) {
            body
        } else {
            let response = check(response).await?;
            let etag = response.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
            let body = response.text().await.map_err(network)?;
            if let Some(etag) = etag {
                self.etags.lock().unwrap().insert(url, (etag, body.clone()));
            }
            body
        };
        serde_json::from_str(&body).map_err(|e| GitHubError::Parse(e.to_string()))
    }
}

fn download_client(hosts: Vec<String>) -> reqwest::Client {
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if download_allowed(attempt.url().as_str(), &hosts) {
            attempt.follow()
        } else {
            let url = attempt.url().to_string();
            attempt.error(format!("redirect to a forbidden host: {url}"))
        }
    });
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(policy)
        .build()
        .expect("download client")
}

fn network(e: reqwest::Error) -> GitHubError {
    if e.is_redirect() {
        return GitHubError::ForbiddenHost(e.url().map(|u| u.to_string()).unwrap_or_default());
    }
    GitHubError::Network(e.to_string())
}

async fn parse<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> GitHubResult<T> {
    response.json().await.map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Maps error statuses; a `403`/`429` with no requests left is a rate limit.
async fn check(response: reqwest::Response) -> GitHubResult<reqwest::Response> {
    let remaining = response.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()?.parse::<f64>().ok());
    if let Some(remaining) = remaining {
        metrics::gauge!("forge_github_rate_remaining").set(remaining);
    }
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(status_error(status, response.headers()))
}

fn status_error(status: StatusCode, headers: &HeaderMap) -> GitHubError {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let exhausted = header("x-ratelimit-remaining") == Some("0");
    match status {
        StatusCode::NOT_FOUND => GitHubError::NotFound,
        StatusCode::TOO_MANY_REQUESTS => GitHubError::RateLimited { reset_at: reset_at(header("x-ratelimit-reset")) },
        StatusCode::FORBIDDEN if exhausted => GitHubError::RateLimited { reset_at: reset_at(header("x-ratelimit-reset")) },
        other => GitHubError::Http(other.as_u16()),
    }
}

fn reset_at(epoch: Option<&str>) -> Option<DateTime<Utc>> {
    epoch?.parse::<i64>().ok().and_then(|s| Utc.timestamp_opt(s, 0).single())
}

fn optional<T>(result: GitHubResult<T>) -> GitHubResult<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(GitHubError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

#[async_trait]
impl GitHubApi for GitHubClient {
    async fn installation_for_repo(&self, full_name: &str) -> GitHubResult<Option<i64>> {
        let url = format!("{}/repos/{full_name}/installation", self.api_url);
        let response = self.request(self.http.get(&url), &self.app_jwt()?).send().await.map_err(network)?;
        let installation: GitHubResult<Installation> = match check(response).await {
            Ok(response) => parse(response).await,
            Err(e) => Err(e),
        };
        Ok(optional(installation)?.map(|i| i.id))
    }

    async fn user_orgs(&self, login: &str) -> GitHubResult<Vec<String>> {
        let orgs: Vec<Owner> = self.get_json(None, &format!("/users/{login}/orgs?per_page=100")).await?;
        Ok(orgs.into_iter().map(|o| o.login).collect())
    }

    async fn user_repos(&self, login: &str) -> GitHubResult<Vec<Repo>> {
        // The 100 newest repos: publishing forms look for active addons.
        self.get_json(None, &format!("/users/{login}/repos?per_page=100&sort=pushed")).await
    }

    async fn repo(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Repo> {
        self.get_json(installation, &format!("/repositories/{repo_id}")).await
    }

    async fn repo_by_name(&self, installation: Option<i64>, full_name: &str) -> GitHubResult<Repo> {
        self.get_json(installation, &format!("/repos/{full_name}")).await
    }

    async fn user_permission(&self, installation: i64, repo_id: i64, login: &str) -> GitHubResult<String> {
        let path = format!("/repositories/{repo_id}/collaborators/{login}/permission");
        match self.get_json::<Permission>(Some(installation), &path).await {
            Ok(p) => Ok(p.permission),
            Err(GitHubError::NotFound) => Ok("none".into()),
            Err(e) => Err(e),
        }
    }

    async fn user_by_login(&self, installation: Option<i64>, login: &str) -> GitHubResult<Option<User>> {
        optional(self.get_json(installation, &format!("/users/{login}")).await)
    }

    async fn releases(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Release>> {
        // Page by page: a release missing from the list counts as deleted on GitHub.
        const PAGE: usize = 100;
        let mut all = Vec::new();
        for page in 1..=20 {
            let batch: Vec<Release> = self.get_json(installation, &format!("/repositories/{repo_id}/releases?per_page={PAGE}&page={page}")).await?;
            let full = batch.len() == PAGE;
            all.extend(batch);
            if !full {
                break;
            }
        }
        Ok(all)
    }

    async fn release(&self, installation: Option<i64>, repo_id: i64, release_id: i64) -> GitHubResult<Option<Release>> {
        optional(self.get_json(installation, &format!("/repositories/{repo_id}/releases/{release_id}")).await)
    }

    async fn tags(&self, installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Tag>> {
        self.get_json(installation, &format!("/repositories/{repo_id}/tags?per_page=100")).await
    }

    async fn oauth_user(&self, code: &str, verifier: &str, redirect_uri: &str) -> GitHubResult<User> {
        let oauth = self.oauth.as_ref().ok_or_else(|| GitHubError::Auth("OAuth is not configured".into()))?;
        let url = format!("{}/login/oauth/access_token", oauth.oauth_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "client_id": oauth.client_id,
            "client_secret": oauth.client_secret,
            "code": code,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        });
        let request = self.http.post(&url).header(ACCEPT, "application/json").header(USER_AGENT, "SurvivorsForge").json(&body);
        let answer: serde_json::Value = parse(check(request.send().await.map_err(network)?).await?).await?;
        let token = answer["access_token"]
            .as_str()
            .ok_or_else(|| GitHubError::Auth(answer["error"].as_str().unwrap_or("no access token").to_string()))?;
        let response = self.request(self.http.get(format!("{}/user", self.api_url)), token).send().await.map_err(network)?;
        parse(check(response).await?).await
    }

    async fn download(&self, url: &str, max_bytes: u64) -> GitHubResult<Vec<u8>> {
        if !download_allowed(url, &self.download_hosts) {
            return Err(GitHubError::ForbiddenHost(url.to_string()));
        }
        let response = self.downloads.get(url).header(USER_AGENT, "SurvivorsForge").send().await.map_err(network)?;
        let mut response = check(response).await?;
        if response.content_length().is_some_and(|len| len > max_bytes) {
            return Err(GitHubError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(GitHubError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
