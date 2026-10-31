//! What the launcher needs from GitHub: release and tag metadata comes from forge-api
//! (`crate::api::ApiGitHub`), only the zip files are downloaded from GitHub directly.

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

pub fn source_zip_url(repo: &str, git_ref: &str) -> String {
    format!("https://codeload.github.com/{repo}/zip/{git_ref}")
}

/// The only hosts zips are downloaded from: GitHub's release and archive downloads.
pub const DOWNLOAD_HOSTS: &[&str] =
    &["github.com", "codeload.github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];

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
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    pub fn new() -> Client {
        let builder = || {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(60))
                .user_agent(&format!("{}/{}", crate::APP_NAME, crate::APP_VERSION))
        };
        Client { agent: builder().build(), install_id: None, downloads: builder().redirects(0).build() }
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
        let mut request = self.agent.get(url);
        if let Some(id) = &self.install_id {
            request = request.set(crate::telemetry::INSTALL_HEADER, id);
        }
        Self::call(request)?.into_string().map_err(|e| Error::Network(e.to_string()))
    }

    /// Downloads into a temp file next to `dest` and renames it only when complete. Only from
    /// GitHub: the URL comes from the server, which must not point the launcher elsewhere, and
    /// every redirect is checked the same way.
    pub fn download(&self, url: &str, dest: &Path) -> Result<()> {
        if !download_allowed(url) {
            return Err(Error::ForbiddenHost(url.to_string()));
        }
        let mut current = url.to_string();
        let mut resp = Self::call(self.downloads.get(&current))?;
        for _ in 0..MAX_REDIRECTS {
            if !(300..400).contains(&resp.status()) {
                break;
            }
            let location = resp.header("location").ok_or_else(|| Error::Network(format!("redirect without a target from {current}")))?;
            current = next_hop(&current, location)?;
            resp = Self::call(self.downloads.get(&current))?;
        }
        if (300..400).contains(&resp.status()) {
            return Err(Error::Network(format!("too many redirects from {url}")));
        }
        let tmp = dest.with_extension("part");
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            std::io::copy(&mut resp.into_reader().take(512 * 1024 * 1024), &mut file)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
