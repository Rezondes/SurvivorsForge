//! The user's login on the platform and their GitHub token, kept in the system's keyring
//! (Windows Credential Manager, macOS Keychain, Secret Service on Linux), never in `config.json`.
//! One entry per platform API, so another API in the settings never sees this login.

use crate::Error;
use crate::github::Client;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

const SERVICE: &str = "SurvivorsForge";

/// A login on the platform: the API's bearer token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    /// RFC 3339.
    pub expires_at: String,
    /// GitHub login name, shown while the platform cannot be reached.
    #[serde(default)]
    pub login: String,
}

/// The user's GitHub token, as the API's `github_token` (`UserToken`) gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubToken {
    pub access_token: String,
    /// RFC 3339; `None` for a token that does not expire.
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub refresh_token_expires_at: Option<String>,
}

/// What the keyring holds for one platform API.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub platform: Option<Session>,
    #[serde(default)]
    pub github: Option<GitHubToken>,
}

/// Where accounts live: the system keyring, or memory in tests.
pub trait SecretStore: Send + Sync {
    fn get(&self, api: &str) -> Result<Option<String>, String>;
    fn set(&self, api: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, api: &str) -> Result<(), String>;
}

/// The system keyring.
pub struct Keyring;

impl SecretStore for Keyring {
    fn get(&self, api: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(SERVICE, api).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, api: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, api).and_then(|e| e.set_password(secret)).map_err(|e| e.to_string())
    }

    fn delete(&self, api: &str) -> Result<(), String> {
        match keyring::Entry::new(SERVICE, api).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Accounts in memory, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore {
    pub entries: Mutex<HashMap<String, String>>,
    /// Every call fails, like a keyring that is locked or missing.
    pub broken: bool,
}

#[cfg(test)]
impl MemoryStore {
    fn check(&self) -> Result<(), String> {
        if self.broken { Err("no keyring".into()) } else { Ok(()) }
    }
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, api: &str) -> Result<Option<String>, String> {
        self.check()?;
        Ok(self.entries.lock().unwrap_or_else(|e| e.into_inner()).get(api).cloned())
    }

    fn set(&self, api: &str, secret: &str) -> Result<(), String> {
        self.check()?;
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).insert(api.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, api: &str) -> Result<(), String> {
        self.check()?;
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).remove(api);
        Ok(())
    }
}

impl Account {
    /// The account for `api`; `None` when there is none or the keyring cannot be read.
    pub fn load(store: &dyn SecretStore, api: &str) -> Option<Account> {
        let secret = store.get(api).ok()??;
        serde_json::from_str(&secret).ok()
    }

    pub fn save(&self, store: &dyn SecretStore, api: &str) -> Result<(), String> {
        let secret = serde_json::to_string(self).map_err(|e| e.to_string())?;
        store.set(api, &secret)
    }

    /// Forgets the account for `api`; one that is already gone is fine.
    pub fn delete(store: &dyn SecretStore, api: &str) -> Result<(), String> {
        store.delete(api)
    }
}

/// Why there is no GitHub token to use.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessError {
    /// No token, or one that is gone or refused: the user has to log in again.
    NeedsLogin,
}

/// A token is renewed when it has less than this left.
const RENEW_BEFORE: Duration = Duration::minutes(5);

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// The user's GitHub token, renewed through the platform when it runs out within five minutes (as
/// the website does). While the platform cannot be reached a token that still runs is used; one
/// the platform refuses to renew is forgotten.
pub fn github_access(client: &Client, store: &dyn SecretStore, api: &str, now: DateTime<Utc>) -> Result<String, AccessError> {
    let mut account = Account::load(store, api).ok_or(AccessError::NeedsLogin)?;
    let github = account.github.clone().ok_or(AccessError::NeedsLogin)?;
    let expires = github.expires_at.as_deref().and_then(parse_time);
    if expires.is_none_or(|e| e - RENEW_BEFORE > now) {
        return Ok(github.access_token);
    }
    let still_valid = || if expires.is_some_and(|e| e > now) { Ok(github.access_token.clone()) } else { Err(AccessError::NeedsLogin) };
    let (Some(session), Some(refresh)) = (&account.platform, &github.refresh_token) else { return still_valid() };
    let body = json!({ "refresh_token": refresh });
    match client.post_json(&format!("{api}/v1/auth/github/refresh"), Some(&session.token), &[], &body) {
        Ok(text) => {
            let renewed: GitHubToken = serde_json::from_str(&text).map_err(|_| AccessError::NeedsLogin)?;
            account.github = Some(renewed.clone());
            // A token that cannot be saved still works for now.
            let _ = account.save(store, api);
            Ok(renewed.access_token)
        }
        Err(Error::Api { code, .. }) if code == "github_reauth" => {
            account.github = None;
            let _ = account.save(store, api);
            Err(AccessError::NeedsLogin)
        }
        Err(_) => still_valid(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::serve;

    const API: &str = "https://forgeapi.test";

    fn account() -> Account {
        Account {
            platform: Some(Session { token: "bearer".into(), expires_at: "2026-12-01T00:00:00Z".into(), login: "octo".into() }),
            github: Some(GitHubToken {
                access_token: "ghu_x".into(),
                expires_at: Some("2026-11-08T20:00:00Z".into()),
                refresh_token: Some("ghr_y".into()),
                refresh_token_expires_at: None,
            }),
        }
    }

    #[test]
    fn saved_account_loads_again() {
        let store = MemoryStore::default();
        account().save(&store, API).unwrap();
        assert_eq!(Account::load(&store, API), Some(account()));
    }

    #[test]
    fn account_belongs_to_its_api() {
        let store = MemoryStore::default();
        account().save(&store, API).unwrap();
        assert_eq!(Account::load(&store, "https://other.test"), None);
    }

    #[test]
    fn delete_twice_is_fine() {
        let store = MemoryStore::default();
        account().save(&store, API).unwrap();
        Account::delete(&store, API).unwrap();
        Account::delete(&store, API).unwrap();
        assert_eq!(Account::load(&store, API), None);
    }

    #[test]
    fn broken_store_reads_as_logged_out() {
        let store = MemoryStore { broken: true, ..Default::default() };
        assert_eq!(Account::load(&store, API), None);
        assert!(account().save(&store, API).is_err());
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// `account()` saved in a new store for `api`.
    fn stored(api: &str) -> MemoryStore {
        let store = MemoryStore::default();
        account().save(&store, api).unwrap();
        store
    }

    #[test]
    fn fresh_token_needs_no_request() {
        // The token runs until 20:00; at 19:50 it still has ten minutes.
        let store = stored("http://127.0.0.1:1");
        let token = github_access(&Client::new(), &store, "http://127.0.0.1:1", at("2026-11-08T19:50:00Z"));
        assert_eq!(token, Ok("ghu_x".into()));
    }

    #[test]
    fn expiring_token_is_renewed_and_saved() {
        let renewed = r#"{"access_token":"ghu_new","expires_at":"2026-11-09T04:00:00Z","refresh_token":"ghr_new","refresh_token_expires_at":null}"#;
        let (api, seen) = serve(vec![(200, vec![], renewed.into())]);
        let store = stored(&api);
        assert_eq!(github_access(&Client::new(), &store, &api, at("2026-11-08T19:57:00Z")), Ok("ghu_new".into()));
        let request = seen.recv().unwrap();
        assert_eq!(request.line, "POST /v1/auth/github/refresh HTTP/1.1");
        assert_eq!(request.header("authorization"), Some("Bearer bearer"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&request.body).unwrap()["refresh_token"], "ghr_y");
        let saved = Account::load(&store, &api).unwrap().github.unwrap();
        assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_deref()), ("ghu_new", Some("ghr_new")));
    }

    #[test]
    fn refused_refresh_drops_github_token() {
        let (api, _seen) = serve(vec![(403, vec![], r#"{"error":"github_reauth","message":"Log in again to post on GitHub"}"#.into())]);
        let store = stored(&api);
        assert_eq!(github_access(&Client::new(), &store, &api, at("2026-11-08T21:00:00Z")), Err(AccessError::NeedsLogin));
        let saved = Account::load(&store, &api).unwrap();
        assert_eq!(saved.github, None);
        assert!(saved.platform.is_some(), "the platform login stays");
    }

    #[test]
    fn api_down_keeps_valid_token() {
        let api = "http://127.0.0.1:1";
        let store = stored(api);
        assert_eq!(github_access(&Client::new(), &store, api, at("2026-11-08T19:58:00Z")), Ok("ghu_x".into()), "two minutes left");
        assert_eq!(github_access(&Client::new(), &store, api, at("2026-11-08T20:01:00Z")), Err(AccessError::NeedsLogin), "expired");
        assert_eq!(Account::load(&store, api), Some(account()), "unchanged");
    }

    #[test]
    fn no_github_token_needs_login() {
        let store = MemoryStore::default();
        assert_eq!(github_access(&Client::new(), &store, "http://127.0.0.1:1", at("2026-11-08T19:00:00Z")), Err(AccessError::NeedsLogin));
    }

    /// Writes to the real keyring of this machine: `cargo test -p survivorsforge real_keyring -- --ignored`.
    #[test]
    #[ignore]
    fn real_keyring_round_trip() {
        let api = "https://keyring-test.invalid";
        account().save(&Keyring, api).unwrap();
        assert_eq!(Account::load(&Keyring, api), Some(account()));
        Account::delete(&Keyring, api).unwrap();
        Account::delete(&Keyring, api).unwrap();
        assert_eq!(Account::load(&Keyring, api), None);
    }

    #[test]
    fn garbage_in_the_store_reads_as_logged_out() {
        let store = MemoryStore::default();
        store.set(API, "not json").unwrap();
        assert_eq!(Account::load(&store, API), None);
    }
}
