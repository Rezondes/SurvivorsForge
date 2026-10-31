//! The list of addons: the platform API (`/v1/registry`), else the last good cached copy, else
//! the copy compiled into the binary. Without the API the launcher is offline: it shows the cached
//! list and checks the addons on github.com directly (`crate::updates::check_all_direct`).

pub use forge_core::registry::*;

use crate::Result;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub const EMBEDDED: &str = include_str!("../registry/verified.json");
const CACHE_FILE: &str = "verified.json";
/// The SurvivorsForge platform, for release builds without `SURVIVORSFORGE_API_URL` and
/// `SURVIVORSFORGE_WEB_URL` at build time.
pub const OFFICIAL_API_URL: &str = "https://forgeapi.rezondes.net";
pub const OFFICIAL_WEB_URL: &str = "https://forge.rezondes.net";
/// Where `docs/local-setup.md` runs the platform; debug builds (`cargo run`) use it.
pub const LOCAL_API_URL: &str = "http://localhost:3081";
pub const LOCAL_WEB_URL: &str = "http://localhost:3080";

/// The address without anything else set: the local platform for debug builds, the official one
/// for release builds.
pub fn fallback_api_url() -> &'static str {
    if cfg!(debug_assertions) { LOCAL_API_URL } else { OFFICIAL_API_URL }
}

pub fn fallback_web_url() -> &'static str {
    if cfg!(debug_assertions) { LOCAL_WEB_URL } else { OFFICIAL_WEB_URL }
}

/// The platform API: `SURVIVORSFORGE_API_URL` at run time, else `setting` from the settings, else
/// the address compiled in by the release build, else the fallback.
pub fn api_url(setting: Option<&str>) -> String {
    let runtime = std::env::var("SURVIVORSFORGE_API_URL").ok();
    pick_url(&[runtime.as_deref(), setting, option_env!("SURVIVORSFORGE_API_URL")], fallback_api_url())
}

/// The platform website, for links the launcher opens in the browser; chosen like `api_url`.
pub fn web_url(setting: Option<&str>) -> String {
    let runtime = std::env::var("SURVIVORSFORGE_WEB_URL").ok();
    pick_url(&[runtime.as_deref(), setting, option_env!("SURVIVORSFORGE_WEB_URL")], fallback_web_url())
}

/// The first non-empty candidate, else `fallback`.
fn pick_url(candidates: &[Option<&str>], fallback: &str) -> String {
    candidates
        .iter()
        .flatten()
        .map(|u| u.trim().trim_end_matches('/'))
        .find(|u| !u.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub fn registry_url(api: &str) -> String {
    format!("{api}/v1/registry")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Origin {
    /// The platform API: every addon with its trust level.
    Api,
    Cache,
    Embedded,
}

pub fn embedded() -> Registry {
    Registry::parse(EMBEDDED).expect("embedded registry is valid")
}

/// The API's answer if it is a valid registry (it also refreshes the cache), else the cache, else
/// the built-in copy. `None` loads without asking the API (at start-up).
pub fn load(fetched: Option<Result<String>>, cache_dir: &Path) -> (Registry, Origin) {
    let cache = cache_dir.join(CACHE_FILE);
    if let Some(Ok(text)) = fetched
        && let Ok(registry) = Registry::parse(&text)
    {
        let _ = crate::write_atomic(&cache, text.as_bytes());
        return (registry, Origin::Api);
    }
    if let Some(registry) = std::fs::read_to_string(&cache).ok().and_then(|t| Registry::parse(&t).ok()) {
        return (registry, Origin::Cache);
    }
    (embedded(), Origin::Embedded)
}

/// How old the cached list is, for the offline notice.
pub fn cache_age(cache_dir: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(cache_dir.join(CACHE_FILE)).and_then(|m| m.modified()).ok()?;
    now.duration_since(modified).ok()
}

/// "5 minutes", "3 hours", "2 days".
pub fn describe_age(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    let (n, unit) = match minutes {
        0..60 => (minutes, "minute"),
        60..1440 => (minutes / 60, "hour"),
        _ => (minutes / 1440, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn embedded_registry_is_valid_and_complete() {
        let registry = embedded();
        let repos: Vec<&str> = registry.addons.iter().map(|a| a.repo.as_str()).collect();
        assert_eq!(repos, [
            "morderous/survivor-ui",
            "cadoky/LuckyOdds",
            "cadoky/ASBuilder",
            "Kindagrey/Azerloot",
            "KeijinDE/KeijinUI_modern-as-ui",
            "threenox/BordoUI",
            "NORehabRocket/AetherHUD",
            "Blackwellcode/BlackwellUI",
            "migwynkriid13/SurvivorsDragonUi",
            "kex0/AZSSpellMeter",
            "Maidenquinn/AzerothStats",
            "Maidenquinn/azeroth-fieldkit",
        ]);
        assert_eq!(registry.get("survivor-ui").unwrap().folders, ["SurvivorUI", "SurvivorPlates"]);
        assert_eq!(registry.get("azerothstats").unwrap().source, SourceSpec::Tag);
        assert_eq!(registry.get("bordoui").unwrap().source, SourceSpec::Release { asset: "BordoUI.zip".into() });
    }

    #[test]
    fn api_then_cache_then_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let offline = || Some(Err(Error::Network("offline".into())));
        assert_eq!(load(offline(), dir.path()), (embedded(), Origin::Embedded));

        let api = r#"{"schema":1,"addons":[{"id":"api","name":"A","author":"a","repo":"o/a","folders":["A"],"source":{"type":"tag"},"trust":"scanned"}]}"#;
        let (registry, origin) = load(Some(Ok(api.into())), dir.path());
        assert_eq!((origin, registry.addons.len()), (Origin::Api, 1));
        assert_eq!(registry.addons[0].trust, Some(forge_core::trust::Trust::Scanned));

        let (registry, origin) = load(offline(), dir.path());
        assert_eq!((origin, registry.addons.len()), (Origin::Cache, 1), "the cached API answer");
        let (_, origin) = load(Some(Ok("<html>502</html>".into())), dir.path());
        assert_eq!(origin, Origin::Cache, "a broken API answer falls back to the cache");
        assert_eq!(load(None, dir.path()).1, Origin::Cache, "start-up reads the cache");
        assert!(cache_age(dir.path(), SystemTime::now() + Duration::from_secs(120)).unwrap() >= Duration::from_secs(119));
    }

    #[test]
    fn api_url_environment_then_setting_then_build_then_fallback() {
        let pick = |env, setting, build| pick_url(&[env, setting, build], OFFICIAL_API_URL);
        assert_eq!(pick(Some("http://localhost:8080/"), Some("https://mine"), Some("https://api.real")), "http://localhost:8080");
        assert_eq!(pick(Some(" "), Some("https://mine/"), Some("https://api.real")), "https://mine", "the setting beats the build");
        assert_eq!(pick(None, None, Some("https://api.real")), "https://api.real");
        assert_eq!(pick(None, None, None), OFFICIAL_API_URL);
        assert_eq!(registry_url("https://api.test"), "https://api.test/v1/registry");
    }

    #[test]
    fn release_builds_talk_to_the_official_platform() {
        assert_eq!(OFFICIAL_API_URL, "https://forgeapi.rezondes.net");
        assert_eq!(OFFICIAL_WEB_URL, "https://forge.rezondes.net");
    }

    #[test]
    fn debug_builds_talk_to_the_local_platform() {
        // Tests are debug builds, like `cargo run`.
        assert_eq!(fallback_api_url(), "http://localhost:3081");
        assert_eq!(fallback_web_url(), "http://localhost:3080");
        if std::env::var_os("SURVIVORSFORGE_API_URL").is_none() && option_env!("SURVIVORSFORGE_API_URL").is_none() {
            assert_eq!(api_url(None), "http://localhost:3081");
            assert_eq!(api_url(Some("https://api.example.test")), "https://api.example.test");
        }
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(describe_age(Duration::from_secs(30)), "0 minutes");
        assert_eq!(describe_age(Duration::from_secs(60)), "1 minute");
        assert_eq!(describe_age(Duration::from_secs(3 * 3600)), "3 hours");
        assert_eq!(describe_age(Duration::from_secs(2 * 86400)), "2 days");
    }
}
