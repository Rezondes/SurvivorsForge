//! What SurvivorsForge installed where: `state.json` in the data folder, one section per AddOns folder.

use crate::registry::{Registry, SourceSpec};
use crate::scan::InstalledAddon;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const FILE: &str = "state.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Verified { registry_id: String },
    #[serde(rename = "github")]
    GitHub { repo: String, spec: SourceSpec },
    Zip { file_name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub folders: Vec<String>,
    /// Release tag, tag name, commit SHA, `zip:<file>` or `toc:<version>` for adopted folders.
    pub version: String,
    pub installed_at: u64,
}

pub const TOC_PREFIX: &str = "toc:";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub entries: Vec<Entry>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    /// Entries with at least one folder missing on disk.
    pub missing: Vec<String>,
    /// Scanned folders no entry owns.
    pub unmanaged: Vec<InstalledAddon>,
}

#[derive(Debug, PartialEq)]
pub enum LoadNote {
    /// The file was unreadable; it was moved to `state.json.bak`.
    CorruptBackedUp,
}

fn key(addons_dir: &Path) -> String {
    let key = addons_dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    // Windows paths ignore case, Linux paths don't.
    if cfg!(windows) { key.to_lowercase() } else { key }
}

fn read_all(data_dir: &Path) -> std::result::Result<BTreeMap<String, State>, ()> {
    match std::fs::read_to_string(data_dir.join(FILE)) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| ()),
        Err(_) => Ok(BTreeMap::new()),
    }
}

impl State {
    pub fn load(data_dir: &Path, addons_dir: &Path) -> (State, Option<LoadNote>) {
        match read_all(data_dir) {
            Ok(mut all) => (all.remove(&key(addons_dir)).unwrap_or_default(), None),
            Err(()) => {
                let _ = std::fs::rename(data_dir.join(FILE), data_dir.join(format!("{FILE}.bak")));
                (State::default(), Some(LoadNote::CorruptBackedUp))
            }
        }
    }

    pub fn save(&self, data_dir: &Path, addons_dir: &Path) -> std::io::Result<()> {
        let mut all = read_all(data_dir).unwrap_or_default();
        all.insert(key(addons_dir), self.clone());
        crate::write_atomic(&data_dir.join(FILE), &serde_json::to_vec_pretty(&all).expect("state serializes"))
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn owner_of(&self, folder: &str) -> Option<&Entry> {
        // As Windows opens folders: "Shared." is the folder "Shared".
        let key = forge_core::folder::folder_key(folder);
        self.entries.iter().find(|e| e.folders.iter().any(|f| forge_core::folder::folder_key(f) == key))
    }

    /// Inserts or replaces the entry with the same id.
    pub fn upsert(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Entry> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(i))
    }

    /// Takes over folders of verified addons that were installed before SurvivorsForge
    /// (or by hand), with the `.toc` version as the best guess.
    pub fn adopt(&mut self, scan: &[InstalledAddon], registry: &Registry) -> bool {
        let mut changed = false;
        for addon in &registry.addons {
            if self.get(&addon.id).is_some() {
                continue;
            }
            let present: Vec<&InstalledAddon> = addon
                .folders
                .iter()
                .filter_map(|f| scan.iter().find(|s| s.folder.eq_ignore_ascii_case(f)))
                .filter(|s| self.owner_of(&s.folder).is_none())
                .collect();
            let Some(first) = present.first() else { continue };
            self.entries.push(Entry {
                id: addon.id.clone(),
                name: addon.name.clone(),
                source: Source::Verified { registry_id: addon.id.clone() },
                folders: present.iter().map(|s| s.folder.clone()).collect(),
                version: format!("{TOC_PREFIX}{}", first.toc.version.clone().unwrap_or_default()),
                installed_at: crate::now_secs(),
            });
            changed = true;
        }
        changed
    }

    pub fn reconcile(&self, scan: &[InstalledAddon]) -> Reconciled {
        let on_disk = |folder: &String| scan.iter().any(|s| s.folder.eq_ignore_ascii_case(folder));
        Reconciled {
            missing: self
                .entries
                .iter()
                .filter(|e| !e.folders.iter().all(on_disk))
                .map(|e| e.id.clone())
                .collect(),
            unmanaged: scan.iter().filter(|s| self.owner_of(&s.folder).is_none()).cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toc::Toc;

    fn scanned(folder: &str, version: &str) -> InstalledAddon {
        InstalledAddon {
            folder: folder.into(),
            toc: Toc { title: folder.into(), version: Some(version.into()), ..Toc::default() },
            disabled: false,
        }
    }

    fn entry(id: &str, folders: &[&str]) -> Entry {
        Entry {
            id: id.into(),
            name: id.into(),
            source: Source::Zip { file_name: "x.zip".into() },
            folders: folders.iter().map(|f| f.to_string()).collect(),
            version: "zip:x.zip".into(),
            installed_at: 1,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let addons = Path::new(r"P:\AddOns");
        let mut state = State::default();
        state.upsert(entry("a", &["A"]));
        state.upsert(Entry {
            source: Source::GitHub { repo: "o/r".into(), spec: SourceSpec::Release { asset: "r-*.zip".into() } },
            ..entry("b", &["B"])
        });
        state.save(dir.path(), addons).unwrap();
        assert_eq!(State::load(dir.path(), addons), (state, None));
        assert_eq!(State::load(dir.path(), Path::new(r"Q:\AddOns")).0, State::default());
    }

    #[test]
    fn reconcile_marks_missing() {
        let mut state = State::default();
        state.upsert(entry("ui", &["SurvivorUI", "SurvivorPlates"]));
        let result = state.reconcile(&[scanned("SurvivorUI", "1")]);
        assert_eq!(result.missing, ["ui"]);
        assert_eq!(state.entries.len(), 1, "entry is kept");
    }

    #[test]
    fn reconcile_reports_unmanaged() {
        let mut state = State::default();
        state.upsert(entry("luckyodds", &["LuckyOdds"]));
        let result = state.reconcile(&[scanned("LuckyOdds", "1"), scanned("Tickfire", "1")]);
        assert!(result.missing.is_empty());
        assert_eq!(result.unmanaged.len(), 1);
        assert_eq!(result.unmanaged[0].folder, "Tickfire");
    }

    #[test]
    fn adopts_known_verified_folders() {
        let mut state = State::default();
        let scan = [scanned("SurvivorPlates", "3.0.36"), scanned("SurvivorUI", "3.0.36"), scanned("Tickfire", "1.0")];
        assert!(state.adopt(&scan, &crate::registry::embedded()));
        assert_eq!(state.entries.len(), 1);
        let e = &state.entries[0];
        assert_eq!(e.id, "survivor-ui");
        assert_eq!(e.version, "toc:3.0.36");
        assert_eq!(e.folders, ["SurvivorUI", "SurvivorPlates"]);
        assert!(!state.adopt(&scan, &crate::registry::embedded()), "second run changes nothing");
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "{ broken").unwrap();
        let (state, note) = State::load(dir.path(), Path::new("x"));
        assert_eq!(state, State::default());
        assert_eq!(note, Some(LoadNote::CorruptBackedUp));
        assert!(dir.path().join("state.json.bak").is_file());
    }
}
