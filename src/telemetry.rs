//! Anonymous counts for the platform's statistics: installs and updates of verified addons, and
//! failed ones with a fixed error code. Two settings turn it off: `telemetry` stops every event,
//! `send_install_id` only the random installation ID.

use crate::config::Config;
use crate::state::Source;
use crate::Error;
use serde::Serialize;
use std::hash::{BuildHasher, Hasher};

/// The header that carries the installation ID to the platform API, never to GitHub.
pub const INSTALL_HEADER: &str = "X-SurvivorsForge-Install";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub kind: &'static str,
    pub addon: String,
    pub version: String,
    pub launcher_version: &'static str,
    pub os: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
}

/// The event of one install or update of the verified addon `registry_id`, or `None` for errors
/// that say nothing about the addon (no connection to the platform at all).
pub fn event(update: bool, registry_id: &str, version: &str, result: Result<(), &Error>) -> Option<Event> {
    let (kind, error_code) = match (update, result) {
        (false, Ok(())) => ("install", None),
        (true, Ok(())) => ("update", None),
        (_, Err(Error::Offline)) => return None,
        (false, Err(e)) => ("install_failed", Some(error_code(e))),
        (true, Err(e)) => ("update_failed", Some(error_code(e))),
    };
    Some(Event {
        kind,
        addon: registry_id.to_string(),
        version: version.to_string(),
        launcher_version: crate::APP_VERSION,
        os: if cfg!(windows) { "windows" } else { "linux" },
        error_code,
    })
}

/// A fixed word per error: the message itself may hold paths or names, so it never leaves.
pub fn error_code(error: &Error) -> &'static str {
    match error {
        Error::Io(_) => "io",
        Error::Zip(_) => "zip_invalid",
        Error::UnsafePath(_) => "unsafe_path",
        Error::NoAddonFound => "no_addon_found",
        Error::NotFound => "not_found",
        Error::RateLimited => "rate_limited",
        Error::Http(_) => "http_error",
        Error::Network(_) => "network",
        Error::Parse(_) => "parse",
        Error::NoRelease => "no_release",
        Error::NoTags => "no_tags",
        Error::NoMatchingAsset => "no_matching_asset",
        Error::FolderOwnedBy(_) => "folder_owned",
        Error::AlreadyVerified(_) => "already_verified",
        Error::InvalidRepo(_) => "invalid_repo",
        Error::NotInstallable(_) => "not_installable",
        Error::HashMismatch { .. } => "hash_mismatch",
        Error::ForbiddenHost(_) => "forbidden_host",
        Error::Offline => "offline",
    }
}

/// The registry ID of an addon whose installs count: only verified addons from the registry, not
/// GitHub repos or zip files the platform does not know.
pub fn reported_addon(source: &Source) -> Option<&str> {
    match source {
        Source::Verified { registry_id } => Some(registry_id),
        Source::GitHub { .. } | Source::Zip { .. } => None,
    }
}

/// The ID to send with every platform request, if the user allows it.
pub fn install_header(config: &Config) -> Option<String> {
    (config.telemetry && config.send_install_id).then(|| config.install_id.clone()).flatten()
}

/// Where events go; the real one posts to the platform, tests collect them.
pub trait Sender: Send + Sync {
    fn send(&self, install_id: Option<&str>, events: &[Event]);
}

/// Hands `events` to `sender` unless the user turned telemetry off.
pub fn report(config: &Config, sender: &dyn Sender, events: &[Event]) {
    if config.telemetry && !events.is_empty() {
        sender.send(install_header(config).as_deref(), events);
    }
}

/// Posts to `{api}/v1/telemetry/events` on its own thread; a failure changes nothing for the user.
pub struct ApiSender {
    pub api: String,
}

impl Sender for ApiSender {
    fn send(&self, install_id: Option<&str>, events: &[Event]) {
        let url = format!("{}/v1/telemetry/events", self.api);
        let body = serde_json::json!({ "events": events });
        let install_id = install_id.map(str::to_string);
        std::thread::spawn(move || {
            let mut request = ureq::post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .set("User-Agent", &format!("{}/{}", crate::APP_NAME, crate::APP_VERSION))
                .set("Content-Type", "application/json");
            if let Some(id) = &install_id {
                request = request.set(INSTALL_HEADER, id);
            }
            let _ = request.send_string(&body.to_string());
        });
    }
}

/// A random version-4 UUID for a new installation.
// ponytail: std's RandomState (OS-seeded SipHash keys), not a CSPRNG; fine for an anonymous ID.
pub fn new_install_id() -> String {
    let random = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default());
        h.finish()
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&random().to_le_bytes());
    bytes[8..].copy_from_slice(&random().to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collect(Mutex<Vec<(Option<String>, Vec<Event>)>>);

    impl Sender for Collect {
        fn send(&self, install_id: Option<&str>, events: &[Event]) {
            self.0.lock().unwrap().push((install_id.map(str::to_string), events.to_vec()));
        }
    }

    fn config(telemetry: bool, send_install_id: bool) -> Config {
        Config { telemetry, send_install_id, install_id: Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301".into()), ..Default::default() }
    }

    #[test]
    fn builds_event_from_install() {
        let e = event(false, "lucky", "v1.2.0", Ok(())).unwrap();
        assert_eq!((e.kind, e.addon.as_str(), e.version.as_str(), e.error_code), ("install", "lucky", "v1.2.0", None));
        assert_eq!(e.launcher_version, crate::APP_VERSION);
        assert_eq!(event(true, "lucky", "v2", Ok(())).unwrap().kind, "update");
        let failed = event(true, "lucky", "v2", Err(&Error::HashMismatch { expected: "a".into(), actual: "b".into() })).unwrap();
        assert_eq!((failed.kind, failed.error_code), ("update_failed", Some("hash_mismatch")));
        assert_eq!(event(false, "lucky", "v2", Err(&Error::Offline)), None, "offline says nothing about the addon");
    }

    #[test]
    fn error_codes_have_no_paths() {
        let code = error_code(&Error::Io("C:\\Users\\me\\AddOns is read-only".into()));
        assert_eq!(code, "io");
        let json = serde_json::to_string(&event(false, "x", "v1", Err(&Error::FolderOwnedBy("My Secret".into())))).unwrap();
        assert!(!json.contains("Secret"), "{json}");
    }

    #[test]
    fn disabled_sends_nothing() {
        let sender = Collect::default();
        let events = [event(false, "lucky", "v1", Ok(())).unwrap()];
        report(&config(false, true), &sender, &events);
        assert!(sender.0.lock().unwrap().is_empty());
        report(&config(true, true), &sender, &[]);
        assert!(sender.0.lock().unwrap().is_empty(), "nothing to send");
    }

    #[test]
    fn id_header_only_when_allowed() {
        let sender = Collect::default();
        let events = [event(false, "lucky", "v1", Ok(())).unwrap()];
        report(&config(true, true), &sender, &events);
        report(&config(true, false), &sender, &events);
        let sent = sender.0.lock().unwrap();
        assert_eq!(sent[0].0.as_deref(), Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301"));
        assert_eq!(sent[1].0, None, "events still go, without the ID");
        assert_eq!(install_header(&config(false, true)), None, "telemetry off hides the ID everywhere");
    }

    #[test]
    fn zip_installs_not_reported() {
        assert_eq!(reported_addon(&Source::Verified { registry_id: "lucky".into() }), Some("lucky"));
        assert_eq!(reported_addon(&Source::Zip { file_name: "my.zip".into() }), None);
    }

    #[test]
    fn install_ids_are_v4_uuids() {
        let (a, b) = (new_install_id(), new_install_id());
        assert_ne!(a, b);
        let groups: Vec<&str> = a.split('-').collect();
        assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(groups[2].starts_with('4') && "89ab".contains(&groups[3][..1]), "{a}");
    }
}
