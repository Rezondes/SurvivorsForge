//! Logging in to the platform from the launcher, like the website does:
//!
//! 1. Open the browser on `{api}/v1/auth/github/start` with our loopback `RETURN_TO` and a PKCE
//!    challenge of our own, so no other local program can use the code it gets back.
//! 2. Wait on `127.0.0.1:47616` for the browser to come back with `?code=` (or `?error=`).
//! 3. Exchange the code with the verifier for the bearer token and the user's GitHub token.
//! 4. Keep both in the keyring (`account`).

use crate::account::{Account, GitHubToken, SecretStore, Session};
use crate::github::Client;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where the browser comes back to; the API lists it in `AUTH_RETURN_URLS`.
pub const PORT: u16 = 47616;
pub const RETURN_TO: &str = "http://127.0.0.1:47616/auth/callback";
/// How long the launcher waits for the browser.
pub const WAIT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum LoginError {
    /// Another program listens on `PORT`.
    PortTaken,
    /// The user said no on GitHub.
    Denied,
    /// The account is banned on the platform.
    Banned,
    /// The browser came back with something we did not ask for.
    Invalid,
    Timeout,
    Cancelled,
    Network(String),
    Store(String),
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginError::PortTaken => write!(f, "Port {PORT} is in use by another program, so the login cannot come back"),
            LoginError::Denied => write!(f, "The login was cancelled on GitHub"),
            LoginError::Banned => write!(f, "This account is banned"),
            LoginError::Invalid => write!(f, "The login came back incomplete, try again"),
            LoginError::Timeout => write!(f, "The login took too long, try again"),
            LoginError::Cancelled => write!(f, "Login cancelled"),
            LoginError::Network(e) => write!(f, "{e}"),
            LoginError::Store(e) => write!(f, "The login cannot be saved in the system keyring: {e}"),
        }
    }
}

/// 32 random bytes from the operating system, base64url.
pub fn new_verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system gives random bytes");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 S256.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The page the browser opens first.
pub fn start_url(api: &str, challenge: &str) -> String {
    format!("{api}/v1/auth/github/start?return_to={}&code_challenge={}", encode(RETURN_TO), encode(challenge))
}

/// The login code from the browser's request line `GET /auth/callback?code=... HTTP/1.1`.
pub fn parse_callback(line: &str) -> Result<String, LoginError> {
    let target = line.strip_prefix("GET ").and_then(|r| r.split(' ').next()).ok_or(LoginError::Invalid)?;
    let query = target.strip_prefix("/auth/callback?").ok_or(LoginError::Invalid)?;
    let param = |key: &str| query.split('&').find_map(|p| p.strip_prefix(key)?.strip_prefix('=')).map(str::to_string);
    match (param("code"), param("error").as_deref()) {
        (Some(code), None) if !code.is_empty() => Ok(code),
        (_, Some("banned")) => Err(LoginError::Banned),
        (_, Some(_)) => Err(LoginError::Denied),
        _ => Err(LoginError::Invalid),
    }
}

/// Opens the loopback port the browser comes back to.
pub fn listen() -> Result<TcpListener, LoginError> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, PORT))).map_err(|_| LoginError::PortTaken)
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>SurvivorsForge</title>\
<body style=\"font-family:sans-serif;background:#16110c;color:#e8d8b0;text-align:center;padding-top:15vh\">\
<h1>Login done</h1><p>You can close this tab and go back to the launcher.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>SurvivorsForge</title>\
<body style=\"font-family:sans-serif;background:#16110c;color:#e8d8b0;text-align:center;padding-top:15vh\">\
<h1>Login not completed</h1><p>Go back to the launcher and try again.</p>";

/// Waits for the browser on `listener` until `timeout` or `cancel`; answers it with a short page.
/// Requests for other paths (a favicon) get a 404 and the wait goes on.
pub fn wait_for_code(listener: TcpListener, timeout: Duration, cancel: &AtomicBool) -> Result<String, LoginError> {
    listener.set_nonblocking(true).map_err(|e| LoginError::Network(e.to_string()))?;
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(LoginError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(LoginError::Timeout);
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(LoginError::Network(e.to_string())),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        if !line.starts_with("GET /auth/callback") {
            let _ = (&stream).write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let result = parse_callback(line.trim_end());
        let page = if result.is_ok() { DONE_PAGE } else { FAILED_PAGE };
        let answer = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        let _ = (&stream).write_all(answer.as_bytes());
        return result;
    }
}

/// Trades the login code for the account, with the login name from `/v1/me`: shown at once, so
/// the settings never fall back to "Log in" between the login and the next check.
pub fn exchange(client: &Client, api: &str, code: &str, verifier: &str) -> Result<Account, LoginError> {
    let body = json!({ "code": code, "return_to": RETURN_TO, "code_verifier": verifier, "github_token": true });
    let text = client.post_json(&format!("{api}/v1/auth/exchange"), None, &[], &body).map_err(|e| LoginError::Network(e.to_string()))?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| LoginError::Network(e.to_string()))?;
    let token = answer["token"].as_str().filter(|t| !t.is_empty()).ok_or(LoginError::Invalid)?;
    let login = client.get_text_as(&format!("{api}/v1/me"), token).ok().and_then(|t| crate::controller::parse_me(&t)).map(|me| me.login).unwrap_or_default();
    let session = Session { token: token.to_string(), expires_at: answer["expires_at"].as_str().unwrap_or_default().to_string(), login };
    let github = serde_json::from_value::<GitHubToken>(answer["github_token"].clone()).ok();
    Ok(Account { platform: Some(session), github })
}

/// The whole login: `open` gets the address for the browser. Saves the account.
pub fn login(client: &Client, store: &dyn SecretStore, api: &str, open: impl FnOnce(&str), cancel: &AtomicBool) -> Result<Account, LoginError> {
    let listener = listen()?;
    let verifier = new_verifier();
    open(&start_url(api, &challenge(&verifier)));
    let code = wait_for_code(listener, WAIT, cancel)?;
    let account = exchange(client, api, &code, &verifier)?;
    account.save(store, api).map_err(LoginError::Store)?;
    Ok(account)
}

/// Ends the session on the platform if it answers, and forgets the account either way.
pub fn logout(client: &Client, store: &dyn SecretStore, api: &str) -> Result<(), LoginError> {
    if let Some(session) = Account::load(store, api).and_then(|a| a.platform) {
        let _ = client.post_json(&format!("{api}/v1/auth/logout"), Some(&session.token), &[], &json!({}));
    }
    Account::delete(store, api).map_err(LoginError::Store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::MemoryStore;
    use crate::test_server::serve;

    #[test]
    fn callback_line_gives_code_or_error() {
        assert_eq!(parse_callback("GET /auth/callback?code=abc HTTP/1.1"), Ok("abc".into()));
        assert_eq!(parse_callback("GET /auth/callback?error=denied HTTP/1.1"), Err(LoginError::Denied));
        assert_eq!(parse_callback("GET /auth/callback?error=banned HTTP/1.1"), Err(LoginError::Banned));
        assert_eq!(parse_callback("GET /auth/callback?code= HTTP/1.1"), Err(LoginError::Invalid));
        assert_eq!(parse_callback("GET /other?code=abc HTTP/1.1"), Err(LoginError::Invalid));
        assert_eq!(parse_callback("POST /auth/callback?code=abc HTTP/1.1"), Err(LoginError::Invalid));
    }

    #[test]
    fn start_url_carries_challenge_of_verifier() {
        // RFC 7636, appendix B.
        let c = challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(
            start_url("https://api.test", &c),
            "https://api.test/v1/auth/github/start?return_to=http%3A%2F%2F127.0.0.1%3A47616%2Fauth%2Fcallback&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = new_verifier();
        assert_eq!(v.len(), 43);
        assert_ne!(v, new_verifier(), "random each time");
    }

    #[test]
    fn exchange_sends_verifier_and_reads_account() {
        let answer = r#"{"token":"bear","expires_at":"2026-12-01T00:00:00Z","github_token":{"access_token":"ghu_1","expires_at":"2026-11-08T20:00:00Z","refresh_token":"ghr_1","refresh_token_expires_at":null}}"#;
        let me = r#"{"id":1,"github_id":9,"login":"octo","avatar_url":"https://avatars.test/9","role":"user"}"#;
        let (api, seen) = serve(vec![(200, vec![], answer.into()), (200, vec![], me.into())]);
        let account = exchange(&Client::new(), &api, "the-code", "the-verifier").unwrap();
        let request = seen.recv().unwrap();
        assert_eq!(request.line, "POST /v1/auth/exchange HTTP/1.1");
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body, json!({ "code": "the-code", "return_to": RETURN_TO, "code_verifier": "the-verifier", "github_token": true }));
        let me = seen.recv().unwrap();
        assert_eq!((me.line.as_str(), me.header("authorization")), ("GET /v1/me HTTP/1.1", Some("Bearer bear")));
        let session = account.platform.unwrap();
        assert_eq!((session.token.as_str(), session.login.as_str()), ("bear", "octo"), "the name comes with the login");
        let github = account.github.unwrap();
        assert_eq!((github.access_token.as_str(), github.refresh_token.as_deref()), ("ghu_1", Some("ghr_1")));
    }

    #[test]
    fn exchange_refused_tells_why() {
        let (api, _seen) = serve(vec![(400, vec![], r#"{"error":"invalid_code","message":"The login code is invalid or expired"}"#.into())]);
        let err = exchange(&Client::new(), &api, "c", "v").unwrap_err();
        assert_eq!(err, LoginError::Network("The login code is invalid or expired".into()));
    }

    #[test]
    fn waiting_ends_after_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(wait_for_code(listener, Duration::from_millis(100), &AtomicBool::new(false)), Err(LoginError::Timeout));
        assert!(TcpListener::bind(addr).is_ok(), "the port is free again");
    }

    #[test]
    fn waiting_stops_on_cancel() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        assert_eq!(wait_for_code(listener, Duration::from_secs(10), &AtomicBool::new(true)), Err(LoginError::Cancelled));
    }

    #[test]
    fn browser_coming_back_gives_the_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let browser = std::thread::spawn(move || {
            let favicon = ureq::get(&format!("http://{addr}/favicon.ico")).call();
            assert!(matches!(favicon, Err(ureq::Error::Status(404, _))));
            ureq::get(&format!("http://{addr}/auth/callback?code=xyz")).call().unwrap().into_string().unwrap()
        });
        assert_eq!(wait_for_code(listener, Duration::from_secs(10), &AtomicBool::new(false)), Ok("xyz".into()));
        assert!(browser.join().unwrap().contains("Login done"));
    }

    #[test]
    fn logout_forgets_account_when_api_is_down() {
        let store = MemoryStore::default();
        let api = "http://127.0.0.1:1";
        let session = Session { token: "t".into(), expires_at: String::new(), login: "u".into() };
        Account { platform: Some(session), github: None }.save(&store, api).unwrap();
        logout(&Client::new(), &store, api).unwrap();
        assert_eq!(Account::load(&store, api), None);
    }

    #[test]
    fn logout_ends_the_platform_session() {
        let (api, seen) = serve(vec![(204, vec![], String::new())]);
        let store = MemoryStore::default();
        let session = Session { token: "tok".into(), expires_at: String::new(), login: "u".into() };
        Account { platform: Some(session), github: None }.save(&store, &api).unwrap();
        logout(&Client::new(), &store, &api).unwrap();
        let request = seen.recv().unwrap();
        assert_eq!(request.line, "POST /v1/auth/logout HTTP/1.1");
        assert_eq!(request.header("authorization"), Some("Bearer tok"));
    }
}
