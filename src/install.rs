//! Installs, replaces and removes addon folders without leaving half-written states behind.
//!
//! Work happens in `AddOns/.survivorsforge/`: always the same file system as the addon folders,
//! even when AddOns is a mount or a link to another drive, so `rename` is atomic. The game only
//! loads folders holding a matching `.toc`, so it ignores this one.

use crate::archive;
use crate::state::{Entry, Source, State};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

pub type MoveFn<'a> = &'a mut dyn FnMut(&Path, &Path) -> std::io::Result<()>;

pub struct Request<'a> {
    pub zip: &'a Path,
    pub id: &'a str,
    pub name: &'a str,
    pub source: Source,
    pub version: String,
    /// Hex SHA-256 the platform scanned; a zip that differs is deleted and nothing is installed.
    pub expected_sha256: Option<String>,
}

pub fn work_dir(addons_dir: &Path) -> PathBuf {
    addons_dir.join(".survivorsforge")
}

pub fn install_zip(addons_dir: &Path, state: &mut State, request: Request) -> Result<Entry> {
    install_zip_with(addons_dir, state, request, &mut |from, to| std::fs::rename(from, to))
}

/// Hex SHA-256 of a file.
pub fn sha256_of(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(hex(&Sha256::digest(std::fs::read(path)?)))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn install_zip_with(addons_dir: &Path, state: &mut State, request: Request, mv: MoveFn) -> Result<Entry> {
    if let Some(expected) = &request.expected_sha256 {
        let actual = sha256_of(request.zip)?;
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = std::fs::remove_file(request.zip);
            return Err(Error::HashMismatch { expected: expected.clone(), actual });
        }
    }
    let detected = archive::analyze(request.zip)?;
    // A name Windows reads as another folder ("Shared.", "NUL") could replace or hide one.
    if let Some(bad) = detected.iter().find(|a| !forge_core::folder::windows_safe_folder(&a.name)) {
        return Err(Error::NotInstallable(format!("The folder name \"{}\" is not allowed on Windows", bad.name)));
    }
    for addon in &detected {
        if let Some(owner) = state.owner_of(&addon.name).filter(|o| o.id != request.id) {
            return Err(Error::FolderOwnedBy(owner.name.clone()));
        }
    }

    let run = crate::unique_name();
    let staging = work_dir(addons_dir).join("staging").join(&run);
    let backup = work_dir(addons_dir).join("backup").join(&run);
    std::fs::create_dir_all(&staging)?;
    let result = archive::extract_addons(request.zip, &detected, &staging).map_err(Error::from).and_then(|()| {
        let mut replaced: Vec<String> = detected.iter().map(|a| a.name.clone()).collect();
        if let Some(old) = state.get(request.id) {
            for folder in &old.folders {
                if !replaced.iter().any(|r| r.eq_ignore_ascii_case(folder)) {
                    replaced.push(folder.clone());
                }
            }
        }
        swap(addons_dir, &staging, &backup, &detected.iter().map(|a| a.name.clone()).collect::<Vec<_>>(), &replaced, mv)
    });
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(&backup);
    result?;

    let entry = Entry {
        id: request.id.to_string(),
        name: request.name.to_string(),
        source: request.source,
        folders: detected.into_iter().map(|a| a.name).collect(),
        version: request.version,
        installed_at: crate::now_secs(),
    };
    state.upsert(entry.clone());
    Ok(entry)
}

/// Moves the old folders to `backup`, the new ones from `staging` into AddOns.
/// On any error everything is put back as it was.
fn swap(addons_dir: &Path, staging: &Path, backup: &Path, new: &[String], replaced: &[String], mv: MoveFn) -> Result<()> {
    std::fs::create_dir_all(backup)?;
    let mut backed_up = Vec::new();
    let mut moved_in = Vec::new();
    let outcome = (|| -> std::io::Result<()> {
        for folder in replaced {
            if let Some(actual) = existing_folder(addons_dir, folder) {
                mv(&addons_dir.join(&actual), &backup.join(&actual))?;
                backed_up.push(actual);
            }
        }
        for folder in new {
            mv(&staging.join(folder), &addons_dir.join(folder))?;
            moved_in.push(folder.clone());
        }
        Ok(())
    })();
    if let Err(e) = outcome {
        for folder in &moved_in {
            let _ = std::fs::remove_dir_all(addons_dir.join(folder));
        }
        for folder in &backed_up {
            let _ = std::fs::rename(backup.join(folder), addons_dir.join(folder));
        }
        return Err(e.into());
    }
    Ok(())
}

/// Removes folders from AddOns with `delete` (the recycle bin in the app).
pub fn remove_folders(addons_dir: &Path, folders: &[String], delete: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    for folder in folders {
        if let Some(actual) = existing_folder(addons_dir, folder) {
            delete(&addons_dir.join(actual))?;
        }
    }
    Ok(())
}

/// The folder's name as it exists on disk, matched regardless of case. Linux file systems keep
/// `luckyodds/` and `LuckyOdds/` apart, but for an addon they are the same folder.
fn existing_folder(addons_dir: &Path, name: &str) -> Option<String> {
    if addons_dir.join(name).exists() {
        return Some(name.to_string());
    }
    std::fs::read_dir(addons_dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| forge_core::folder::folder_key(n) == forge_core::folder::folder_key(name))
}

pub fn uninstall(addons_dir: &Path, state: &mut State, id: &str, delete: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    let Some(entry) = state.get(id).cloned() else { return Ok(()) };
    remove_folders(addons_dir, &entry.folders, delete)?;
    state.remove(id);
    Ok(())
}

/// Where removed folders wait for Undo, one folder per removal: in the work folder, so moving
/// there and back is a rename on the same file system, and the game does not load them.
pub fn parking_root(addons_dir: &Path) -> PathBuf {
    work_dir(addons_dir).join("removed")
}

/// A delete for `uninstall` and `remove_folders` that moves each folder into `dir` instead.
pub fn park_into(dir: &Path) -> impl FnMut(&Path) -> Result<()> + '_ {
    move |path| {
        std::fs::create_dir_all(dir)?;
        let name = path.file_name().ok_or_else(|| Error::Io(format!("{} has no name", path.display())))?;
        std::fs::rename(path, dir.join(name)).map_err(Into::into)
    }
}

/// Moves the folders parked in `dir` back into the AddOns folder. Refuses, moving nothing, when
/// one of them is there again (installed anew meanwhile).
pub fn unpark(addons_dir: &Path, dir: &Path) -> Result<Vec<String>> {
    let names: Vec<String> = std::fs::read_dir(dir)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    if let Some(back) = names.iter().find(|n| existing_folder(addons_dir, n).is_some()) {
        return Err(Error::Io(format!("{back} is in the AddOns folder again, so it stays removed")));
    }
    for name in &names {
        std::fs::rename(dir.join(name), addons_dir.join(name))?;
    }
    std::fs::remove_dir(dir)?;
    Ok(names)
}

/// Sends what waits in `dir` (one removal, or the whole `parking_root`) to `delete`, the recycle
/// bin in the app: Undo is over for it.
pub fn flush_parked(dir: &Path, delete: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
    for entry in entries.flatten() {
        delete(&entry.path())?;
    }
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

pub fn to_recycle_bin(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|e| Error::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Entry, Source};

    fn folders(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    fn setup() -> (tempfile::TempDir, PathBuf, State) {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("Addons");
        for f in ["Lucky", "Lucky_Options", "Other"] {
            std::fs::create_dir_all(addons.join(f)).unwrap();
            std::fs::write(addons.join(f).join(format!("{f}.toc")), "## Title: x").unwrap();
        }
        let mut state = State::default();
        state.upsert(Entry {
            id: "lucky".into(),
            name: "Lucky".into(),
            source: Source::Zip { file_name: "lucky.zip".into() },
            folders: vec!["Lucky".into(), "Lucky_Options".into()],
            version: "zip:lucky.zip".into(),
            installed_at: 0,
        });
        (dir, addons, state)
    }

    #[test]
    fn a_removed_addon_waits_in_the_work_folder() {
        let (_dir, addons, mut state) = setup();
        let parked = parking_root(&addons).join("lucky-1");
        uninstall(&addons, &mut state, "lucky", &mut park_into(&parked)).unwrap();
        assert_eq!(folders(&addons), ["Other"]);
        assert_eq!(folders(&parked), ["Lucky", "Lucky_Options"]);
        assert!(state.get("lucky").is_none());
    }

    #[test]
    fn undo_puts_the_folders_back() {
        let (_dir, addons, mut state) = setup();
        let parked = parking_root(&addons).join("lucky-1");
        uninstall(&addons, &mut state, "lucky", &mut park_into(&parked)).unwrap();
        assert_eq!(unpark(&addons, &parked).unwrap(), ["Lucky", "Lucky_Options"]);
        assert_eq!(folders(&addons), ["Lucky", "Lucky_Options", "Other"]);
        assert!(!parked.exists());
        assert!(addons.join("Lucky").join("Lucky.toc").exists(), "with their files");
    }

    #[test]
    fn undo_refuses_when_a_folder_came_back() {
        let (_dir, addons, mut state) = setup();
        let parked = parking_root(&addons).join("lucky-1");
        uninstall(&addons, &mut state, "lucky", &mut park_into(&parked)).unwrap();
        std::fs::create_dir(addons.join("lucky_options")).unwrap();
        let e = unpark(&addons, &parked).unwrap_err();
        assert!(e.to_string().contains("Lucky_Options is in the AddOns folder again"), "{e}");
        assert_eq!(folders(&parked), ["Lucky", "Lucky_Options"], "nothing moved");
        assert!(!addons.join("Lucky").exists());
    }

    #[test]
    fn what_waits_goes_to_the_bin_at_last() {
        let (_dir, addons, mut state) = setup();
        let root = parking_root(&addons);
        uninstall(&addons, &mut state, "lucky", &mut park_into(&root.join("lucky-1"))).unwrap();
        remove_folders(&addons, &["Other".into()], &mut park_into(&root.join("other-2"))).unwrap();
        let mut binned = Vec::new();
        flush_parked(&root, &mut |p| {
            binned.push(p.file_name().unwrap().to_string_lossy().into_owned());
            std::fs::remove_dir_all(p).map_err(Into::into)
        })
        .unwrap();
        binned.sort();
        assert_eq!(binned, ["lucky-1", "other-2"]);
        assert!(!root.exists());
        assert!(flush_parked(&root, &mut |_| panic!("nothing left")).is_ok());
    }
}
