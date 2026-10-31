//! An in-memory GitHub for tests. Counts every call, so tests can prove that nothing hit GitHub.

use async_trait::async_trait;
use forge_api::github::types::{Release, Repo, Tag, User};
use forge_api::github::{GitHubApi, GitHubError, GitHubResult};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
pub struct FakeData {
    /// OAuth code -> user.
    pub oauth_users: HashMap<String, User>,
    /// `owner/name` -> installation ID.
    pub installations: HashMap<String, i64>,
    pub repos: HashMap<i64, Repo>,
    /// (repo ID, login) -> permission.
    pub permissions: HashMap<(i64, String), String>,
    /// Login (lowercase) -> user.
    pub users: HashMap<String, User>,
    pub releases: HashMap<i64, Vec<Release>>,
    pub tags: HashMap<i64, Vec<Tag>>,
    /// URL -> bytes.
    pub downloads: HashMap<String, Vec<u8>>,
    /// Login (lowercase) -> its public organizations.
    pub orgs: HashMap<String, Vec<String>>,
}

#[derive(Default)]
pub struct FakeGitHub {
    data: Mutex<FakeData>,
    calls: AtomicUsize,
}

impl FakeGitHub {
    pub fn data(&self) -> MutexGuard<'_, FakeData> {
        self.data.lock().unwrap()
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn call(&self) -> MutexGuard<'_, FakeData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.data()
    }

    /// Adds a public repo `owner/name` with the App installed and `admin_login` as admin.
    pub fn add_repo(&self, id: i64, full_name: &str, admin_login: &str) {
        let mut data = self.data();
        let owner = full_name.split('/').next().unwrap().to_string();
        data.repos.insert(
            id,
            Repo { id, full_name: full_name.into(), private: false, owner: forge_api::github::types::Owner { id: 0, login: owner }, description: None },
        );
        data.installations.insert(full_name.to_lowercase(), 1);
        data.permissions.insert((id, admin_login.to_lowercase()), "admin".into());
    }

    /// A public repo of the account with GitHub id `owner_id`, without the App.
    pub fn add_own_repo(&self, id: i64, full_name: &str, owner_id: i64) {
        let owner = full_name.split('/').next().unwrap().to_string();
        let owner = forge_api::github::types::Owner { id: owner_id, login: owner };
        self.data().repos.insert(id, Repo { id, full_name: full_name.into(), private: false, owner, description: None });
    }
}

#[async_trait]
impl GitHubApi for FakeGitHub {
    async fn installation_for_repo(&self, full_name: &str) -> GitHubResult<Option<i64>> {
        Ok(self.call().installations.get(&full_name.to_lowercase()).copied())
    }

    async fn user_orgs(&self, login: &str) -> GitHubResult<Vec<String>> {
        Ok(self.call().orgs.get(&login.to_lowercase()).cloned().unwrap_or_default())
    }

    async fn user_repos(&self, login: &str) -> GitHubResult<Vec<Repo>> {
        let mut repos: Vec<Repo> = self.call().repos.values().filter(|r| r.owner.login.eq_ignore_ascii_case(login)).cloned().collect();
        repos.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        Ok(repos)
    }

    async fn repo(&self, _installation: Option<i64>, repo_id: i64) -> GitHubResult<Repo> {
        self.call().repos.get(&repo_id).cloned().ok_or(GitHubError::NotFound)
    }

    async fn repo_by_name(&self, _installation: Option<i64>, full_name: &str) -> GitHubResult<Repo> {
        let data = self.call();
        data.repos.values().find(|r| r.full_name.eq_ignore_ascii_case(full_name)).cloned().ok_or(GitHubError::NotFound)
    }

    async fn user_permission(&self, _installation: i64, repo_id: i64, login: &str) -> GitHubResult<String> {
        Ok(self.call().permissions.get(&(repo_id, login.to_lowercase())).cloned().unwrap_or_else(|| "none".into()))
    }

    async fn user_by_login(&self, _installation: Option<i64>, login: &str) -> GitHubResult<Option<User>> {
        Ok(self.call().users.get(&login.to_lowercase()).cloned())
    }

    async fn releases(&self, _installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Release>> {
        Ok(self.call().releases.get(&repo_id).cloned().unwrap_or_default())
    }

    async fn release(&self, _installation: Option<i64>, repo_id: i64, release_id: i64) -> GitHubResult<Option<Release>> {
        Ok(self.call().releases.get(&repo_id).and_then(|rs| rs.iter().find(|r| r.id == release_id).cloned()))
    }

    async fn tags(&self, _installation: Option<i64>, repo_id: i64) -> GitHubResult<Vec<Tag>> {
        Ok(self.call().tags.get(&repo_id).cloned().unwrap_or_default())
    }

    async fn oauth_user(&self, code: &str, _verifier: &str, _redirect_uri: &str) -> GitHubResult<User> {
        self.call().oauth_users.get(code).cloned().ok_or_else(|| GitHubError::Auth("bad_verification_code".into()))
    }

    async fn rate_limit(&self) -> GitHubResult<forge_api::github::types::RateLimit> {
        Ok(forge_api::github::types::RateLimit { limit: 5000, remaining: 4321, reset_at: None })
    }

    async fn download(&self, url: &str, max_bytes: u64) -> GitHubResult<Vec<u8>> {
        let bytes = self.call().downloads.get(url).cloned().ok_or(GitHubError::NotFound)?;
        if bytes.len() as u64 > max_bytes {
            return Err(GitHubError::TooLarge);
        }
        Ok(bytes)
    }
}
