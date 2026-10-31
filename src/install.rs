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
        .find(|n| n.eq_ignore_ascii_case(name))
}

pub fn uninstall(addons_dir: &Path, state: &mut State, id: &str, delete: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    let Some(entry) = state.get(id).cloned() else { return Ok(()) };
    remove_folders(addons_dir, &entry.folders, delete)?;
    state.remove(id);
    Ok(())
}

pub fn to_recycle_bin(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|e| Error::Io(e.to_string()))
}
