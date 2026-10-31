pub mod config;
pub mod controller;
pub mod deep_link;
pub mod github;
pub mod install;
pub mod pictures;
pub mod registry;
pub mod repo_detect;
pub mod scan;
pub mod self_update;
pub mod state;
pub mod telemetry;
pub mod updates;

pub use forge_core::{archive, toc};

use std::fmt;
use std::path::Path;

pub const APP_NAME: &str = "SurvivorsForge";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// GitHub repo of SurvivorsForge itself: hosts the releases for self-update.
pub const APP_REPO: &str = "Rezondes/SurvivorsForge";

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Io(String),
    Zip(String),
    UnsafePath(String),
    NoAddonFound,
    NotFound,
    RateLimited,
    Http(u16),
    Network(String),
    Parse(String),
    NoRelease,
    NoTags,
    NoMatchingAsset,
    FolderOwnedBy(String),
    AlreadyVerified(String),
    InvalidRepo(String),
    /// The platform does not let this version be installed (blocked, or changed on GitHub).
    NotInstallable(String),
    /// The downloaded file is not the one the platform scanned.
    HashMismatch { expected: String, actual: String },
    /// A download URL outside `github::DOWNLOAD_HOSTS`.
    ForbiddenHost(String),
    /// The platform cannot be reached, so nothing is installed or updated.
    Offline,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "File error: {e}"),
            Error::Zip(e) => write!(f, "The zip file is damaged or invalid: {e}"),
            Error::UnsafePath(p) => write!(f, "The zip contains an unsafe path ({p}), installation cancelled"),
            Error::NoAddonFound => write!(f, "No addon found"),
            Error::NotFound => write!(f, "Not found on GitHub"),
            Error::RateLimited => write!(f, "GitHub is limiting requests right now. Try again in a few minutes"),
            Error::Http(code) => write!(f, "The server answered with error {code}"),
            Error::Network(e) => write!(f, "Network error: {e}"),
            Error::Parse(e) => write!(f, "Unexpected response: {e}"),
            Error::NoRelease => write!(f, "The repo has no release"),
            Error::NoTags => write!(f, "The repo has no tags"),
            Error::NoMatchingAsset => write!(f, "The latest release has no matching addon zip"),
            Error::FolderOwnedBy(name) => write!(f, "The folder already belongs to the addon \"{name}\""),
            Error::AlreadyVerified(name) => write!(f, "\"{name}\" is a verified addon, install it from Browse"),
            Error::NotInstallable(why) => write!(f, "{why}"),
            Error::HashMismatch { expected, actual } => write!(
                f,
                "The download is not the file that was checked (expected {}..., got {}...), nothing was installed",
                &expected[..expected.len().min(12)],
                &actual[..actual.len().min(12)]
            ),
            Error::ForbiddenHost(url) => write!(f, "Downloads are only allowed from GitHub, not from {url}"),
            Error::Offline => write!(f, "The SurvivorsForge server cannot be reached. Installing and updating resume when it is back"),
            Error::InvalidRepo(input) => write!(f, "\"{input}\" is not a GitHub repo (expected owner/repo or a GitHub URL)"),
        }
    }
}

impl From<forge_core::Error> for Error {
    fn from(e: forge_core::Error) -> Self {
        match e {
            forge_core::Error::Io(e) => Error::Io(e),
            forge_core::Error::Zip(e) => Error::Zip(e),
            forge_core::Error::UnsafePath(p) => Error::UnsafePath(p),
            forge_core::Error::NoAddonFound => Error::NoAddonFound,
            forge_core::Error::Parse(e) => Error::Parse(e),
            e @ (forge_core::Error::FolderTwice(_) | forge_core::Error::TooLarge(_)) => Error::NotInstallable(e.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Writes via a temp file and rename, so a crash never leaves a half-written file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unique-enough name for temp folders and files of this process.
pub fn unique_name() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", std::process::id(), nanos, COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_survivorsforge() {
        assert_eq!(APP_NAME, "SurvivorsForge");
    }

    #[test]
    fn error_messages_are_human_readable() {
        assert_eq!(Error::NoAddonFound.to_string(), "No addon found");
        assert!(Error::RateLimited.to_string().contains("limiting requests"));
        assert!(Error::FolderOwnedBy("Survivor UI".into()).to_string().contains("Survivor UI"));
    }
}
