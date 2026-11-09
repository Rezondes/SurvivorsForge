//! Backups of the game's addon settings: `saved/` (one file per addon) and `saved-variables.lua`
//! (the UI's own) in the game's config folder, the parent of the AddOns folder. Made before an
//! update, a removal or a profile changes addons, kept in `backups/` there, the newest
//! `Config::backups_kept`.

use crate::{Error, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const SAVED_DIR: &str = "saved";
const SAVED_FILE: &str = "saved-variables.lua";
const BACKUPS: &str = "backups";
const PREFIX: &str = "saved-";

/// The choices for how many backups stay.
pub const KEPT_CHOICES: [u8; 5] = [3, 5, 10, 20, 50];

/// One backup as the settings list it.
#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    pub path: PathBuf,
    /// "2026-11-09 14:03:12", from the file name.
    pub when: String,
    pub bytes: u64,
}

/// The game's config folder (`benilla-config`), where `saved/` lives: the AddOns folder's parent.
pub fn game_config_dir(addons_dir: &Path) -> Option<&Path> {
    addons_dir.parent().filter(|p| !p.as_os_str().is_empty())
}

/// Zips `saved/` and `saved-variables.lua` of `config_dir` into `backups/saved-<stamp>.zip`
/// (`stamp` as `YYYYMMDD-HHMMSS`). None when there is nothing to back up yet.
pub fn create(config_dir: &Path, stamp: &str) -> Result<Option<PathBuf>> {
    let saved = config_dir.join(SAVED_DIR);
    let file = config_dir.join(SAVED_FILE);
    if !saved.is_dir() && !file.is_file() {
        return Ok(None);
    }
    let dir = config_dir.join(BACKUPS);
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{PREFIX}{stamp}.zip"));
    let part = dir.join(format!("{PREFIX}{stamp}.zip.part"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&part)?);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zip_err = |e: zip::result::ZipError| Error::Zip(e.to_string());
    if file.is_file() {
        zip.start_file(SAVED_FILE, options).map_err(zip_err)?;
        zip.write_all(&std::fs::read(&file)?)?;
    }
    if saved.is_dir() {
        let mut files = Vec::new();
        collect(&saved, &mut files)?;
        for path in files {
            let name = path.strip_prefix(config_dir).map_err(|e| Error::Io(e.to_string()))?;
            let name = name.to_string_lossy().replace('\\', "/");
            zip.start_file(name, options).map_err(zip_err)?;
            zip.write_all(&std::fs::read(&path)?)?;
        }
    }
    zip.finish().map_err(zip_err)?;
    std::fs::rename(&part, &target)?;
    Ok(Some(target))
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// The backups in `config_dir`, newest first.
pub fn list(config_dir: &Path) -> Vec<Backup> {
    let Ok(entries) = std::fs::read_dir(config_dir.join(BACKUPS)) else { return Vec::new() };
    let mut backups: Vec<Backup> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stamp = name.strip_prefix(PREFIX)?.strip_suffix(".zip")?.to_string();
            let when = describe(&stamp)?;
            Some(Backup { path: e.path(), when, bytes: e.metadata().map(|m| m.len()).unwrap_or(0) })
        })
        .collect();
    backups.sort_by(|a, b| b.when.cmp(&a.when));
    backups
}

/// `20261109-140312` as `2026-11-09 14:03:12`; None for anything else.
fn describe(stamp: &str) -> Option<String> {
    let b = stamp.as_bytes();
    let ok = b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit());
    ok.then(|| format!("{}-{}-{} {}:{}:{}", &stamp[0..4], &stamp[4..6], &stamp[6..8], &stamp[9..11], &stamp[11..13], &stamp[13..15]))
}

/// Deletes all but the newest `keep` backups.
pub fn prune(config_dir: &Path, keep: usize) -> Result<()> {
    for old in list(config_dir).into_iter().skip(keep.max(1)) {
        std::fs::remove_file(old.path)?;
    }
    Ok(())
}

/// Puts the backup `zip` back: the current settings are backed up first (`stamp`), then
/// `saved/` and `saved-variables.lua` are replaced by the backup's. A zip with a path outside
/// those is refused before anything changes.
pub fn restore(config_dir: &Path, zip: &Path, stamp: &str) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(|e| Error::Zip(e.to_string()))?;
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| Error::Zip(e.to_string()))?;
        let raw = entry.name().to_string();
        let Some(path) = entry.enclosed_name() else { return Err(Error::UnsafePath(raw)) };
        let inside = path == Path::new(SAVED_FILE) || path.starts_with(SAVED_DIR);
        if !inside {
            return Err(Error::UnsafePath(raw));
        }
        if entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        files.push((path, bytes));
    }
    create(config_dir, stamp)?;
    let saved = config_dir.join(SAVED_DIR);
    if saved.is_dir() {
        std::fs::remove_dir_all(&saved)?;
    }
    let _ = std::fs::remove_file(config_dir.join(SAVED_FILE));
    for (path, bytes) in files {
        let target = config_dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::write_atomic(&target, &bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("benilla-config");
        std::fs::create_dir_all(config.join("saved")).unwrap();
        std::fs::create_dir_all(config.join("Addons")).unwrap();
        std::fs::write(config.join("saved").join("Lucky.lua"), "LuckyDB = { odds = 7 }").unwrap();
        std::fs::write(config.join(SAVED_FILE), "SIMPLE_CHAT = \"0\"").unwrap();
        (dir, config)
    }

    fn names(zip: &Path) -> Vec<String> {
        let archive = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        names
    }

    #[test]
    fn a_backup_holds_saved_and_the_variables_file() {
        let (_dir, config) = game();
        assert_eq!(game_config_dir(&config.join("Addons")), Some(config.as_path()));
        let zip = create(&config, "20261109-140312").unwrap().expect("a backup");
        assert_eq!(zip, config.join("backups").join("saved-20261109-140312.zip"));
        assert_eq!(names(&zip), ["saved-variables.lua", "saved/Lucky.lua"]);
        assert_eq!(list(&config).iter().map(|b| b.when.as_str()).collect::<Vec<_>>(), ["2026-11-09 14:03:12"]);
        assert!(list(&config)[0].bytes > 0);
    }

    #[test]
    fn without_saved_data_there_is_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(create(dir.path(), "20261109-140312").unwrap(), None);
        assert!(!dir.path().join("backups").exists());
    }

    #[test]
    fn only_the_newest_stay() {
        let (_dir, config) = game();
        for stamp in ["20261101-100000", "20261103-100000", "20261102-100000"] {
            create(&config, stamp).unwrap();
        }
        std::fs::write(config.join("backups").join("notes.txt"), "mine").unwrap();
        prune(&config, 2).unwrap();
        assert_eq!(list(&config).iter().map(|b| b.when.as_str()).collect::<Vec<_>>(), ["2026-11-03 10:00:00", "2026-11-02 10:00:00"]);
        assert!(config.join("backups").join("notes.txt").exists(), "other files stay");
    }

    #[test]
    fn restore_puts_the_settings_back_and_keeps_the_current_ones() {
        let (_dir, config) = game();
        let zip = create(&config, "20261101-100000").unwrap().unwrap();
        std::fs::write(config.join("saved").join("Lucky.lua"), "LuckyDB = { odds = 0 }").unwrap();
        std::fs::write(config.join("saved").join("New.lua"), "NewDB = {}").unwrap();
        restore(&config, &zip, "20261109-140312").unwrap();
        assert_eq!(std::fs::read_to_string(config.join("saved").join("Lucky.lua")).unwrap(), "LuckyDB = { odds = 7 }");
        assert!(!config.join("saved").join("New.lua").exists(), "as it was then");
        let before = config.join("backups").join("saved-20261109-140312.zip");
        assert_eq!(names(&before), ["saved-variables.lua", "saved/Lucky.lua", "saved/New.lua"], "the settings from before the restore");
    }

    #[test]
    fn restore_refuses_paths_outside_the_settings() {
        let (_dir, config) = game();
        let zip = config.join("evil.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("saved/Lucky.lua", options).unwrap();
        writer.write_all(b"x").unwrap();
        writer.start_file("../Addons/Evil/Evil.toc", options).unwrap();
        writer.write_all(b"x").unwrap();
        writer.finish().unwrap();
        assert!(matches!(restore(&config, &zip, "20261109-140312"), Err(Error::UnsafePath(_))));
        assert_eq!(std::fs::read_to_string(config.join("saved").join("Lucky.lua")).unwrap(), "LuckyDB = { odds = 7 }", "nothing changed");
        assert!(!config.join("backups").exists(), "not even a backup");
    }
}
