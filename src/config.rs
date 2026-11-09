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
    /// Install checked updates of every addon on their own (`controller::auto_update_ids`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_update_all: bool,
    /// Entry ids that update on their own while `auto_update_all` is off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_update: Vec<String>,
    /// Entry ids the user put back to an older version: they do not update on their own until the
    /// newest is installed again by hand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
    /// Where the window was when the launcher closed; not used in Game Mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowPlace>,
    /// The page open when the launcher closed: 0 installed, 1 browse, 2 settings.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_page: u8,
    /// What the launcher does once the game it started runs; chosen in the settings.
    #[serde(default, skip_serializing_if = "AfterLaunch::is_nothing")]
    pub after_launch: AfterLaunch,
    /// Look for updates on its own while the launcher is open, every `check_minutes`.
    #[serde(default = "yes")]
    pub periodic_check: bool,
    /// 15 to 60 in steps of 5 (`snap_check_minutes`).
    #[serde(default = "default_check_minutes")]
    pub check_minutes: u8,
    /// How many backups of the addon settings stay (`crate::backup`).
    #[serde(default = "default_backups_kept")]
    pub backups_kept: u8,
    /// Disabled addons get their updates too, where they wait; off, an update waits until the
    /// addon is enabled again.
    #[serde(default = "yes")]
    pub update_disabled: bool,
    /// The platform addons known when the launcher last closed; the others are new in Browse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_addons: Vec<String>,
}

fn default_backups_kept() -> u8 {
    10
}

fn default_check_minutes() -> u8 {
    30
}

/// The choices for the time between update checks, in minutes.
pub const CHECK_MINUTES: [u8; 10] = [15, 20, 25, 30, 35, 40, 45, 50, 55, 60];

/// `minutes` as one of `CHECK_MINUTES`: the nearest, a tie to the shorter one.
pub fn snap_check_minutes(minutes: u8) -> u8 {
    let m = minutes.clamp(15, 60);
    let down = m - (m - 15) % 5;
    if m - down > 2 { down + 5 } else { down }
}

/// What the launcher does after it started the game.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AfterLaunch {
    #[default]
    Nothing,
    Minimize,
    Close,
}

impl AfterLaunch {
    /// The choices in the order of the settings dropdown.
    pub const LABELS: [&str; 3] = ["Stay open", "Minimize", "Close"];

    fn is_nothing(&self) -> bool {
        *self == AfterLaunch::Nothing
    }

    pub fn index(self) -> i32 {
        self as i32
    }

    pub fn from_index(i: i32) -> AfterLaunch {
        match i {
            1 => AfterLaunch::Minimize,
            2 => AfterLaunch::Close,
            _ => AfterLaunch::Nothing,
        }
    }
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// The window in physical pixels, as the system places it. Maximized keeps the size from before.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// A screen's area in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowPlace {
    /// Whether enough of the window's top shows on one of `screens` to grab it there: a screen
    /// unplugged since, and the window would open where no one sees it.
    pub fn on_a_screen(&self, screens: &[Screen]) -> bool {
        const GRIP: i64 = 64;
        let (left, top, right) = (self.x as i64, self.y as i64, self.x as i64 + self.width as i64);
        screens.iter().any(|s| {
            let (sl, st) = (s.x as i64, s.y as i64);
            let (sr, sb) = (sl + s.width as i64, st + s.height as i64);
            let wide = right.min(sr) - left.max(sl);
            wide >= GRIP && top >= st && top + GRIP <= sb
        })
    }
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
            auto_update_all: false,
            auto_update: Vec::new(),
            held: Vec::new(),
            window: None,
            last_page: 0,
            after_launch: AfterLaunch::Nothing,
            periodic_check: true,
            check_minutes: default_check_minutes(),
            backups_kept: default_backups_kept(),
            update_disabled: true,
            seen_addons: Vec::new(),
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

/// What the settings page holds, typed or ticked.
pub struct SettingsInput {
    pub addons_dir: PathBuf,
    pub game_version: Option<String>,
    pub api_url: String,
    pub web_url: String,
    pub telemetry: bool,
    pub send_install_id: bool,
    pub auto_update_all: bool,
    pub after_launch: AfterLaunch,
    pub periodic_check: bool,
    pub check_minutes: u8,
    pub backups_kept: u8,
    pub update_disabled: bool,
}

/// The settings with every valid field of `input` taken over; an invalid field keeps the saved
/// value and gives an error naming it, so one typo never blocks the other settings. Also the
/// warning about an odd AddOns folder.
pub fn apply_settings(saved: &Config, input: SettingsInput) -> (Config, SettingsErrors, Option<DirWarning>) {
    let mut errors = SettingsErrors::default();
    let mut config = Config {
        game_version: input.game_version,
        telemetry: input.telemetry,
        send_install_id: input.send_install_id,
        auto_update_all: input.auto_update_all,
        after_launch: input.after_launch,
        periodic_check: input.periodic_check,
        check_minutes: snap_check_minutes(input.check_minutes),
        backups_kept: input.backups_kept.clamp(1, 50),
        update_disabled: input.update_disabled,
        ..saved.clone()
    };
    let mut warning = None;
    match validate_addons_dir(&input.addons_dir) {
        Ok(w) => {
            warning = w;
            config.addons_dir = input.addons_dir;
        }
        Err(e) => errors.path = Some(e.to_string()),
    }
    match platform_url(&input.api_url) {
        Ok(url) => config.api_url = url,
        Err(e) => errors.api = Some(e),
    }
    match platform_url(&input.web_url) {
        Ok(url) => config.web_url = url,
        Err(e) => errors.web = Some(e),
    }
    (config, errors, warning)
}

/// Why a settings field was not taken over, per field; shown under the field and in a toast.
#[derive(Debug, Default, PartialEq)]
pub struct SettingsErrors {
    pub path: Option<String>,
    pub api: Option<String>,
    pub web: Option<String>,
}

impl SettingsErrors {
    pub fn is_empty(&self) -> bool {
        self.path.is_none() && self.api.is_none() && self.web.is_none()
    }

    /// Each error with the field it is about, for the toast.
    pub fn messages(&self) -> Vec<String> {
        [("AddOns folder", &self.path), ("Platform API", &self.api), ("Platform website", &self.web)]
            .into_iter()
            .filter_map(|(field, e)| e.as_ref().map(|e| format!("{field}: {e}")))
            .collect()
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

    /// What to do once the game runs. Game Mode stays: the Steam overlay is the way back there.
    pub fn after_launch(&self, game_mode: bool) -> AfterLaunch {
        if game_mode { AfterLaunch::Nothing } else { self.after_launch }
    }

    /// Whether the installed addon `id` updates on its own.
    pub fn auto_updates(&self, id: &str) -> bool {
        !self.held.iter().any(|h| h == id) && (self.auto_update_all || self.auto_update.iter().any(|a| a == id))
    }

    /// Holds `id` at the older version just installed, or (`false`) lets it update again.
    pub fn hold(&mut self, id: &str, held: bool) {
        self.held.retain(|h| h != id);
        if held {
            self.held.push(id.to_string());
        }
    }

    /// The saved settings. A file from before `install_id` gets one now and keeps it.
    pub fn load(dir: &Path) -> Option<Config> {
        let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
        let mut config: Config = serde_json::from_str(&text).ok()?;
        config.check_minutes = snap_check_minutes(config.check_minutes);
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
    /// An AddOns folder, but not in the game's `benilla-config`: maybe another game's.
    NoGame,
}

impl fmt::Display for DirWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DirWarning::NotNamedAddOns => "The folder is not named \"AddOns\". Is this really the game's AddOns folder?",
            DirWarning::NoGame => "No Azeroth Survivors game found next to this folder. The AddOns folder is usually <game>/benilla-config/AddOns.",
        })
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

/// The game's executable next to `<game>/benilla-config/AddOns`: `AzerothSurvivors.exe` on
/// Windows and macOS, or a native build whose name starts with `AzerothSurvivors` on Linux.
pub fn game_executable(addons_dir: &Path, windows_build: bool) -> Option<PathBuf> {
    let game = addons_dir.parent()?.parent()?;
    if windows_build {
        return Some(game.join(crate::game_update::GAME_EXE)).filter(|p| p.is_file());
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
    if !named_addons {
        return Ok(Some(DirWarning::NotNamedAddOns));
    }
    let in_game = path.parent().and_then(Path::file_name).is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("benilla-config"));
    Ok((!in_game).then_some(DirWarning::NoGame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_addons_folder_outside_the_game_is_questioned() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("Interface").join("AddOns");
        let game = dir.path().join("benilla-config").join("AddOns");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&game).unwrap();
        assert!(matches!(validate_addons_dir(&elsewhere), Ok(Some(DirWarning::NoGame))));
        assert!(matches!(validate_addons_dir(&game), Ok(None)));
    }

    #[test]
    fn one_bad_setting_does_not_block_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("AddOns");
        std::fs::create_dir(&addons).unwrap();
        let saved = Config { addons_dir: addons.clone(), ..Default::default() };
        let input = SettingsInput {
            addons_dir: dir.path().join("Typo"),
            game_version: None,
            api_url: "not a url".into(),
            web_url: "https://forge.example".into(),
            telemetry: false,
            send_install_id: true,
            auto_update_all: false,
            after_launch: AfterLaunch::Minimize,
            periodic_check: true,
            check_minutes: 45,
            backups_kept: 0,
            update_disabled: false,
        };
        let (config, errors, _) = apply_settings(&saved, input);
        assert_eq!(config.after_launch, AfterLaunch::Minimize);
        assert_eq!(config.check_minutes, 45);
        assert_eq!(config.backups_kept, 1, "at least one backup stays");
        assert_eq!(Config::default().backups_kept, 10);
        assert!(!config.update_disabled);
        assert!(Config::default().update_disabled, "disabled addons update unless switched off");
        assert!(!config.telemetry, "the privacy toggle is taken over");
        assert_eq!(config.web_url.as_deref(), Some("https://forge.example"));
        assert_eq!(config.addons_dir, addons, "the bad path keeps the saved one");
        assert_eq!(config.api_url, None);
        assert!(errors.path.is_some() && errors.api.is_some() && errors.web.is_none(), "{errors:?}");
        let messages = errors.messages();
        assert!(messages[0].starts_with("AddOns folder: ") && messages[1].starts_with("Platform API: "), "{messages:?}");
    }

    #[test]
    fn an_addon_held_at_an_older_version_does_not_update_on_its_own() {
        let mut config = Config { auto_update_all: true, ..Config::default() };
        assert!(config.auto_updates("lucky"));
        config.hold("lucky", true);
        assert!(!config.auto_updates("lucky"), "the user went back to an older version");
        assert!(config.auto_updates("other"));
        config.hold("lucky", true);
        assert_eq!(config.held, ["lucky"], "held once");
        config.hold("lucky", false);
        assert!(config.auto_updates("lucky"), "installing the newest by hand lets it update again");
    }

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
        let addons = dir.path().join("benilla-config").join("AddOns");
        let foo = dir.path().join("foo");
        std::fs::create_dir_all(&addons).unwrap();
        std::fs::create_dir(&foo).unwrap();
        assert_eq!(validate_addons_dir(&addons), Ok(None));
        assert_eq!(validate_addons_dir(&foo), Ok(Some(DirWarning::NotNamedAddOns)));
        assert_eq!(std::fs::read_dir(&addons).unwrap().count(), 0, "probe file is removed");
    }

    #[test]
    fn window_place_and_page_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let place = WindowPlace { x: -1200, y: 40, width: 1300, height: 900, maximized: true };
        let config = Config { addons_dir: PathBuf::from(r"P:\AddOns"), window: Some(place), last_page: 1, ..Default::default() };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()), Some(config));
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        let old = Config::load(dir.path()).unwrap();
        assert_eq!((old.window, old.last_page), (None, 0), "old config files still load");
    }

    #[test]
    fn after_launch_does_nothing_unless_chosen() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        let old = Config::load(dir.path()).unwrap();
        assert_eq!(old.after_launch, AfterLaunch::Nothing, "old config files still load");
        assert_eq!(old.after_launch(false), AfterLaunch::Nothing);
        let close = Config { after_launch: AfterLaunch::Close, ..old };
        assert_eq!(close.after_launch(false), AfterLaunch::Close);
        assert_eq!(close.after_launch(true), AfterLaunch::Nothing, "Game Mode has its own way back");
        close.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().after_launch, AfterLaunch::Close);
        assert_eq!(AfterLaunch::from_index(1), AfterLaunch::Minimize);
        assert_eq!(AfterLaunch::from_index(9), AfterLaunch::Nothing);
        assert_eq!(AfterLaunch::Close.index(), 2);
    }

    #[test]
    fn the_update_check_runs_every_30_minutes_unless_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x"}"#).unwrap();
        let old = Config::load(dir.path()).unwrap();
        assert!(old.periodic_check, "on for old config files");
        assert_eq!(old.check_minutes, 30);
        std::fs::write(dir.path().join("config.json"), r#"{"addons_dir":"C:/x","periodic_check":false,"check_minutes":33}"#).unwrap();
        let typed = Config::load(dir.path()).unwrap();
        assert!(!typed.periodic_check);
        assert_eq!(typed.check_minutes, 35, "to the nearest step");
        assert_eq!([7, 15, 17, 18, 60, 90].map(snap_check_minutes), [15, 15, 15, 20, 60, 60]);
    }

    #[test]
    fn a_window_shows_only_where_a_screen_is() {
        let screens = [Screen { x: 0, y: 0, width: 1920, height: 1080 }, Screen { x: 1920, y: 0, width: 1920, height: 1080 }];
        let at = |x, y| WindowPlace { x, y, width: 1180, height: 800, maximized: false };
        assert!(at(100, 100).on_a_screen(&screens));
        assert!(at(3000, 200).on_a_screen(&screens), "the second screen");
        assert!(!at(-2500, 100).on_a_screen(&screens), "a screen that is gone");
        assert!(!at(100, 1070).on_a_screen(&screens), "only its last pixels show");
        assert!(!at(100, 100).on_a_screen(&[]));
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
