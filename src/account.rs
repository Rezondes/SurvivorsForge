//! The user's login on the platform and their GitHub token, kept in the system's keyring
//! (Windows Credential Manager, macOS Keychain, Secret Service on Linux), never in `config.json`.
//! One entry per platform API, so another API in the settings never sees this login.

use serde::{Deserialize, Serialize};
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

#[cfg(test)]
mod tests {
    use super::*;

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
