//! Finds the addons installed in an AddOns folder.

use crate::toc::Toc;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstalledAddon {
    pub folder: String,
    pub toc: Toc,
    /// It waits in `disabled_dir`: the game does not load it.
    pub disabled: bool,
}

/// Where disabled addons wait: `AddOns.disabled` beside the AddOns folder, where the game does not
/// look and the user still finds them.
pub fn disabled_dir(addons_dir: &Path) -> PathBuf {
    addons_dir.parent().unwrap_or(addons_dir).join("AddOns.disabled")
}

/// The addons of the AddOns folder and the disabled ones beside it. An addon is a folder `<Name>/`
/// holding `<Name>.toc` (the WoW 1.12 rule). Files (like `account.txt`) and hidden folders are
/// skipped and never read. A disabled folder that is also in AddOns counts as the one in AddOns.
pub fn scan_addons(addons_dir: &Path) -> std::io::Result<Vec<InstalledAddon>> {
    let mut found = scan_dir(addons_dir, false)?;
    let disabled = disabled_dir(addons_dir);
    if disabled.is_dir() {
        for addon in scan_dir(&disabled, true)? {
            if !found.iter().any(|f| f.folder.eq_ignore_ascii_case(&addon.folder)) {
                found.push(addon);
            }
        }
    }
    found.sort_by_key(|a| a.folder.to_lowercase());
    Ok(found)
}

fn scan_dir(dir: &Path, disabled: bool) -> std::io::Result<Vec<InstalledAddon>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        if folder.starts_with('.') {
            continue;
        }
        if let Ok(bytes) = std::fs::read(entry.path().join(format!("{folder}.toc"))) {
            found.push(InstalledAddon { toc: Toc::parse_bytes(&bytes, &folder), folder, disabled });
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_lists_only_folders_with_matching_toc() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::create_dir_all(p.join("A")).unwrap();
        fs::write(p.join("A/A.toc"), "## Version: 1.0").unwrap();
        fs::create_dir_all(p.join("B")).unwrap();
        fs::write(p.join("B/other.toc"), "").unwrap();
        fs::write(p.join("account.txt"), "secret").unwrap();
        let found = scan_addons(p).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].folder, "A");
        assert_eq!(found[0].toc.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn disabled_addons_count_as_installed() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("Addons");
        for (root, folder) in [(addons.clone(), "A"), (disabled_dir(&addons), "B"), (disabled_dir(&addons), "a")] {
            fs::create_dir_all(root.join(folder)).unwrap();
            fs::write(root.join(folder).join(format!("{folder}.toc")), "").unwrap();
        }
        assert_eq!(disabled_dir(&addons), dir.path().join("AddOns.disabled"));
        let found: Vec<(String, bool)> = scan_addons(&addons).unwrap().into_iter().map(|a| (a.folder, a.disabled)).collect();
        assert_eq!(found, [("A".to_string(), false), ("B".to_string(), true)], "the one in AddOns wins");
    }
}
