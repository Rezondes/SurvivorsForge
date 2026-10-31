use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

const FILE: &str = "config.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub addons_dir: PathBuf,
    /// The game version the user plays, for compatibility warnings; chosen in the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_version: Option<String>,
    /// Another platform than the built-in one (a self-hosted copy, a local test); set in the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// Count installs and updates in the platform's public statistics (`crate::telemetry`).
    #[serde(default = "yes")]
    pub telemetry: bool,
    /// Send the random `install_id` along; only matters while `telemetry` is on.
    #[serde(default = "yes")]
    pub send_install_id: bool,
    /// Random, made on the first start; it says nothing about the user or the computer.
    #[serde(default = "new_install_id")]
    pub install_id: Option<String>,
    /// The lists shown as tiles instead of rows (`installed`, `browse`), each chosen on its page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tile_pages: Vec<String>,
}

fn yes() -> bool {
    true
}

fn new_install_id() -> Option<String> {
    Some(crate::telemetry::new_install_id())
}

impl Default for Config {
    fn default() -> Config {
        Config {
            addons_dir: PathBuf::new(),
            game_version: None,
            api_url: None,
            web_url: None,
            telemetry: true,
            send_install_id: true,
            install_id: new_install_id(),
            tile_pages: Vec::new(),
        }
    }
}

/// A platform address typed in the settings: empty for the default, else an http(s) URL without
/// trailing slash.
pub fn platform_url(input: &str) -> Result<Option<String>, String> {
    let url = input.trim().trim_end_matches('/');
    if url.is_empty() {
        return Ok(None);
    }
    let host = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    match host {
        Some(host) if !host.is_empty() && !url.contains(char::is_whitespace) => Ok(Some(url.to_string())),
        _ => Err(format!("\"{url}\" is not a web address (http:// or https://)")),
    }
}

/// `%APPDATA%\SurvivorsForge` on Windows, `~/.config/SurvivorsForge` on Linux.
/// `SURVIVORSFORGE_DATA_DIR` overrides it, for testing against a copy of an AddOns folder.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SURVIVORSFORGE_DATA_DIR") {
        return dir.into();
    }
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join(crate::APP_NAME))
        .unwrap_or_else(|| PathBuf::from(crate::APP_NAME))
}

impl Config {
    pub fn tiles(&self, page: &str) -> bool {
        self.tile_pages.iter().any(|p| p == page)
    }

    pub fn set_tiles(&mut self, page: &str, on: bool) {
        self.tile_pages.retain(|p| p != page);
        if on {
            self.tile_pages.push(page.to_string());
        }
    }

    /// The saved settings. A file from before `install_id` gets one now and keeps it.
    pub fn load(dir: &Path) -> Option<Config> {
        let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
        let config: Config = serde_json::from_str(&text).ok()?;
        let raw: serde_json::Value = serde_json::from_str(&text).ok()?;
        if raw.get("install_id").is_none() {
            let _ = config.save(dir);
        }
        Some(config)
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).expect("config serializes");
        crate::write_atomic(&dir.join(FILE), &json)
    }
}

#[derive(Debug, PartialEq)]
pub enum DirError {
    NotFound(PathBuf),
    NotADirectory,
    NotWritable,
}

impl fmt::Display for DirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DirError::NotFound(path) => {
                write!(f, "No folder at \"{}\". Check the spelling", path.display())?;
                f.write_str(if cfg!(windows) { "." } else { "; paths on Linux are case-sensitive." })
            }
            DirError::NotADirectory => f.write_str("The path is not a folder."),
            DirError::NotWritable => f.write_str("The folder is not writable."),
        }
    }
}

/// A typed path with a leading `~` resolved to the home folder, as a shell would.
pub fn expand_home(input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    if let (Some(home), Some(rest)) = (home, input.strip_prefix('~')) {
        if rest.is_empty() {
            return home.to_path_buf();
        }
        if let Some(rest) = rest.strip_prefix(['/', '\\']) {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Button label for a found AddOns folder: the game folder above `benilla-config/AddOns`,
/// with the home folder shortened to `~` on Linux.
pub fn suggestion_label(addons: &Path, home: Option<&Path>) -> String {
    let game = addons.parent().and_then(Path::parent).unwrap_or(addons);
    match home.filter(|_| !cfg!(windows)).and_then(|h| game.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => game.display().to_string(),
    }
}

#[derive(Debug, PartialEq)]
pub enum DirWarning {
    NotNamedAddOns,
}

impl fmt::Display for DirWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The folder is not named \"AddOns\". Is this really the game's AddOns folder?")
    }
}

/// Where the game is usually unpacked: the home folder and its Games, Downloads and Desktop
/// folders, removable media on Linux (Steam Deck SD card), every drive root on Windows.
pub fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(user) = directories::UserDirs::new() {
        roots.push(user.home_dir().join("Games"));
        roots.extend(user.download_dir().map(Path::to_path_buf));
        roots.extend(user.desktop_dir().map(Path::to_path_buf));
        roots.push(user.home_dir().to_path_buf());
    }
    if cfg!(windows) {
        roots.extend((b'C'..=b'Z').map(|d| PathBuf::from(format!("{}:\\", d as char))).filter(|p| p.is_dir()));
    } else {
        // `/run/media/<user>/<label>` on most distros, `/run/media/<device>` on SteamOS
        roots.push(PathBuf::from("/run/media"));
    }
    roots.into_iter().filter(|p| p.is_dir()).collect()
}

/// Folders that look like the game's AddOns folder (`<game>/benilla-config/AddOns`), found by a
/// short walk below each root. Hidden folders and symlinks are skipped, and each root has a budget
/// of directory entries, so a huge drive can't stall the setup.
pub fn find_addons_dirs(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    const BUDGET_PER_ROOT: usize = 20_000;
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        let mut budget = BUDGET_PER_ROOT;
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let candidate = dir.join("benilla-config").join("AddOns");
            if candidate.is_dir() && !found.contains(&candidate) {
                found.push(candidate);
            }
            if depth >= max_depth {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let hidden = entry.file_name().to_string_lossy().starts_with(['.', '$']);
                if !hidden && entry.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push((entry.path(), depth + 1));
                }
            }
        }
    }
    found.sort();
    found
}

/// The game's executable next to `<game>/benilla-config/AddOns`: `AzerothSurvivors.exe` on Windows,
/// on Linux a native build whose name starts with `AzerothSurvivors`. A Windows build on Linux runs
/// through Proton and can't be started from here.
pub fn game_executable(addons_dir: &Path, windows: bool) -> Option<PathBuf> {
    let game = addons_dir.parent()?.parent()?;
    if windows {
        return Some(game.join("AzerothSurvivors.exe")).filter(|p| p.is_file());
    }
    // ponytail: first match by name; pin the exact name once the Linux build's file is known
    let mut native: Vec<PathBuf> = std::fs::read_dir(game)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("AzerothSurvivors")))
        .filter(|p| !p.extension().is_some_and(|e| ["exe", "dll", "pdb"].iter().any(|x| e.eq_ignore_ascii_case(x))))
        .collect();
    native.sort();
    native.into_iter().next()
}

/// Steam Deck Game Mode (and any gamescope session): the app runs fullscreen there.
pub fn is_game_mode(xdg_current_desktop: Option<&str>, gamescope_display: bool) -> bool {
    gamescope_display || xdg_current_desktop.is_some_and(|d| d.eq_ignore_ascii_case("gamescope"))
}

pub fn validate_addons_dir(path: &Path) -> Result<Option<DirWarning>, DirError> {
    if !path.exists() {
        return Err(DirError::NotFound(path.to_path_buf()));
    }
    if !path.is_dir() {
        return Err(DirError::NotADirectory);
    }
    let probe = path.join(format!(".survivorsforge-probe-{}", crate::unique_name()));
    std::fs::write(&probe, b"").map_err(|_| DirError::NotWritable)?;
    let _ = std::fs::remove_file(&probe);
    let named_addons = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("AddOns"));
    Ok((!named_addons).then_some(DirWarning::NotNamedAddOns))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_game_executable() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("AzerothSurvivors");
        let addons = game.join("benilla-config").join("AddOns");
        std::fs::create_dir_all(&addons).unwrap();
        assert_eq!(game_executable(&addons, true), None, "nothing there yet");
        std::fs::write(game.join("AzerothSurvivors.exe"), b"").unwrap();
        assert_eq!(game_executable(&addons, true), Some(game.join("AzerothSurvivors.exe")));
        assert_eq!(game_executable(&addons, false), None, "Linux can't start the Windows build directly");
        std::fs::write(game.join("AzerothSurvivors.x86_64"), b"").unwrap();
        assert_eq!(game_executable(&addons, false), Some(game.join("AzerothSurvivors.x86_64")));
        assert_eq!(game_executable(Path::new(""), true), None, "no AddOns folder set");
    }

    #[test]
    fn suggests_existing_addon_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let game = root.join("Games/AzerothSurvivors/benilla-config/AddOns");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(root.join(".hidden/AzerothSurvivors/benilla-config/AddOns")).unwrap();
        std::fs::create_dir_all(root.join("a/b/c/d/benilla-config/AddOns")).unwrap();
        assert_eq!(find_addons_dirs(&[root.to_path_buf()], 3), std::slice::from_ref(&game));
        assert_eq!(find_addons_dirs(&[root.to_path_buf(), root.join("Games")], 3), [game], "no duplicates");
    }

    #[test]
    fn detects_game_mode_from_env() {
        assert!(is_game_mode(Some("gamescope"), false));
        assert!(is_game_mode(None, true));
        assert!(!is_game_mode(Some("KDE"), false));
        assert!(!is_game_mode(None, false));
    }

    #[test]
    fn each_list_keeps_its_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        assert!(!config.tiles("browse"), "lists until the user picks tiles");
        config.set_tiles("browse", true);
        config.save(dir.path()).unwrap();
        let loaded = Config::load(dir.path()).unwrap();
        assert!(loaded.tiles("browse") && !loaded.tiles("installed"), "one page's choice leaves the other alone");
        let mut back = loaded.clone();
        back.set_tiles("browse", false);
        assert!(!back.tiles("browse"));
        std::fs::write(dir.path().join(FILE), r#"{"addons_dir": "x"}"#).unwrap();
        assert!(!Config::load(dir.path()).unwrap().tiles("installed"), "a config from before still loads");
    }

    #[test]
    fn load_returns_none_without_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()), None);
    }

    #[test]
    fn validate_rejects_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert_eq!(validate_addons_dir(&missing), Err(DirError::NotFound(missing.clone())));
    }

    #[test]
    fn expands_leading_tilde_only() {
        let home = Path::new("/home/deck");
        assert_eq!(expand_home(" ~ ", Some(home)), home);
        assert_eq!(expand_home("~/Games/AddOns", Some(home)), home.join("Games/AddOns"));
        assert_eq!(expand_home("~deck/AddOns", Some(home)), PathBuf::from("~deck/AddOns"));
        assert_eq!(expand_home("/opt/~/AddOns", Some(home)), PathBuf::from("/opt/~/AddOns"));
        assert_eq!(expand_home("~/AddOns", None), PathBuf::from("~/AddOns"));
    }

    #[test]
    fn labels_suggestion_by_game_folder() {
        let home = Path::new("/home/deck");
        let addons = home.join("Games/AzerothSurvivors/benilla-config/AddOns");
        let expected = if cfg!(windows) { home.join("Games/AzerothSurvivors").display().to_string() } else { "~/Games/AzerothSurvivors".into() };
        assert_eq!(suggestion_label(&addons, Some(home)), expected);
        let card = Path::new("/run/media/mmcblk0p1/AS/benilla-config/AddOns");
        assert_eq!(suggestion_label(card, Some(home)), Path::new("/run/media/mmcblk0p1/AS").display().to_string());
    }

    #[test]
    fn validate_accepts_addons_and_warns_on_unusual_name() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("AddOns");
        let foo = dir.path().join("foo");
        std::fs::create_dir(&addons).unwrap();
        std::fs::create_dir(&foo).unwrap();
        assert_eq!(validate_addons_dir(&addons), Ok(None));
        assert_eq!(validate_addons_dir(&foo), Ok(Some(DirWarning::NotNamedAddOns)));
        assert_eq!(std::fs::read_dir(&addons).unwrap().count(), 0, "probe file is removed");
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { addons_dir: PathBuf::from(r"P:\AddOns"), ..Default::default() };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()), Some(config));
    }

    #[test]
    fn install_id_created_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        let first = Config::load(dir.path()).unwrap();
        assert!(first.install_id.is_some());
        assert_eq!(Config::load(dir.path()).unwrap().install_id, first.install_id, "saved on the first load");
        assert_ne!(Config::default().install_id, first.install_id, "every installation its own");
    }

    #[test]
    fn old_config_loads_with_telemetry_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert!(config.telemetry && config.send_install_id);
        let off = Config { telemetry: false, send_install_id: false, ..config };
        off.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()), Some(off), "the choice is kept");
    }

    #[test]
    fn game_version_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { addons_dir: dir.path().join("AddOns"), game_version: Some("0.69.1".into()), ..Default::default() };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().game_version.as_deref(), Some("0.69.1"));
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().game_version, None, "old config files still load");
    }

    #[test]
    fn platform_addresses_are_checked_and_kept() {
        assert_eq!(platform_url(""), Ok(None), "empty means the default");
        assert_eq!(platform_url(" https://api.example.test/ "), Ok(Some("https://api.example.test".into())));
        assert_eq!(platform_url("http://localhost:3081"), Ok(Some("http://localhost:3081".into())));
        for bad in ["api.example.test", "ftp://x", "https://a b", "https://"] {
            assert!(platform_url(bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let config = Config { addons_dir: dir.path().join("AddOns"), api_url: Some("http://localhost:3081".into()), ..Default::default() };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().api_url.as_deref(), Some("http://localhost:3081"));
    }
}
