//! An in-memory GitHub for tests. Counts every call, so tests can prove that nothing hit GitHub.

use async_trait::async_trait;
use chrono::Utc;
use forge_api::github::types::{Issue, IssueComment, Label, Release, Repo, Tag, User, UserToken};
use forge_api::github::{GitHubApi, GitHubError, GitHubResult};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
pub struct FakeData {
    /// GitHub user token -> its account; a login hands out `gho-<github id>`.
    pub user_tokens: HashMap<String, User>,
    /// Refresh token -> its account; a login hands out `ghr-<github id>`.
    pub refresh_tokens: HashMap<String, User>,
    /// Installations whose owner has not granted `Issues: Read & write`.
    pub issues_read_only: HashSet<i64>,
    /// `owner/name` (lowercase) -> issues and pull requests, as GitHub lists them.
    pub issues: HashMap<String, Vec<Issue>>,
    /// (`owner/name` lowercase, issue number) -> replies.
    pub comments: HashMap<(String, i64), Vec<IssueComment>>,
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
            Repo { id, full_name: full_name.into(), private: false, owner: forge_api::github::types::Owner { id: 0, login: owner }, description: None, has_issues: true },
        );
        data.installations.insert(full_name.to_lowercase(), 1);
        data.permissions.insert((id, admin_login.to_lowercase()), "admin".into());
    }

    /// A public repo of the account with GitHub id `owner_id`, without the App.
    pub fn add_own_repo(&self, id: i64, full_name: &str, owner_id: i64) {
        let owner = full_name.split('/').next().unwrap().to_string();
        let owner = forge_api::github::types::Owner { id: owner_id, login: owner };
        self.data().repos.insert(id, Repo { id, full_name: full_name.into(), private: false, owner, description: None, has_issues: true });
    }
}

fn user_token(user: &User, prefix: &str) -> UserToken {
    UserToken {
        access_token: format!("{prefix}-{}", user.id),
        expires_at: Some(Utc::now() + chrono::Duration::hours(8)),
        refresh_token: Some(format!("ghr-{prefix}-{}", user.id)),
        refresh_token_expires_at: Some(Utc::now() + chrono::Duration::days(180)),
    }
}

/// An open issue of `author`, as GitHub returns it.
pub fn fake_issue(repo: &str, number: i64, title: &str, body: &str, author: &User) -> Issue {
    let now = Utc::now();
    Issue {
        number,
        title: title.into(),
        body: Some(body.into()),
        state: "open".into(),
        state_reason: None,
        user: Some(author.clone()),
        labels: Vec::new(),
        comments: 0,
        created_at: now,
        updated_at: now,
        html_url: format!("https://github.com/{repo}/issues/{number}"),
        locked: false,
        pull_request: None,
    }
}

impl FakeData {
    /// The account of `token`, if the App may write issues in `repo`.
    fn writer(&self, token: &str, repo: &str) -> GitHubResult<User> {
        let user = self.user_tokens.get(token).cloned().ok_or(GitHubError::Http(401))?;
        let installation = self.installations.get(&repo.to_lowercase()).ok_or(GitHubError::Http(403))?;
        if self.issues_read_only.contains(installation) {
            return Err(GitHubError::Http(403));
        }
        Ok(user)
    }

    fn own_comment(&mut self, repo: &str, comment_id: i64, user: &User) -> GitHubResult<&mut IssueComment> {
        let repo = repo.to_lowercase();
        let comment = self
            .comments
            .iter_mut()
            .filter(|((r, _), _)| *r == repo)
            .flat_map(|(_, all)| all.iter_mut())
            .find(|c| c.id == comment_id)
            .ok_or(GitHubError::NotFound)?;
        if comment.user.as_ref().map(|u| u.id) != Some(user.id) {
            return Err(GitHubError::Http(403));
        }
        Ok(comment)
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

    async fn oauth_user(&self, code: &str, _verifier: &str, _redirect_uri: &str) -> GitHubResult<(User, UserToken)> {
        let mut data = self.call();
        let user = data.oauth_users.get(code).cloned().ok_or_else(|| GitHubError::Auth("bad_verification_code".into()))?;
        let token = user_token(&user, "gho");
        data.user_tokens.insert(token.access_token.clone(), user.clone());
        data.refresh_tokens.insert(token.refresh_token.clone().unwrap(), user.clone());
        Ok((user, token))
    }

    async fn refresh_user_token(&self, refresh_token: &str) -> GitHubResult<UserToken> {
        let mut data = self.call();
        let user = data.refresh_tokens.remove(refresh_token).ok_or_else(|| GitHubError::Auth("bad_refresh_token".into()))?;
        let token = user_token(&user, "gho2");
        data.user_tokens.insert(token.access_token.clone(), user.clone());
        data.refresh_tokens.insert(token.refresh_token.clone().unwrap(), user);
        Ok(token)
    }

    async fn token_user(&self, token: &str) -> GitHubResult<User> {
        self.call().user_tokens.get(token).cloned().ok_or(GitHubError::Http(401))
    }

    async fn issues_writable(&self, installation: i64) -> GitHubResult<bool> {
        Ok(!self.call().issues_read_only.contains(&installation))
    }

    async fn issues(&self, _installation: Option<i64>, repo: &str) -> GitHubResult<Vec<Issue>> {
        let mut issues = self.call().issues.get(&repo.to_lowercase()).cloned().unwrap_or_default();
        issues.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        issues.truncate(100);
        Ok(issues)
    }

    async fn issue(&self, _installation: Option<i64>, repo: &str, number: i64) -> GitHubResult<Option<Issue>> {
        Ok(self.call().issues.get(&repo.to_lowercase()).and_then(|all| all.iter().find(|i| i.number == number).cloned()))
    }

    async fn issue_comments(&self, _installation: Option<i64>, repo: &str, number: i64) -> GitHubResult<Vec<IssueComment>> {
        Ok(self.call().comments.get(&(repo.to_lowercase(), number)).cloned().unwrap_or_default())
    }

    async fn add_labels(&self, installation: i64, repo: &str, number: i64, labels: &[&str]) -> GitHubResult<()> {
        let mut data = self.call();
        if data.issues_read_only.contains(&installation) {
            return Err(GitHubError::Http(403));
        }
        let issue = data.issues.get_mut(&repo.to_lowercase()).and_then(|all| all.iter_mut().find(|i| i.number == number)).ok_or(GitHubError::NotFound)?;
        issue.labels.extend(labels.iter().map(|l| Label { name: l.to_string() }));
        Ok(())
    }

    async fn create_issue(&self, token: &str, repo: &str, title: &str, body: &str) -> GitHubResult<Issue> {
        let mut data = self.call();
        let user = data.writer(token, repo)?;
        let all = data.issues.entry(repo.to_lowercase()).or_default();
        let number = all.iter().map(|i| i.number).max().unwrap_or(0) + 1;
        let issue = fake_issue(repo, number, title, body, &user);
        all.push(issue.clone());
        Ok(issue)
    }

    async fn update_issue(&self, token: &str, repo: &str, number: i64, changes: &serde_json::Value) -> GitHubResult<Issue> {
        let mut data = self.call();
        let user = data.user_tokens.get(token).cloned().ok_or(GitHubError::Http(401))?;
        let issue = data.issues.get_mut(&repo.to_lowercase()).and_then(|all| all.iter_mut().find(|i| i.number == number)).ok_or(GitHubError::NotFound)?;
        if issue.user.as_ref().map(|u| u.id) != Some(user.id) {
            return Err(GitHubError::Http(403));
        }
        if let Some(title) = changes["title"].as_str() {
            issue.title = title.into();
        }
        if let Some(body) = changes["body"].as_str() {
            issue.body = Some(body.into());
        }
        if let Some(state) = changes["state"].as_str() {
            issue.state = state.into();
        }
        issue.updated_at = Utc::now();
        Ok(issue.clone())
    }

    async fn create_comment(&self, token: &str, repo: &str, number: i64, body: &str) -> GitHubResult<IssueComment> {
        let mut data = self.call();
        let user = data.writer(token, repo)?;
        let issue = data.issues.get_mut(&repo.to_lowercase()).and_then(|all| all.iter_mut().find(|i| i.number == number)).ok_or(GitHubError::NotFound)?;
        issue.comments += 1;
        let id = data.comments.values().flatten().map(|c| c.id).max().unwrap_or(1000) + 1;
        let now = Utc::now();
        let comment = IssueComment {
            id,
            body: Some(body.into()),
            user: Some(user),
            created_at: now,
            updated_at: now,
            html_url: format!("https://github.com/{repo}/issues/{number}#issuecomment-{id}"),
        };
        data.comments.entry((repo.to_lowercase(), number)).or_default().push(comment.clone());
        Ok(comment)
    }

    async fn update_comment(&self, token: &str, repo: &str, comment_id: i64, body: &str) -> GitHubResult<IssueComment> {
        let mut data = self.call();
        let user = data.user_tokens.get(token).cloned().ok_or(GitHubError::Http(401))?;
        let comment = data.own_comment(repo, comment_id, &user)?;
        comment.body = Some(body.into());
        comment.updated_at = Utc::now() + chrono::Duration::seconds(5);
        Ok(comment.clone())
    }

    async fn delete_comment(&self, token: &str, repo: &str, comment_id: i64) -> GitHubResult<()> {
        let mut data = self.call();
        let user = data.user_tokens.get(token).cloned().ok_or(GitHubError::Http(401))?;
        data.own_comment(repo, comment_id, &user)?;
        for ((r, _), all) in data.comments.iter_mut() {
            if *r == repo.to_lowercase() {
                all.retain(|c| c.id != comment_id);
            }
        }
        Ok(())
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
