//! Updates SurvivorsForge itself from the releases of `APP_REPO`.
//!
//! Release assets per platform, under the same name in every release:
//! - Windows: `SurvivorsForge-windows-x86_64.zip` holding the `.exe`, which replaces the running
//!   executable.
//! - Linux: `SurvivorsForge-linux-x86_64.AppImage`, which replaces the AppImage file itself (`$APPIMAGE`). A bare binary outside an AppImage is
//!   not updated in place.

use crate::github::{Client, Release};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

pub const RELEASES_URL: &str = concat!("https://github.com/", "Rezondes/SurvivorsForge", "/releases/latest");

pub fn asset_name_for(windows: bool) -> String {
    if windows {
        format!("{}-windows-x86_64.zip", crate::APP_NAME)
    } else {
        format!("{}-linux-x86_64.AppImage", crate::APP_NAME)
    }
}

pub fn asset_name() -> String {
    asset_name_for(cfg!(windows))
}

/// The AppImage file to replace: `$APPIMAGE`, set by the AppImage runtime, if it points at a file.
pub fn appimage_path(appimage_env: Option<std::ffi::OsString>) -> Option<PathBuf> {
    appimage_env.map(PathBuf::from).filter(|p| p.is_file())
}

/// Whether this process can replace itself; otherwise the update banner links to the release.
pub fn can_replace_self() -> bool {
    cfg!(windows) || appimage_path(std::env::var_os("APPIMAGE")).is_some()
}

fn parse(version: &str) -> Option<semver::Version> {
    semver::Version::parse(version.trim().trim_start_matches(['v', 'V'])).ok()
}

pub fn is_newer(current: &str, tag: &str) -> bool {
    matches!((parse(current), parse(tag)), (Some(c), Some(t)) if t > c)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Available {
    pub version: String,
    pub url: String,
}

/// The newer version from `release` with an asset for this platform, if any.
pub fn available(current: &str, release: &Release) -> Option<Available> {
    if !is_newer(current, &release.tag) {
        return None;
    }
    let version = parse(&release.tag)?.to_string();
    let named = |wanted: String| release.assets.iter().find(|a| a.name == wanted);
    let asset = named(asset_name())?;
    Some(Available { version, url: asset.url.clone() })
}

/// Downloads the release zip and swaps the running executable (`swap_files`).
#[cfg(windows)]
pub fn apply(client: &Client, update: &Available, work_dir: &Path) -> Result<()> {
    use std::io::{Read, Write};
    let zip_path = work_dir.join(asset_name());
    client.download(&update.url, &zip_path)?;
    let exe_name = format!("{}.exe", crate::APP_NAME);
    let current = std::env::current_exe()?.canonicalize()?;
    let (new, old) = (sibling(&current, "new"), sibling(&current, "old"));
    let result = (|| -> Result<()> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&zip_path)?).map_err(|e| Error::Zip(e.to_string()))?;
        let mut file = zip.by_name(&exe_name).map_err(|e| Error::Zip(e.to_string()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // Next to the running exe: the same folder, so the renames never copy.
        let mut out = std::fs::File::create(&new)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        drop(out);
        swap_files(&current, &new, &old).map_err(|e| Error::Io(format!("SurvivorsForge could not be replaced, it stays as it was: {e}")))
    })();
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_file(&new);
    result
}

fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    file.with_file_name(name)
}

/// Puts `new` in place of `current`: `current` moves aside to `old` first (Windows lets a running
/// exe be renamed, not overwritten), then `new` takes its name. A virus scanner often holds a freshly
/// written exe for a moment ("used by another process"), so each rename is tried again for some
/// seconds; when `new` cannot take the name, `current` moves back and nothing is lost.
pub fn swap_files(current: &Path, new: &Path, old: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(old);
    retry(|| std::fs::rename(current, old))?;
    if let Err(e) = retry(|| std::fs::rename(new, current)) {
        let _ = retry(|| std::fs::rename(old, current));
        return Err(e);
    }
    Ok(())
}

/// `step` again while it fails because something else holds the file, for about ten seconds.
fn retry(mut step: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match step() {
            // 5: access denied, 32: used by another process, 33: locked (Windows).
            Err(e) if tries < 40 && matches!(e.raw_os_error(), Some(5 | 32 | 33)) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            other => return other,
        }
    }
}

/// Removes what updates left next to `exe`: the old exe set aside, an unfinished new one, and
/// the files of the self-update before 0.5.2 (`.SurvivorsForge.<random>.__relocated__.exe`, ...).
/// The old exe may still run for a moment after a restart, so it is tried a few times.
pub fn clean_up_after_update(exe: &Path) {
    let Some(dir) = exe.parent() else { return };
    let stem = exe.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let leftover = |name: &str| {
        name.starts_with(&format!(".{stem}.")) && ["__relocated__.exe", "__temp__.exe", "__selfdelete__.exe"].iter().any(|s| name.ends_with(s))
    };
    let mut files = vec![sibling(exe, "old"), sibling(exe, "new")];
    if let Ok(entries) = std::fs::read_dir(dir) {
        files.extend(entries.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| leftover(&n.to_string_lossy()))));
    }
    for file in files.into_iter().filter(|f| f != exe && f.exists()) {
        let _ = retry(|| std::fs::remove_file(&file));
    }
}

/// Downloads the new AppImage next to the running one and renames it over it (same folder, so
/// the swap is atomic; the running process keeps its already opened file).
#[cfg(not(windows))]
pub fn apply(client: &Client, update: &Available, _work_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let target = appimage_path(std::env::var_os("APPIMAGE"))
        .ok_or_else(|| Error::Io("SurvivorsForge only updates itself when it runs as an AppImage".into()))?;
    let new = target.with_extension("AppImage.new");
    let result = (|| -> Result<()> {
        client.download(&update.url, &new)?;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&new, &target)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&new);
    result
}

/// Starts the (now replaced) program and ends this process. An AppImage restarts from its file:
/// `current_exe` points into the AppImage's temporary mount, which goes away with this process.
pub fn restart() -> ! {
    let program = appimage_path(std::env::var_os("APPIMAGE")).or_else(|| std::env::current_exe().ok());
    if let Some(program) = program {
        let _ = std::process::Command::new(program).spawn();
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Asset;

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag: tag.into(),
            assets: assets.iter().map(|n| Asset { name: n.to_string(), url: format!("https://dl/{n}") }).collect(),
        }
    }

    #[test]
    fn swap_puts_the_new_file_in_place_and_keeps_the_old_one_aside() {
        let dir = tempfile::tempdir().unwrap();
        let (current, new, old) = (dir.path().join("SurvivorsForge.exe"), dir.path().join("SurvivorsForge.exe.new"), dir.path().join("SurvivorsForge.exe.old"));
        std::fs::write(&current, b"v1").unwrap();
        std::fs::write(&new, b"v2").unwrap();
        std::fs::write(&old, b"v0 from the update before").unwrap();
        swap_files(&current, &new, &old).unwrap();
        assert_eq!((std::fs::read(&current).unwrap(), std::fs::read(&old).unwrap()), (b"v2".to_vec(), b"v1".to_vec()));
        assert!(!new.exists());
    }

    #[test]
    fn a_failed_swap_brings_the_old_file_back() {
        let dir = tempfile::tempdir().unwrap();
        let (current, old) = (dir.path().join("SurvivorsForge.exe"), dir.path().join("SurvivorsForge.exe.old"));
        std::fs::write(&current, b"v1").unwrap();
        assert!(swap_files(&current, &dir.path().join("missing.new"), &old).is_err());
        assert_eq!(std::fs::read(&current).unwrap(), b"v1", "the launcher is never lost");
        assert!(!old.exists());
    }

    /// A virus scanner holds a freshly written exe for a moment; the swap waits for it.
    #[cfg(windows)]
    #[test]
    fn a_swap_waits_while_the_new_file_is_held() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (current, new, old) = (dir.path().join("SurvivorsForge.exe"), dir.path().join("SurvivorsForge.exe.new"), dir.path().join("SurvivorsForge.exe.old"));
        std::fs::write(&current, b"v1").unwrap();
        std::fs::write(&new, b"v2").unwrap();
        // No sharing at all: renaming it fails with "used by another process" (os error 32).
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&new).unwrap();
        assert_eq!(std::fs::rename(&new, dir.path().join("x")).unwrap_err().raw_os_error(), Some(32));
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(700));
            drop(held);
        });
        swap_files(&current, &new, &old).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), b"v2");
    }

    #[test]
    fn cleanup_removes_what_updates_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("SurvivorsForge.exe");
        for name in ["SurvivorsForge.exe", "SurvivorsForge.exe.old", "SurvivorsForge.exe.new",
                     ".SurvivorsForge.abcdefgh.__relocated__.exe", ".SurvivorsForge.abcdefgh.__temp__.exe", ".Other.abc.__temp__.exe", "notes.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        clean_up_after_update(&exe);
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, [".Other.abc.__temp__.exe", "SurvivorsForge.exe", "notes.txt"]);
    }

    #[test]
    fn newer_version_shows_banner() {
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("0.1.0", "v0.0.9"));
        assert!(!is_newer("0.1.0", "nightly"));
        // A tester on a release candidate gets the final version; nobody goes back to a candidate.
        assert!(is_newer("0.4.0-rc.1", "v0.4.0"));
        assert!(is_newer("0.4.0-rc.1", "v0.4.0-rc.2"));
        assert!(!is_newer("0.4.0", "v0.4.0-rc.2"));
    }

    #[test]
    fn asset_name_per_platform() {
        assert_eq!(asset_name_for(true), "SurvivorsForge-windows-x86_64.zip");
        assert_eq!(asset_name_for(false), "SurvivorsForge-linux-x86_64.AppImage");
    }

    #[test]
    fn appimage_target_from_env() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SurvivorsForge.AppImage");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(appimage_path(Some(file.clone().into())), Some(file));
        assert_eq!(appimage_path(Some(dir.path().join("missing").into())), None);
        assert_eq!(appimage_path(None), None);
    }

    #[test]
    fn available_needs_matching_asset() {
        let name = asset_name();
        assert_eq!(
            available("0.1.0", &release("v0.2.0", &[&name])),
            Some(Available { version: "0.2.0".into(), url: format!("https://dl/{name}") })
        );
        assert_eq!(available("0.1.0", &release("v0.2.0", &["other.zip"])), None);
        assert_eq!(available("0.2.0", &release("v0.2.0", &[&name])), None);
    }
}
