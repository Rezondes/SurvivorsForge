//! Logging in to GitHub alone, without the platform (GitHub's device flow): for when the
//! SurvivorsForge server cannot be reached. The launcher shows a code, the user enters it at
//! github.com/login/device (on any device), and the launcher gets a GitHub token that it renews
//! with GitHub itself. It serves releases and zips (`TokenGitHub`); comments and issues still
//! need the platform login.
//!
//! The GitHub App must have "Enable Device Flow" switched on. A device flow token renews without
//! the App's client secret, so the launcher needs only the public client ID.

use crate::account::{Account, GitHubToken, SecretStore, TokenSource};
use crate::github::Client;
use crate::login::LoginError;
use chrono::{DateTime, Duration as Span, Utc};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Where GitHub's OAuth endpoints live.
pub const GITHUB_OAUTH: &str = "https://github.com";
/// Client ID of the GitHub App "SurvivorsForge" (public, not a secret).
// ponytail: empty until the App's client ID is filled in; the device login stays hidden until then.
pub const OFFICIAL_CLIENT_ID: &str = "";
/// Client ID of the GitHub App "SurvivorsForge Local", for debug builds against the local platform.
pub const LOCAL_CLIENT_ID: &str = "";

/// The client ID to use: `SURVIVORSFORGE_GITHUB_CLIENT_ID`, else the one of this build; `None`
/// when there is none, then the device login is not offered.
pub fn client_id() -> Option<String> {
    let built = if cfg!(debug_assertions) { LOCAL_CLIENT_ID } else { OFFICIAL_CLIENT_ID };
    let id = std::env::var("SURVIVORSFORGE_GITHUB_CLIENT_ID").ok().filter(|id| !id.trim().is_empty()).unwrap_or_else(|| built.to_string());
    (!id.trim().is_empty()).then(|| id.trim().to_string())
}

/// What GitHub hands out to start a device login.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    /// What the user types at `verification_uri`, e.g. `WDJB-MJHT`.
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds between two polls.
    pub interval: u64,
    /// Seconds the code is good for.
    pub expires_in: u64,
}

pub fn parse_device_code(json: &str) -> Option<DeviceCode> {
    let v: Value = serde_json::from_str(json).ok()?;
    Some(DeviceCode {
        device_code: v["device_code"].as_str()?.to_string(),
        user_code: v["user_code"].as_str()?.to_string(),
        verification_uri: v["verification_uri"].as_str()?.to_string(),
        interval: v["interval"].as_u64().unwrap_or(5),
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
    })
}

/// A token answer of GitHub's `access_token` endpoint (`expires_in` in seconds) as a `GitHubToken`.
pub fn token_from_answer(v: &Value, now: DateTime<Utc>) -> Option<GitHubToken> {
    let at = |key: &str| v[key].as_i64().map(|s| (now + Span::seconds(s)).to_rfc3339());
    Some(GitHubToken {
        access_token: v["access_token"].as_str().filter(|t| !t.is_empty())?.to_string(),
        expires_at: at("expires_in"),
        refresh_token: v["refresh_token"].as_str().map(str::to_string),
        refresh_token_expires_at: at("refresh_token_expires_in"),
        source: TokenSource::Device,
        login: String::new(),
    })
}

/// One answer while waiting for the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Token(GitHubToken),
    Pending,
    /// Asked too often: wait this many seconds between polls from now on.
    SlowDown(u64),
    Expired,
    Denied,
    Failed(String),
}

pub fn poll_answer(json: &str, interval: u64, now: DateTime<Utc>) -> Poll {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Poll::Failed("Unexpected answer from GitHub".into()) };
    if let Some(token) = token_from_answer(&v, now) {
        return Poll::Token(token);
    }
    match v["error"].as_str().unwrap_or_default() {
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown(v["interval"].as_u64().unwrap_or(interval + 5).max(interval + 5)),
        "expired_token" => Poll::Expired,
        "access_denied" => Poll::Denied,
        other => Poll::Failed(v["error_description"].as_str().unwrap_or(other).to_string()),
    }
}

fn post(client: &Client, url: &str, body: &Value) -> Result<String, LoginError> {
    client.post_json(url, None, &[("Accept", "application/json")], body).map_err(|e| LoginError::Network(e.to_string()))
}

/// Asks GitHub for a device code.
pub fn start(client: &Client, oauth: &str, client_id: &str) -> Result<DeviceCode, LoginError> {
    let text = post(client, &format!("{oauth}/login/device/code"), &json!({ "client_id": client_id }))?;
    parse_device_code(&text).ok_or(LoginError::Invalid)
}

/// Waits for the user to enter the code, polling as GitHub asks; `sleep` waits between polls.
pub fn wait(client: &Client, oauth: &str, client_id: &str, code: &DeviceCode, cancel: &AtomicBool, sleep: impl Fn(Duration)) -> Result<GitHubToken, LoginError> {
    let mut interval = code.interval;
    let mut waited = 0;
    let body = json!({ "client_id": client_id, "device_code": code.device_code, "grant_type": "urn:ietf:params:oauth:grant-type:device_code" });
    loop {
        // In steps of a tenth, so Cancel does not wait for a whole interval.
        for _ in 0..10 {
            if cancel.load(Ordering::Relaxed) {
                return Err(LoginError::Cancelled);
            }
            sleep(Duration::from_millis(interval * 100));
        }
        waited += interval;
        let text = post(client, &format!("{oauth}/login/oauth/access_token"), &body)?;
        match poll_answer(&text, interval, Utc::now()) {
            Poll::Token(token) => return Ok(token),
            Poll::Pending if waited < code.expires_in => {}
            Poll::Pending | Poll::Expired => return Err(LoginError::Timeout),
            Poll::SlowDown(next) => interval = next,
            Poll::Denied => return Err(LoginError::Denied),
            Poll::Failed(why) => return Err(LoginError::Network(why)),
        }
    }
}

/// Renews a device flow token with GitHub: no client secret needed for these.
pub fn renew(client: &Client, oauth: &str, client_id: &str, refresh_token: &str, now: DateTime<Utc>) -> Option<GitHubToken> {
    let body = json!({ "client_id": client_id, "grant_type": "refresh_token", "refresh_token": refresh_token });
    let text = post(client, &format!("{oauth}/login/oauth/access_token"), &body).ok()?;
    token_from_answer(&serde_json::from_str(&text).ok()?, now)
}

/// The GitHub login name of `token`, from GitHub's REST API.
pub fn login_name(client: &Client, api: &str, token: &str) -> Option<String> {
    let text = client.get_text_as(&format!("{api}/user"), token).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["login"].as_str().map(str::to_string)
}

/// Keeps a device flow token for `api` in the keyring; a platform login there stays.
pub fn save(store: &dyn SecretStore, api: &str, token: GitHubToken) -> Result<Account, LoginError> {
    let mut account = Account::load(store, api).unwrap_or_default();
    account.github = Some(token);
    account.save(store, api).map_err(LoginError::Store)?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::MemoryStore;
    use crate::test_server::serve;

    #[test]
    fn device_code_answer_is_read() {
        let json = r#"{"device_code":"3584d83","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
        let code = parse_device_code(json).unwrap();
        assert_eq!(code, DeviceCode { device_code: "3584d83".into(), user_code: "WDJB-MJHT".into(), verification_uri: "https://github.com/login/device".into(), interval: 5, expires_in: 900 });
        assert_eq!(parse_device_code(r#"{"error":"device_flow_disabled"}"#), None);
    }

    #[test]
    fn poll_answers_are_told_apart() {
        let now = DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z").unwrap().with_timezone(&Utc);
        let token = r#"{"access_token":"ghu_d","expires_in":28800,"refresh_token":"ghr_d","refresh_token_expires_in":15897600,"token_type":"bearer"}"#;
        let Poll::Token(t) = poll_answer(token, 5, now) else { panic!("a token") };
        assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref(), t.source), ("ghu_d", Some("ghr_d"), TokenSource::Device));
        assert_eq!(t.expires_at.as_deref(), Some("2026-11-08T20:00:00+00:00"));
        assert_eq!(poll_answer(r#"{"error":"authorization_pending"}"#, 5, now), Poll::Pending);
        assert_eq!(poll_answer(r#"{"error":"slow_down","interval":10}"#, 5, now), Poll::SlowDown(10));
        assert_eq!(poll_answer(r#"{"error":"slow_down"}"#, 5, now), Poll::SlowDown(10), "old interval + 5");
        assert_eq!(poll_answer(r#"{"error":"expired_token"}"#, 5, now), Poll::Expired);
        assert_eq!(poll_answer(r#"{"error":"access_denied"}"#, 5, now), Poll::Denied);
        assert_eq!(poll_answer(r#"{"error":"device_flow_disabled","error_description":"Device Flow must be explicitly enabled for this App"}"#, 5, now), Poll::Failed("Device Flow must be explicitly enabled for this App".into()));
    }

    #[test]
    fn flow_ends_with_saved_github_token() {
        let code = r#"{"device_code":"dc","user_code":"AB-CD","verification_uri":"https://github.com/login/device","expires_in":900,"interval":1}"#;
        let pending = r#"{"error":"authorization_pending"}"#;
        let token = r#"{"access_token":"ghu_d","expires_in":28800,"refresh_token":"ghr_d","refresh_token_expires_in":15897600}"#;
        let (oauth, seen) = serve(vec![(200, vec![], code.into()), (200, vec![], pending.into()), (200, vec![], pending.into()), (200, vec![], token.into())]);
        let client = Client::new();
        let device = start(&client, &oauth, "Iv1.test").unwrap();
        assert_eq!(device.user_code, "AB-CD");
        let github = wait(&client, &oauth, "Iv1.test", &device, &AtomicBool::new(false), |_| {}).unwrap();
        let store = MemoryStore::default();
        let account = save(&store, "https://api.test", github).unwrap();
        assert_eq!(account.platform, None);
        assert_eq!(Account::load(&store, "https://api.test").unwrap().github.unwrap().source, TokenSource::Device);
        let first = seen.recv().unwrap();
        assert_eq!(first.line, "POST /login/device/code HTTP/1.1");
        assert_eq!(first.header("accept"), Some("application/json"));
        let poll: Value = serde_json::from_str(&seen.recv().unwrap().body).unwrap();
        assert_eq!(poll, json!({ "client_id": "Iv1.test", "device_code": "dc", "grant_type": "urn:ietf:params:oauth:grant-type:device_code" }));
    }

    #[test]
    fn denied_and_cancelled_end_the_wait() {
        let device = DeviceCode { device_code: "dc".into(), user_code: "AB".into(), verification_uri: String::new(), interval: 1, expires_in: 900 };
        let (oauth, _seen) = serve(vec![(200, vec![], r#"{"error":"access_denied"}"#.into())]);
        assert_eq!(wait(&Client::new(), &oauth, "id", &device, &AtomicBool::new(false), |_| {}), Err(LoginError::Denied));
        assert_eq!(wait(&Client::new(), "http://127.0.0.1:1", "id", &device, &AtomicBool::new(true), |_| {}), Err(LoginError::Cancelled));
    }
}
