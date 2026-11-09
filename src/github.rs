//! What the launcher needs from GitHub: release and tag metadata comes from forge-api
//! (`ApiGitHub`), or from GitHub itself while the API cannot be reached: its REST API with the
//! user's token when logged in (`TokenGitHub`), else its web pages (`WebGitHub`). The zip files are
//! always downloaded from GitHub directly.

pub use forge_core::github_web::*;

use crate::{Error, Result};
use std::io::Read;
use std::path::Path;
use std::collections::HashMap;
use std::time::Duration;

/// What the update and detection logic needs from GitHub. `Client` is the real one; tests use fakes.
pub trait GitHub {
    /// `None` when the repo has no release.
    fn latest_release(&self, repo: &str) -> Result<Option<Release>>;
    /// The release of `tag`, also a pre-release; no assets when the tag has no release.
    fn release(&self, repo: &str, tag: &str) -> Result<Release>;
    /// Newest first.
    fn tags(&self, repo: &str) -> Result<Vec<Tag>>;
    fn default_branch(&self, repo: &str) -> Result<String>;
    fn commit_sha(&self, repo: &str, git_ref: &str) -> Result<String>;
}

/// Accepts `owner/repo` and GitHub URLs (`.git`, `/tree/...`, trailing slash), returns `owner/repo`.
pub fn normalize_repo(input: &str) -> Result<String> {
    let invalid = || Error::InvalidRepo(input.trim().to_string());
    let mut s = input.trim();
    for prefix in ["https://", "http://"] {
        s = s.strip_prefix(prefix).unwrap_or(s);
    }
    s = s.strip_prefix("www.").unwrap_or(s);
    let had_host = s.len() != input.trim().len() || s.starts_with("github.com/");
    if let Some(rest) = s.strip_prefix("github.com/") {
        s = rest;
    } else if had_host {
        return Err(invalid());
    }
    let mut parts = s.split('/').filter(|p| !p.is_empty());
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else { return Err(invalid()) };
    if !had_host && parts.next().is_some() {
        return Err(invalid());
    }
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let valid = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid(owner) || !valid(repo) {
        return Err(invalid());
    }
    Ok(format!("{owner}/{repo}"))
}

pub fn map_error(status: u16) -> Error {
    match status {
        // github.com answers 401 (asks for login) for repos that don't exist or are private.
        401 | 404 => Error::NotFound,
        429 => Error::RateLimited,
        _ => Error::Http(status),
    }
}

/// The platform's error answer `{"error": code, "message": text}`; a body without them keeps the status.
pub fn api_error(status: u16, body: &str) -> Error {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let field = |k: &str| json[k].as_str().unwrap_or_default().to_string();
    let message = Some(field("message")).filter(|m| !m.is_empty()).unwrap_or_else(|| format!("The server answered with error {status}"));
    Error::Api { status, code: field("error"), message }
}

pub fn source_zip_url(repo: &str, git_ref: &str) -> String {
    format!("https://codeload.github.com/{repo}/zip/{git_ref}")
}

/// The only hosts zips are downloaded from: GitHub's release and archive downloads, and its REST
/// API for release assets (`TokenGitHub`).
pub const DOWNLOAD_HOSTS: &[&str] =
    &["github.com", "codeload.github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com", "api.github.com"];

/// GitHub's REST API.
pub const GITHUB_API: &str = "https://api.github.com";

/// The headers one hop of a download sends: the user's GitHub token only to GitHub's REST API,
/// never to a host it redirects to.
pub fn download_headers(url: &str, github_token: Option<&str>) -> Vec<(&'static str, String)> {
    match github_token {
        Some(token) if url.starts_with(&format!("{GITHUB_API}/")) => {
            vec![("Authorization", format!("Bearer {token}")), ("Accept", "application/octet-stream".into())]
        }
        _ => Vec::new(),
    }
}

/// `url` is https on one of `DOWNLOAD_HOSTS`.
pub fn download_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    DOWNLOAD_HOSTS.contains(&host.as_str())
}

/// The next address of a download after a redirect to `location`: absolute, or relative to the
/// host of `current`. It must again be one of `DOWNLOAD_HOSTS`.
pub fn next_hop(current: &str, location: &str) -> Result<String> {
    let next = if location.starts_with('/') && !location.starts_with("//") {
        let host_end = current.find("://").map(|i| i + 3).and_then(|i| current[i..].find('/').map(|j| i + j)).unwrap_or(current.len());
        format!("{}{location}", &current[..host_end])
    } else {
        location.to_string()
    };
    if download_allowed(&next) { Ok(next) } else { Err(Error::ForbiddenHost(next)) }
}

/// `call`, once more when it failed on the way (a timeout, a dropped connection) before any
/// answer: GitHub's download servers sometimes do not answer one request. An answer of the server
/// (404, 429) is not retried.
pub fn retry_network<T>(mut call: impl FnMut() -> Result<T>) -> Result<T> {
    match call() {
        Err(Error::Network(_)) => call(),
        other => other,
    }
}

/// Redirects one download may take: GitHub sends one or two.
const MAX_REDIRECTS: usize = 5;

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    /// Sent as `X-SurvivorsForge-Install` with every platform request (`get_text`); downloads
    /// from GitHub never carry it.
    install_id: Option<String>,
    /// Follows no redirect itself, so every hop of a download is checked.
    downloads: ureq::Agent,
    /// The user's GitHub token, for release assets from GitHub's REST API (`download_headers`).
    github_token: Option<String>,
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

/// Header of the build token; only requests to the platform carry it.
pub const BUILD_HEADER: &str = "X-SurvivorsForge-Build";

/// The token the release build compiles in (`SURVIVORSFORGE_BUILD_TOKEN`, a secret of the release
/// workflow). The platform takes `SurvivorsForge/<version>` only with it (`forge-api`
/// request_stats::LauncherToken); every other build names itself `SurvivorsForge-unofficial`.
pub fn build_token() -> Option<&'static str> {
    option_env!("SURVIVORSFORGE_BUILD_TOKEN").filter(|t| !t.is_empty())
}

/// `request` to the platform with the build token of an official build.
pub fn with_build_token(request: ureq::Request) -> ureq::Request {
    match build_token() {
        Some(token) => request.set(BUILD_HEADER, token),
        None => request,
    }
}

/// The name a build without the token sends: `SURVIVORSFORGE_CLIENT_NAME` at build time, so a fork
/// names itself, else `SurvivorsForge-dev` for a debug build (the local platform) and
/// `SurvivorsForge-unofficial` for a release build, which the platform's public API refuses.
fn unofficial_name() -> &'static str {
    let fallback = if cfg!(debug_assertions) { "SurvivorsForge-dev" } else { "SurvivorsForge-unofficial" };
    option_env!("SURVIVORSFORGE_CLIENT_NAME").filter(|n| !n.is_empty()).unwrap_or(fallback)
}

/// `SurvivorsForge/<version> (<platform>)` for an official build: the platform counts the active
/// launchers by both (`forge-api` request_stats::launcher_build). Other builds send
/// `SurvivorsForge-unofficial/<version> (<platform>)`, so they never pose as the official launcher.
pub fn user_agent() -> String {
    let name = if build_token().is_some() { crate::APP_NAME } else { unofficial_name() };
    format!("{name}/{} ({})", crate::APP_VERSION, std::env::consts::OS)
}

impl Client {
    pub fn new() -> Client {
        let builder = || {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(60))
                .user_agent(&user_agent())
        };
        Client { agent: builder().build(), install_id: None, downloads: builder().redirects(0).build(), github_token: None }
    }

    /// The same client, downloading release assets from GitHub's REST API as the user.
    pub fn with_github_token(mut self, token: Option<String>) -> Client {
        self.github_token = token;
        self
    }

    /// One hop of a download, with the headers `download_headers` allows for it.
    fn download_hop(&self, url: &str) -> Result<ureq::Response> {
        retry_network(|| {
            let mut request = self.downloads.get(url);
            for (name, value) in download_headers(url, self.github_token.as_deref()) {
                request = request.set(name, &value);
            }
            Self::call(request)
        })
    }

    /// The same client, sending `install_id` to the platform (`telemetry::install_header`).
    pub fn with_install_id(mut self, install_id: Option<String>) -> Client {
        self.install_id = install_id;
        self
    }

    fn call(req: ureq::Request) -> Result<ureq::Response> {
        match req.call() {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(code, _)) => Err(map_error(code)),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// A platform request; only these carry the installation ID.
    pub fn get_text(&self, url: &str) -> Result<String> {
        self.get_text_within(url, None)
    }

    /// A small platform answer a page waits on (comments, the issues frame): it gives up after
    /// `controller::QUICK_TIMEOUT` instead of the minute a download may take.
    pub fn get_text_quick(&self, url: &str) -> Result<String> {
        self.get_text_within(url, Some(crate::controller::QUICK_TIMEOUT))
    }

    fn get_text_within(&self, url: &str, timeout: Option<Duration>) -> Result<String> {
        let mut request = self.agent.get(url);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        if let Some(id) = &self.install_id {
            request = request.set(crate::telemetry::INSTALL_HEADER, id);
        }
        Self::call(with_build_token(request))?.into_string().map_err(|e| Error::Network(e.to_string()))
    }

    /// A platform GET as the logged-in user (`bearer`).
    pub fn get_text_as(&self, url: &str, bearer: &str) -> Result<String> {
        self.platform_call(self.agent.get(url), bearer, &[], None)
    }

    /// A platform POST of JSON, as `bearer` when given, with `headers` on top (`x-github-token`).
    pub fn post_json(&self, url: &str, bearer: Option<&str>, headers: &[(&str, &str)], body: &serde_json::Value) -> Result<String> {
        self.platform_call(self.agent.post(url), bearer.unwrap_or_default(), headers, Some(body))
    }

    /// A platform PUT of JSON as `bearer`.
    pub fn put_json(&self, url: &str, bearer: &str, body: &serde_json::Value) -> Result<String> {
        self.platform_call(self.agent.put(url), bearer, &[], Some(body))
    }

    /// A platform DELETE as `bearer`.
    pub fn delete_as(&self, url: &str, bearer: &str) -> Result<String> {
        self.platform_call(self.agent.delete(url), bearer, &[], None)
    }

    fn platform_call(&self, mut request: ureq::Request, bearer: &str, headers: &[(&str, &str)], body: Option<&serde_json::Value>) -> Result<String> {
        if !bearer.is_empty() {
            request = request.set("Authorization", &format!("Bearer {bearer}"));
        }
        if let Some(id) = &self.install_id {
            request = request.set(crate::telemetry::INSTALL_HEADER, id);
        }
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let request = with_build_token(request);
        let answer = match body {
            Some(body) => request.set("Content-Type", "application/json").send_string(&body.to_string()),
            None => request.call(),
        };
        match answer {
            Ok(resp) => resp.into_string().map_err(|e| Error::Network(e.to_string())),
            Err(ureq::Error::Status(status, resp)) => Err(api_error(status, &resp.into_string().unwrap_or_default())),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// An image of the platform (icon, screenshot), at most `max` bytes; without the installation ID.
    pub fn get_image(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        self.get_file(url, max)
    }

    /// Any file of at most `max` bytes, without the installation ID.
    pub fn get_file(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        self.get_file_with(url, max, &mut |_| {})
    }

    /// `get_file`, telling `progress` the bytes of each piece as it arrives.
    pub fn get_file_with(&self, url: &str, max: u64, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(Error::Network(format!("not a web address: {url}")));
        }
        let mut bytes = Vec::new();
        let reader = Self::call(self.agent.get(url))?.into_reader();
        std::io::Read::read_to_end(&mut Counted { inner: std::io::Read::take(reader, max + 1), progress }, &mut bytes)
            .map_err(|e| Error::Network(e.to_string()))?;
        if bytes.len() as u64 > max {
            return Err(Error::Network(format!("file larger than {max} bytes: {url}")));
        }
        Ok(bytes)
    }

    /// Downloads into a temp file next to `dest` and renames it only when complete. Only from
    /// GitHub: the URL comes from the server, which must not point the launcher elsewhere, and
    /// every redirect is checked the same way.
    pub fn download(&self, url: &str, dest: &Path) -> Result<()> {
        self.download_with(url, dest, &mut |_, _| {})
    }

    /// `download`, telling `progress` the bytes so far and the size, when the server says it.
    pub fn download_with(&self, url: &str, dest: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<()> {
        if !download_allowed(url) {
            return Err(Error::ForbiddenHost(url.to_string()));
        }
        let mut current = url.to_string();
        let mut resp = self.download_hop(&current)?;
        for _ in 0..MAX_REDIRECTS {
            if !(300..400).contains(&resp.status()) {
                break;
            }
            let location = resp.header("location").ok_or_else(|| Error::Network(format!("redirect without a target from {current}")))?;
            current = next_hop(&current, location)?;
            resp = self.download_hop(&current)?;
        }
        if (300..400).contains(&resp.status()) {
            return Err(Error::Network(format!("too many redirects from {url}")));
        }
        let total = resp.header("content-length").and_then(|v| v.parse::<u64>().ok());
        let tmp = dest.with_extension("part");
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            let mut done = 0u64;
            let mut count = |n: u64| {
                done += n;
                progress(done, total);
            };
            std::io::copy(&mut Counted { inner: resp.into_reader().take(512 * 1024 * 1024), progress: &mut count }, &mut file)?;
            file.sync_all()
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Network(e.to_string()));
        }
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }
}

/// A reader that tells `progress` how many bytes each read brought.
struct Counted<'a, R> {
    inner: R,
    progress: &'a mut dyn FnMut(u64),
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        (self.progress)(n as u64);
        Ok(n)
    }
}

/// Release and tag metadata through forge-api (`/v1/github/...`), which caches it.
pub struct ApiGitHub {
    client: Client,
    api: String,
}

#[derive(serde::Deserialize)]
struct RefsAnswer {
    head: Option<String>,
    #[serde(default)]
    branches: HashMap<String, String>,
    #[serde(default)]
    tags: Vec<Tag>,
}

impl ApiGitHub {
    pub fn new(client: Client, api: &str) -> ApiGitHub {
        ApiGitHub { client, api: api.trim_end_matches('/').to_string() }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let text = self.client.get_text(&format!("{}/v1/github/{path}", self.api))?;
        serde_json::from_str(&text).map_err(|e| Error::Parse(e.to_string()))
    }

    fn refs(&self, repo: &str) -> Result<RefsAnswer> {
        self.get(&format!("{repo}/refs"))
    }
}

/// Percent-encodes a tag for one path segment (`/` and other reserved characters).
fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~+".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl GitHub for ApiGitHub {
    fn latest_release(&self, repo: &str) -> Result<Option<Release>> {
        self.get(&format!("{repo}/releases/latest"))
    }

    fn release(&self, repo: &str, tag: &str) -> Result<Release> {
        self.get(&format!("{repo}/releases/{}", encode_segment(tag)))
    }

    fn tags(&self, repo: &str) -> Result<Vec<Tag>> {
        Ok(self.refs(repo)?.tags)
    }

    fn default_branch(&self, repo: &str) -> Result<String> {
        self.refs(repo)?.head.ok_or_else(|| Error::Parse("the repo has no default branch".into()))
    }

    fn commit_sha(&self, repo: &str, git_ref: &str) -> Result<String> {
        self.refs(repo)?.branches.remove(git_ref).ok_or_else(|| Error::Parse(format!("branch {git_ref} not found")))
    }
}

/// The platform serves only repos it knows (`known`, lowercased: the registry's and the
/// launcher's own); any other repo comes straight from GitHub (`direct`).
pub struct RoutedGitHub {
    pub platform: Option<ApiGitHub>,
    pub direct: Box<dyn GitHub + Send>,
    pub known: std::collections::HashSet<String>,
}

impl RoutedGitHub {
    fn pick(&self, repo: &str) -> &dyn GitHub {
        match &self.platform {
            Some(api) if self.known.contains(&repo.to_ascii_lowercase()) => api,
            _ => self.direct.as_ref(),
        }
    }
}

impl GitHub for RoutedGitHub {
    fn latest_release(&self, repo: &str) -> Result<Option<Release>> {
        self.pick(repo).latest_release(repo)
    }
    fn release(&self, repo: &str, tag: &str) -> Result<Release> {
        self.pick(repo).release(repo, tag)
    }
    fn tags(&self, repo: &str) -> Result<Vec<Tag>> {
        self.pick(repo).tags(repo)
    }
    fn default_branch(&self, repo: &str) -> Result<String> {
        self.pick(repo).default_branch(repo)
    }
    fn commit_sha(&self, repo: &str, git_ref: &str) -> Result<String> {
        self.pick(repo).commit_sha(repo, git_ref)
    }
}

/// Release and tag metadata from GitHub's REST API with the user's token, for when the
/// SurvivorsForge server cannot be reached: 5000 requests an hour, and unaffected when GitHub
/// slows down requests without a login. A token GitHub refuses (`401`) falls back to `WebGitHub`.
pub struct TokenGitHub {
    client: Client,
    token: String,
    api: String,
    web: WebGitHub,
}

impl TokenGitHub {
    pub fn new(client: Client, token: String) -> TokenGitHub {
        TokenGitHub::with_api(client, token, GITHUB_API)
    }

    /// Against another address, for tests.
    pub fn with_api(client: Client, token: String, api: &str) -> TokenGitHub {
        TokenGitHub { web: WebGitHub::new(client.clone()), client, token, api: api.trim_end_matches('/').to_string() }
    }

    /// GETs `path` of the REST API; `Ok(None)` for `404`.
    fn get(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let request = self
            .client
            .agent
            .get(&format!("{}{path}", self.api))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28");
        match request.call() {
            Ok(resp) => {
                let text = resp.into_string().map_err(|e| Error::Network(e.to_string()))?;
                serde_json::from_str(&text).map(Some).map_err(|e| Error::Parse(e.to_string()))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(401, _)) => Err(Error::Http(401)),
            Err(ureq::Error::Status(403 | 429, _)) => Err(Error::RateLimited),
            Err(ureq::Error::Status(code, _)) => Err(Error::Http(code)),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// `rest`, or the same from the web pages once GitHub refuses the token.
    fn or_web<T>(&self, rest: Result<T>, web: impl FnOnce(&WebGitHub) -> Result<T>) -> Result<T> {
        match rest {
            Err(Error::Http(401)) => web(&self.web),
            other => other,
        }
    }
}

/// A release of the REST API: its assets as the REST address, which `download_headers` sends
/// the token to.
fn rest_release(v: &serde_json::Value, tag: &str) -> Release {
    let assets = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some(Asset { name: a["name"].as_str()?.to_string(), url: a["url"].as_str()?.to_string() }))
        .collect();
    Release { tag: v["tag_name"].as_str().unwrap_or(tag).to_string(), assets }
}

impl GitHub for TokenGitHub {
    fn latest_release(&self, repo: &str) -> Result<Option<Release>> {
        let repo = normalize_repo(repo)?;
        let rest = self.get(&format!("/repos/{repo}/releases/latest")).map(|v| v.map(|v| rest_release(&v, "")));
        self.or_web(rest, |web| web.latest_release(&repo))
    }

    fn release(&self, repo: &str, tag: &str) -> Result<Release> {
        let repo = normalize_repo(repo)?;
        let rest = self
            .get(&format!("/repos/{repo}/releases/tags/{}", encode_segment(tag)))
            .map(|v| v.map_or_else(|| Release { tag: tag.to_string(), assets: Vec::new() }, |v| rest_release(&v, tag)));
        self.or_web(rest, |web| web.release(&repo, tag))
    }

    fn tags(&self, repo: &str) -> Result<Vec<Tag>> {
        let repo = normalize_repo(repo)?;
        let rest = self.get(&format!("/repos/{repo}/tags?per_page=100")).and_then(|v| {
            let v = v.ok_or(Error::NotFound)?;
            let refs: Vec<(String, String)> = v
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| Some((t["commit"]["sha"].as_str()?.to_string(), format!("refs/tags/{}", t["name"].as_str()?))))
                .collect();
            // The same order as the web pages give: newest version first.
            Ok(tags_from_refs(&refs))
        });
        self.or_web(rest, |web| web.tags(&repo))
    }

    fn default_branch(&self, repo: &str) -> Result<String> {
        let repo = normalize_repo(repo)?;
        let rest = self.get(&format!("/repos/{repo}")).and_then(|v| {
            v.and_then(|v| v["default_branch"].as_str().map(str::to_string)).ok_or(Error::NotFound)
        });
        self.or_web(rest, |web| web.default_branch(&repo))
    }

    fn commit_sha(&self, repo: &str, git_ref: &str) -> Result<String> {
        let repo = normalize_repo(repo)?;
        let rest = self.get(&format!("/repos/{repo}/branches/{}", encode_segment(git_ref))).and_then(|v| {
            v.and_then(|v| v["commit"]["sha"].as_str().map(str::to_string)).ok_or_else(|| Error::Parse(format!("branch {git_ref} not found")))
        });
        self.or_web(rest, |web| web.commit_sha(&repo, git_ref))
    }
}

/// Release and tag metadata straight from github.com, for when the SurvivorsForge server cannot be
/// reached: the same pages and parsers the server reads (not the REST API, so no hourly limit).
pub struct WebGitHub {
    client: Client,
}

const GITHUB_WEB: &str = "https://github.com";
/// No release page or ref list comes near this.
const MAX_PAGE: u64 = 4 << 20;

impl WebGitHub {
    pub fn new(client: Client) -> WebGitHub {
        WebGitHub { client }
    }

    /// The page at `path` on github.com, or `Err` with the target of a redirect (`Ok(None)` without one).
    fn page(&self, path: &str) -> Result<std::result::Result<String, Option<String>>> {
        // The download agent follows no redirect, so /releases/latest can be read.
        let resp = Client::call(self.client.downloads.get(&format!("{GITHUB_WEB}{path}")))?;
        if (300..400).contains(&resp.status()) {
            return Ok(Err(resp.header("location").map(str::to_string)));
        }
        let mut text = String::new();
        resp.into_reader().take(MAX_PAGE).read_to_string(&mut text).map_err(|e| Error::Network(e.to_string()))?;
        Ok(Ok(text))
    }

    fn refs(&self, repo: &str) -> Result<Refs> {
        let repo = normalize_repo(repo)?;
        let body = self.page(&format!("/{repo}.git/info/refs?service=git-upload-pack"))?.map_err(|_| Error::Network(format!("{repo}: unexpected redirect")))?;
        Ok(parse_refs(&body))
    }
}

impl GitHub for WebGitHub {
    fn latest_release(&self, repo: &str) -> Result<Option<Release>> {
        let repo = normalize_repo(repo)?;
        match self.page(&format!("/{repo}/releases/latest"))? {
            Err(Some(location)) => match parse_latest_location(&location) {
                Some(tag) => self.release(&repo, &tag).map(Some),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn release(&self, repo: &str, tag: &str) -> Result<Release> {
        let repo = normalize_repo(repo)?;
        let html = self.page(&format!("/{repo}/releases/expanded_assets/{}", encode_segment(tag)))?.unwrap_or_default();
        Ok(Release { assets: parse_assets(&html, GITHUB_WEB, &repo, tag), tag: tag.to_string() })
    }

    fn tags(&self, repo: &str) -> Result<Vec<Tag>> {
        Ok(tags_from_refs(&self.refs(repo)?.refs))
    }

    fn default_branch(&self, repo: &str) -> Result<String> {
        self.refs(repo)?.head.ok_or_else(|| Error::Parse("the repo has no default branch".into()))
    }

    fn commit_sha(&self, repo: &str, git_ref: &str) -> Result<String> {
        let wanted = format!("refs/heads/{git_ref}");
        self.refs(repo)?.refs.into_iter().find(|(_, name)| *name == wanted).map(|(sha, _)| sha).ok_or_else(|| Error::Parse(format!("branch {git_ref} not found")))
    }
}

#[cfg(test)]
mod tests {
    use crate::test_server::serve;

    #[test]
    fn a_network_failure_is_tried_once_more() {
        let calls = std::cell::Cell::new(0);
        let flaky = || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 { Err(Error::Network("timed out (os error 10060)".into())) } else { Ok(7) }
        };
        assert_eq!(retry_network(flaky), Ok(7));
        assert_eq!(calls.get(), 2);

        calls.set(0);
        let down = || {
            calls.set(calls.get() + 1);
            Err::<(), _>(Error::Network("timed out".into()))
        };
        assert!(retry_network(down).is_err());
        assert_eq!(calls.get(), 2, "once more, not forever");

        calls.set(0);
        let refused = || {
            calls.set(calls.get() + 1);
            Err::<(), _>(Error::NotFound)
        };
        assert_eq!(retry_network(refused), Err(Error::NotFound));
        assert_eq!(calls.get(), 1, "an answer of the server is not retried");
    }

    #[test]
    fn routed_github_sends_own_repos_past_the_platform() {
        // The platform at a closed port: any request to it fails.
        let platform = ApiGitHub::new(Client::new(), "http://127.0.0.1:1");
        let (web, _seen) = serve(vec![(200, vec![], r#"{"default_branch":"dev"}"#.into())]);
        let direct = TokenGitHub::with_api(Client::new(), "t".into(), &web);
        let routed = RoutedGitHub { platform: Some(platform), direct: Box::new(direct), known: ["kex0/azsspellmeter".to_string()].into() };
        assert_eq!(routed.default_branch("me/Own").unwrap(), "dev", "an own repo goes straight to GitHub");
        assert!(routed.default_branch("kex0/AZSSpellMeter").is_err(), "a registry repo asks the platform");
    }

    #[test]
    fn token_github_reads_release_json() {
        let release = r#"{"tag_name":"v2.0","assets":[{"name":"Lucky-v2.0.zip","url":"https://api.github.com/repos/o/r/releases/assets/7","browser_download_url":"https://github.com/o/r/releases/download/v2.0/Lucky-v2.0.zip"}]}"#;
        let tags = r#"[{"name":"v1.9","commit":{"sha":"aaa"}},{"name":"v2.0","commit":{"sha":"bbb"}}]"#;
        let (api, _seen) = serve(vec![(200, vec![], release.into()), (200, vec![], tags.into()), (404, vec![], "{}".into())]);
        let gh = TokenGitHub::with_api(Client::new(), "ghu_t".into(), &api);
        let latest = gh.latest_release("o/r").unwrap().unwrap();
        assert_eq!(latest.tag, "v2.0");
        assert_eq!(latest.assets, vec![Asset { name: "Lucky-v2.0.zip".into(), url: "https://api.github.com/repos/o/r/releases/assets/7".into() }]);
        let tags = gh.tags("o/r").unwrap();
        assert_eq!(tags.iter().map(|t| (t.name.as_str(), t.sha.as_str())).collect::<Vec<_>>(), [("v2.0", "bbb"), ("v1.9", "aaa")], "newest first");
        assert_eq!(gh.latest_release("o/r"), Ok(None), "404: no release");
    }

    #[test]
    fn token_github_sends_auth_headers() {
        let (api, seen) = serve(vec![(200, vec![], r#"{"default_branch":"main"}"#.into())]);
        let gh = TokenGitHub::with_api(Client::new(), "ghu_t".into(), &api);
        assert_eq!(gh.default_branch("o/r").unwrap(), "main");
        let request = seen.recv().unwrap();
        assert_eq!(request.line, "GET /repos/o/r HTTP/1.1");
        assert_eq!(request.header("authorization"), Some("Bearer ghu_t"));
        assert_eq!(request.header("accept"), Some("application/vnd.github+json"));
        assert_eq!(request.header("user-agent"), Some(user_agent().as_str()));
        assert_eq!(request.header("x-survivorsforge-build"), None, "the build token never goes to GitHub");
    }

    #[test]
    fn token_not_sent_after_redirect_to_other_host() {
        let asset = "https://api.github.com/repos/o/r/releases/assets/7";
        assert_eq!(download_headers(asset, Some("ghu_t")), vec![("Authorization", "Bearer ghu_t".to_string()), ("Accept", "application/octet-stream".to_string())]);
        assert!(download_headers("https://release-assets.githubusercontent.com/x?sig=1", Some("ghu_t")).is_empty(), "the redirect target gets no token");
        assert!(download_headers("https://github.com/o/r/releases/download/v1/a.zip", Some("ghu_t")).is_empty());
        assert!(download_headers("https://api.github.com.evil.test/x", Some("ghu_t")).is_empty());
        assert!(download_headers(asset, None).is_empty());
        assert!(download_allowed(asset));
    }

    use super::*;

    #[test]
    fn names_version_and_platform_for_the_active_launcher_counts() {
        let name = if build_token().is_some() { "SurvivorsForge" } else { unofficial_name() };
        assert_eq!(user_agent(), format!("{name}/{} ({})", crate::APP_VERSION, std::env::consts::OS));
    }

    /// Talks to github.com: `cargo test -- --ignored web_github_live`.
    #[test]
    #[ignore]
    fn web_github_live() {
        let gh = WebGitHub::new(Client::new());
        let release = gh.latest_release("cadoky/LuckyOdds").unwrap().expect("LuckyOdds has releases");
        assert!(release.assets.iter().any(|a| a.name.ends_with(".zip")), "{release:?}");
        assert!(!gh.tags("cadoky/LuckyOdds").unwrap().is_empty());
        let branch = gh.default_branch("cadoky/LuckyOdds").unwrap();
        assert_eq!(gh.commit_sha("cadoky/LuckyOdds", &branch).unwrap().len(), 40);
    }

    #[test]
    fn parses_latest_location() {
        assert_eq!(
            parse_latest_location("https://github.com/cadoky/LuckyOdds/releases/tag/v0.6.0").as_deref(),
            Some("v0.6.0")
        );
        assert_eq!(parse_latest_location("https://github.com/o/r/releases/tag/a%2Bb").as_deref(), Some("a+b"));
        assert_eq!(parse_latest_location("https://github.com/Rezondes/SurvivorsForge/releases"), None);
    }

    #[test]
    fn parses_release_assets() {
        let html = include_str!("../tests/fixtures/github/assets_survivor_ui.html");
        let assets = parse_assets(html, "https://github.com", "morderous/survivor-ui", "v3.0.36");
        let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["SurvivorUI-Installer.zip", "SurvivorUI-v3.0.36.zip"]);
        assert_eq!(
            assets[1].url,
            "https://github.com/morderous/survivor-ui/releases/download/v3.0.36/SurvivorUI-v3.0.36.zip"
        );
        assert!(parse_assets("<p>nothing</p>", "https://github.com", "a/b", "v1").is_empty());
    }

    #[test]
    fn parses_refs_tags_and_head() {
        let body = String::from_utf8_lossy(include_bytes!("../tests/fixtures/github/refs_bordoui.txt"));
        let Refs { refs, head } = parse_refs(&body);
        assert_eq!(head.as_deref(), Some("main"));
        assert!(refs.iter().any(|(sha, name)| name == "refs/heads/main" && sha.len() == 40));
        let tags = tags_from_refs(&refs);
        assert_eq!(tags[0].name, "v2.8.6");
        assert!(tags.iter().all(|t| !t.name.ends_with("^{}")));
    }

    #[test]
    fn tags_sort_by_version_numbers_and_use_peeled_sha() {
        let refs = [
            ("a".repeat(40), "refs/tags/v2.8.9".to_string()),
            ("b".repeat(40), "refs/tags/v2.8.10".to_string()),
            ("c".repeat(40), "refs/tags/v2.8.10^{}".to_string()),
            ("d".repeat(40), "refs/heads/main".to_string()),
        ];
        let tags = tags_from_refs(&refs);
        assert_eq!(tags, [
            Tag { name: "v2.8.10".into(), sha: "c".repeat(40) },
            Tag { name: "v2.8.9".into(), sha: "a".repeat(40) },
        ]);
    }

    #[test]
    fn tags_rank_release_above_its_pre_releases() {
        let names = ["v1.5.1-review.1", "v1.6.0-beta.2", "v1.6.0", "v1.6.0-beta.10", "v1.5.1"];
        let refs: Vec<(String, String)> = names.iter().map(|n| ("a".repeat(40), format!("refs/tags/{n}"))).collect();
        let sorted: Vec<String> = tags_from_refs(&refs).into_iter().map(|t| t.name).collect();
        assert_eq!(sorted, ["v1.6.0", "v1.6.0-beta.10", "v1.6.0-beta.2", "v1.5.1", "v1.5.1-review.1"]);
    }

    #[test]
    fn normalizes_repo_input() {
        for (input, expected) in [
            ("cadoky/LuckyOdds", "cadoky/LuckyOdds"),
            (" https://github.com/cadoky/LuckyOdds ", "cadoky/LuckyOdds"),
            ("https://github.com/cadoky/LuckyOdds.git", "cadoky/LuckyOdds"),
            ("https://github.com/cadoky/LuckyOdds/tree/main", "cadoky/LuckyOdds"),
            ("github.com/cadoky/LuckyOdds/", "cadoky/LuckyOdds"),
        ] {
            assert_eq!(normalize_repo(input).as_deref(), Ok(expected), "{input}");
        }
        for input in ["", "cadoky", "https://gitlab.com/a/b", "a/b/c", "a b/c"] {
            assert!(normalize_repo(input).is_err(), "{input}");
        }
    }

    #[test]
    fn maps_status_codes() {
        assert_eq!(map_error(404), Error::NotFound);
        assert_eq!(map_error(401), Error::NotFound);
        assert_eq!(map_error(429), Error::RateLimited);
        assert_eq!(map_error(500), Error::Http(500));
    }

    #[test]
    fn downloads_only_from_github() {
        assert!(download_allowed("https://github.com/o/r/releases/download/v1/a.zip"));
        assert!(download_allowed("https://codeload.github.com/o/r/zip/v1"));
        for url in ["http://github.com/a.zip", "https://github.com.evil.test/a.zip", "https://evil.test/github.com/a.zip", "file:///c:/a.zip"] {
            assert!(!download_allowed(url), "{url}");
        }
        let dir = tempfile::tempdir().unwrap();
        let refused = Client::new().download("https://evil.test/a.zip", &dir.path().join("a.zip"));
        assert_eq!(refused, Err(Error::ForbiddenHost("https://evil.test/a.zip".into())));
    }

    #[test]
    fn every_redirect_stays_on_github() {
        let start = "https://github.com/o/r/releases/download/v1/a.zip";
        let asset = "https://release-assets.githubusercontent.com/x/a.zip?sig=1";
        assert_eq!(next_hop(start, asset), Ok(asset.to_string()));
        assert_eq!(next_hop(start, "/o/r/archive/v1.zip"), Ok("https://github.com/o/r/archive/v1.zip".to_string()), "relative to the host");
        for evil in ["https://evil.test/a.zip", "http://objects.githubusercontent.com/a.zip", "//evil.test/a.zip"] {
            assert!(matches!(next_hop(start, evil), Err(Error::ForbiddenHost(_))), "{evil}");
        }
    }

    #[test]
    fn tags_are_encoded_for_the_path() {
        assert_eq!(encode_segment("v1.2+b/x y"), "v1.2+b%2Fx%20y");
    }

    #[test]
    #[ignore = "needs a running forge-api in SURVIVORSFORGE_API_URL and network"]
    fn api_live() {
        let github = ApiGitHub::new(Client::new(), &crate::registry::api_url(None));
        let release = github.latest_release("cadoky/LuckyOdds").unwrap().unwrap();
        assert!(release.tag.starts_with('v'));
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        Client::new().download(&release.assets[0].url, &zip).unwrap();
        assert!(!crate::archive::analyze(&zip).unwrap().is_empty());
        assert_eq!(github.default_branch("cadoky/LuckyOdds").unwrap(), "main");
        assert_eq!(github.commit_sha("cadoky/LuckyOdds", "main").unwrap().len(), 40);
        assert!(github.tags("threenox/BordoUI").unwrap()[0].name.starts_with('v'));
        assert_eq!(github.latest_release("nope-xyz-123/nothing"), Err(Error::NotFound));
    }
}
