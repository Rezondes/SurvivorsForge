//! Configuration from environment variables only. No config files, no secrets in the repo.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub public_api_url: String,
    pub bind_addr: SocketAddr,
    /// Where `/metrics` listens for Prometheus; Caddy never forwards to it.
    pub metrics_bind_addr: SocketAddr,
    pub migrate_on_start: bool,
    pub rate_limit_per_minute: u32,
    /// Seconds public answers stay in the API's cache; 0 turns it off.
    pub cache_seconds: u64,
    pub trusted_proxies: Vec<IpAddr>,
    /// The website's address: it limits each visitor itself, so the API does not count its requests.
    pub website_ip: Option<IpAddr>,
    pub github_api_url: String,
    /// Token for public GitHub reads outside an installation; 5000 instead of 60 requests per hour.
    pub github_read_token: Option<String>,
    pub github_app_id: String,
    /// PEM text; `\n` escapes allowed. Read by `github::app_auth::load_private_key`.
    pub github_app_private_key: Option<String>,
    pub github_app_private_key_file: Option<String>,
    pub github_oauth_url: String,
    /// github.com itself, for release and tag metadata; only tests change it.
    pub github_web_url: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    /// Exact client URLs a login may return to.
    pub auth_return_urls: Vec<String>,
    /// GitHub user IDs that are admins.
    pub admin_github_ids: Vec<i64>,
    /// Roles that see the detailed statistics on `/admin/stats`.
    pub stats_roles: Vec<forge_core::roles::Role>,
    /// Public base URL of uploaded images, e.g. `https://forge.example.com/img`.
    pub public_image_url: String,
    /// Secret of the GitHub App webhook, checked against `X-Hub-Signature-256`.
    pub github_webhook_secret: String,
    /// `owner/name` of the launcher's own repository, whose release downloads `/stats` shows.
    pub launcher_repo: String,
    /// Password of the read-only database role `grafana_reader`; without it the role cannot log in.
    pub grafana_db_password: Option<String>,
    /// Versions the source check looks at per run.
    pub source_check_batch: i64,
    /// Where uploaded images are stored; the reverse proxy serves it as `public_image_url`.
    pub image_dir: String,
    /// Where built addon packs are stored; the reverse proxy serves it as `public_pack_url`.
    pub pack_dir: String,
    pub public_pack_url: String,
    pub pack_max_addons: i64,
    /// Largest pack zip in bytes.
    pub pack_max_bytes: u64,
    /// New packs per user and hour.
    pub packs_per_hour: i64,
    /// Days a pack zip is kept without a download.
    pub pack_retention_days: i64,
    /// All pack zips together; the least recently downloaded go first.
    pub pack_storage_bytes: u64,
}

#[derive(Debug, PartialEq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Config {
    /// Every variable `from_map` reads. `.env.example` must list each of them.
    pub const VARIABLES: &[&str] = &[
        "DATABASE_URL",
        "PUBLIC_API_URL",
        "BIND_ADDR",
        "METRICS_BIND_ADDR",
        "MIGRATE_ON_START",
        "RATE_LIMIT_PER_MINUTE",
        "CACHE_SECONDS",
        "TRUSTED_PROXIES",
        "WEBSITE_IP",
        "GITHUB_API_URL",
        "GITHUB_READ_TOKEN",
        "GITHUB_APP_ID",
        "GITHUB_APP_PRIVATE_KEY",
        "GITHUB_APP_PRIVATE_KEY_FILE",
        "GITHUB_OAUTH_URL",
        "GITHUB_CLIENT_ID",
        "GITHUB_CLIENT_SECRET",
        "AUTH_RETURN_URLS",
        "ADMIN_GITHUB_IDS",
        "ADMIN_STATS_ROLES",
        "PUBLIC_IMAGE_URL",
        "GITHUB_WEBHOOK_SECRET",
        "SOURCE_CHECK_BATCH",
        "LAUNCHER_REPO",
        "GRAFANA_DB_PASSWORD",
        "IMAGE_DIR",
        "PACK_DIR",
        "PUBLIC_PACK_URL",
        "PACK_MAX_ADDONS",
        "PACK_MAX_MB",
        "PACKS_PER_HOUR",
        "PACK_RETENTION_DAYS",
        "PACK_STORAGE_MB",
    ];

    pub fn from_env() -> Result<Config, ConfigError> {
        Config::from_map(&std::env::vars().collect())
    }

    pub fn from_map(env: &HashMap<String, String>) -> Result<Config, ConfigError> {
        let get = |name: &str| env.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
        let required = |name: &str| get(name).map(str::to_string).ok_or_else(|| ConfigError(format!("{name} is not set")));
        let parse = |name: &str, default: &str| -> Result<String, ConfigError> { Ok(get(name).unwrap_or(default).to_string()) };
        let invalid = |name: &str, value: &str| ConfigError(format!("{name} has an invalid value: {value}"));
        let number = |name: &str, default: &str| -> Result<u32, ConfigError> {
            let value = parse(name, default)?;
            value.parse().map_err(|_| invalid(name, &value))
        };

        let bind = parse("BIND_ADDR", "0.0.0.0:8080")?;
        let metrics_bind = parse("METRICS_BIND_ADDR", "0.0.0.0:9100")?;
        let migrate = parse("MIGRATE_ON_START", "false")?;
        let limit = parse("RATE_LIMIT_PER_MINUTE", "120")?;
        let proxies = parse("TRUSTED_PROXIES", "")?;
        let cache = parse("CACHE_SECONDS", "60")?;
        Ok(Config {
            database_url: required("DATABASE_URL")?,
            public_api_url: required("PUBLIC_API_URL")?.trim_end_matches('/').to_string(),
            bind_addr: bind.parse().map_err(|_| invalid("BIND_ADDR", &bind))?,
            metrics_bind_addr: metrics_bind.parse().map_err(|_| invalid("METRICS_BIND_ADDR", &metrics_bind))?,
            migrate_on_start: migrate.parse().map_err(|_| invalid("MIGRATE_ON_START", &migrate))?,
            rate_limit_per_minute: limit.parse().map_err(|_| invalid("RATE_LIMIT_PER_MINUTE", &limit))?,
            cache_seconds: cache.parse().map_err(|_| invalid("CACHE_SECONDS", &cache))?,
            trusted_proxies: list(&proxies)
                .map(|p| p.parse().map_err(|_| invalid("TRUSTED_PROXIES", p)))
                .collect::<Result<_, _>>()?,
            website_ip: get("WEBSITE_IP").map(|ip| ip.parse().map_err(|_| invalid("WEBSITE_IP", ip))).transpose()?,
            github_api_url: parse("GITHUB_API_URL", "https://api.github.com")?.trim_end_matches('/').to_string(),
            github_read_token: get("GITHUB_READ_TOKEN").map(str::to_string),
            github_app_id: required("GITHUB_APP_ID")?,
            github_app_private_key: get("GITHUB_APP_PRIVATE_KEY").map(str::to_string),
            github_app_private_key_file: get("GITHUB_APP_PRIVATE_KEY_FILE").map(str::to_string),
            github_oauth_url: parse("GITHUB_OAUTH_URL", "https://github.com")?.trim_end_matches('/').to_string(),
            github_web_url: "https://github.com".into(),
            github_client_id: required("GITHUB_CLIENT_ID")?,
            github_client_secret: required("GITHUB_CLIENT_SECRET")?,
            auth_return_urls: list(&parse("AUTH_RETURN_URLS", "")?).map(str::to_string).collect(),
            admin_github_ids: list(&parse("ADMIN_GITHUB_IDS", "")?)
                .map(|id| id.parse().map_err(|_| invalid("ADMIN_GITHUB_IDS", id)))
                .collect::<Result<_, _>>()?,
            stats_roles: list(&parse("ADMIN_STATS_ROLES", "admin,moderator")?)
                .map(|role| role.parse().map_err(|_| invalid("ADMIN_STATS_ROLES", role)))
                .collect::<Result<_, _>>()?,
            public_image_url: required("PUBLIC_IMAGE_URL")?.trim_end_matches('/').to_string(),
            github_webhook_secret: required("GITHUB_WEBHOOK_SECRET")?,
            launcher_repo: parse("LAUNCHER_REPO", "Rezondes/SurvivorsForge")?,
            grafana_db_password: get("GRAFANA_DB_PASSWORD").map(str::to_string),
            source_check_batch: {
                let batch = parse("SOURCE_CHECK_BATCH", "200")?;
                batch.parse().map_err(|_| invalid("SOURCE_CHECK_BATCH", &batch))?
            },
            image_dir: parse("IMAGE_DIR", "/data/images")?,
            pack_dir: parse("PACK_DIR", "/data/packs")?,
            public_pack_url: required("PUBLIC_PACK_URL")?.trim_end_matches('/').to_string(),
            pack_max_addons: number("PACK_MAX_ADDONS", "20")?.into(),
            pack_max_bytes: u64::from(number("PACK_MAX_MB", "50")?) * 1_000_000,
            packs_per_hour: number("PACKS_PER_HOUR", "10")?.into(),
            pack_retention_days: number("PACK_RETENTION_DAYS", "7")?.into(),
            pack_storage_bytes: u64::from(number("PACK_STORAGE_MB", "2048")?) * 1_000_000,
        })
    }

    /// A config for tests: only the database differs between them.
    pub fn for_tests(database_url: &str) -> Config {
        Config {
            database_url: database_url.to_string(),
            public_api_url: "http://api.test".into(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            metrics_bind_addr: "127.0.0.1:0".parse().unwrap(),
            migrate_on_start: false,
            rate_limit_per_minute: 10_000,
            // Off: tests change the database directly and read it right back.
            cache_seconds: 0,
            trusted_proxies: Vec::new(),
            website_ip: None,
            github_api_url: "http://github.test".into(),
            github_read_token: None,
            github_app_id: "1".into(),
            github_app_private_key: None,
            github_app_private_key_file: None,
            github_oauth_url: "http://github-oauth.test".into(),
            github_web_url: "http://github-web.test".into(),
            github_client_id: "client-id".into(),
            github_client_secret: "client-secret".into(),
            auth_return_urls: vec!["https://web.test/auth/callback".into()],
            admin_github_ids: Vec::new(),
            stats_roles: vec![forge_core::roles::Role::Admin, forge_core::roles::Role::Moderator],
            public_image_url: "https://web.test/img".into(),
            github_webhook_secret: "webhook-secret".into(),
            launcher_repo: "Rezondes/SurvivorsForge".into(),
            grafana_db_password: None,
            source_check_batch: 200,
            image_dir: std::env::temp_dir().join("forge-test-images").to_string_lossy().into_owned(),
            pack_dir: std::env::temp_dir().join("forge-test-packs").to_string_lossy().into_owned(),
            public_pack_url: "https://web.test/packs-files".into(),
            pack_max_addons: 20,
            pack_max_bytes: 50_000_000,
            packs_per_hour: 10,
            pack_retention_days: 7,
            pack_storage_bytes: 2_048_000_000,
        }
    }
}

/// Comma-separated values without blanks.
fn list(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::roles::Role;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const MINIMAL: &[(&str, &str)] = &[
        ("DATABASE_URL", "postgres://x"),
        ("PUBLIC_API_URL", "https://api.example.com/"),
        ("GITHUB_APP_ID", "1"),
        ("GITHUB_CLIENT_ID", "c"),
        ("GITHUB_CLIENT_SECRET", "s"),
        ("PUBLIC_IMAGE_URL", "https://web.example.com/img"),
        ("GITHUB_WEBHOOK_SECRET", "w"),
        ("PUBLIC_PACK_URL", "https://web.example.com/packs-files/"),
    ];

    #[test]
    fn missing_database_url_names_variable() {
        let err = Config::from_map(&env(&[("PUBLIC_API_URL", "https://a"), ("GITHUB_APP_ID", "1"), ("GITHUB_CLIENT_ID", "c"), ("GITHUB_CLIENT_SECRET", "s"), ("PUBLIC_IMAGE_URL", "https://i"), ("GITHUB_WEBHOOK_SECRET", "w"), ("PUBLIC_PACK_URL", "https://p")])).unwrap_err();
        assert!(err.0.contains("DATABASE_URL"), "{}", err.0);
        let err =
            Config::from_map(&env(&[("DATABASE_URL", "postgres://x"), ("PUBLIC_API_URL", " "), ("GITHUB_APP_ID", "1"), ("GITHUB_CLIENT_ID", "c"), ("GITHUB_CLIENT_SECRET", "s"), ("PUBLIC_IMAGE_URL", "https://i"), ("GITHUB_WEBHOOK_SECRET", "w"), ("PUBLIC_PACK_URL", "https://p")])).unwrap_err();
        assert!(err.0.contains("PUBLIC_API_URL"), "{}", err.0);
    }

    #[test]
    fn migrate_on_start_defaults_to_false() {
        let config = Config::from_map(&env(MINIMAL)).unwrap();
        assert!(!config.migrate_on_start);
        assert_eq!(config.rate_limit_per_minute, 120);
        assert_eq!(config.cache_seconds, 60);
        assert_eq!((config.pack_max_addons, config.pack_max_bytes, config.packs_per_hour), (20, 50_000_000, 10));
        assert_eq!((config.pack_retention_days, config.pack_storage_bytes), (7, 2_048_000_000));
        assert_eq!(config.public_pack_url, "https://web.example.com/packs-files");
        assert_eq!(config.public_api_url, "https://api.example.com");
        let mut on = env(MINIMAL);
        on.insert("MIGRATE_ON_START".into(), "true".into());
        assert!(Config::from_map(&on).unwrap().migrate_on_start);
    }

    #[test]
    fn invalid_values_are_named() {
        let mut bad = env(MINIMAL);
        bad.insert("TRUSTED_PROXIES".into(), "10.0.0.1, nope".into());
        assert!(Config::from_map(&bad).unwrap_err().0.contains("TRUSTED_PROXIES"));
        let mut bad = env(MINIMAL);
        bad.insert("PACK_MAX_MB".into(), "lots".into());
        assert!(Config::from_map(&bad).unwrap_err().0.contains("PACK_MAX_MB"));
        let mut bad = env(MINIMAL);
        bad.insert("ADMIN_GITHUB_IDS".into(), "12, octocat".into());
        assert!(Config::from_map(&bad).unwrap_err().0.contains("ADMIN_GITHUB_IDS"));
        let mut bad = env(MINIMAL);
        bad.insert("ADMIN_STATS_ROLES".into(), "admin, root".into());
        assert!(Config::from_map(&bad).unwrap_err().0.contains("ADMIN_STATS_ROLES"));
        assert_eq!(Config::from_map(&env(MINIMAL)).unwrap().stats_roles, [Role::Admin, Role::Moderator], "the default");
        let mut good = env(MINIMAL);
        good.insert("ADMIN_GITHUB_IDS".into(), "12, 34".into());
        good.insert("AUTH_RETURN_URLS".into(), "https://a/cb, https://b/cb".into());
        good.insert("ADMIN_STATS_ROLES".into(), "admin".into());
        let config = Config::from_map(&good).unwrap();
        assert_eq!(config.stats_roles, [Role::Admin]);
        assert_eq!(config.admin_github_ids, [12, 34]);
        assert_eq!(config.auth_return_urls, ["https://a/cb", "https://b/cb"]);
    }

    #[test]
    fn self_hosting_doc_lists_every_variable() {
        let example = include_str!("../../../.env.example");
        let doc = include_str!("../../../docs/self-hosting.md");
        for line in example.lines().filter(|l| !l.starts_with('#')) {
            if let Some((name, _)) = line.split_once('=') {
                assert!(doc.contains(&format!("`{name}`")), "{name} is not explained in docs/self-hosting.md");
            }
        }
    }

    #[test]
    fn env_example_lists_every_variable() {
        let example = include_str!("../../../.env.example");
        for name in Config::VARIABLES {
            assert!(example.lines().any(|l| l.starts_with(&format!("{name}="))), "{name} missing in .env.example");
        }
    }
}
