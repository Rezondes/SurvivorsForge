//! Profiles: named sets of addons, each on or off. Applying one installs the platform addons it
//! has and the launcher lacks, enables the ones it has on and disables all others; nothing is
//! deleted. Kept in `profiles.json` in the data folder.

use crate::controller::{Row, UNMANAGED_PREFIX};
use crate::state::{Source, State};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "profiles.json";
pub const NAME_MAX: usize = 40;

/// Where an addon of a profile comes from, as another computer finds it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProfileSource {
    /// A platform addon: `id` is its registry id.
    Platform { id: String },
    GitHub { repo: String },
    /// Installed from a zip or found in the folder: only this computer has it.
    Local { folder: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileAddon {
    pub name: String,
    pub source: ProfileSource,
    pub enabled: bool,
    /// The version it is kept at on purpose; none for the newest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub addons: Vec<ProfileAddon>,
    /// Seconds since 1970 of the last change; the newer side wins a sync.
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profiles {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// The profile applied last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Profiles deleted here that the account still has, until the next sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
}

impl Profiles {
    pub fn load(data_dir: &Path) -> Profiles {
        std::fs::read_to_string(data_dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        crate::write_atomic(&data_dir.join(FILE), &serde_json::to_vec_pretty(self).expect("profiles serialize"))
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Adds `profile` or replaces the one of the same name (any case).
    pub fn put(&mut self, profile: Profile) {
        self.deleted.retain(|d| !d.eq_ignore_ascii_case(&profile.name));
        match self.profiles.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&profile.name)) {
            Some(old) => *old = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.profiles.retain(|p| !p.name.eq_ignore_ascii_case(name));
        if self.active.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(name)) {
            self.active = None;
        }
        self.deleted.push(name.to_string());
    }

    /// `name`, or with " (2)", " (3)"... when a profile has it already.
    pub fn free_name(&self, name: &str) -> String {
        if self.get(name).is_none() {
            return name.to_string();
        }
        (2..).map(|n| format!("{name} ({n})")).find(|n| self.get(n).is_none()).expect("a free name")
    }
}

/// A name as typed: trimmed, 1 to `NAME_MAX` characters.
pub fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    match name.chars().count() {
        0 => Err("Give the profile a name".into()),
        n if n > NAME_MAX => Err(format!("A profile name has at most {NAME_MAX} characters")),
        _ => Ok(name.to_string()),
    }
}

/// The installed addons as a profile's list: each with where it comes from and whether it is on.
pub fn snapshot(rows: &[Row], state: &State, held: &[String]) -> Vec<ProfileAddon> {
    rows.iter()
        .map(|row| {
            let entry = state.get(&row.id);
            let source = match entry.map(|e| &e.source) {
                Some(Source::Verified { registry_id }) => ProfileSource::Platform { id: registry_id.clone() },
                Some(Source::GitHub { repo, .. }) => ProfileSource::GitHub { repo: repo.clone() },
                _ => ProfileSource::Local { folder: row.id.strip_prefix(UNMANAGED_PREFIX).unwrap_or(&row.id).to_string() },
            };
            let version = entry.filter(|e| held.contains(&e.id)).map(|e| e.version.clone());
            ProfileAddon { name: row.name.clone(), source, enabled: !row.disabled, version }
        })
        .collect()
}

/// What applying a profile does; row ids for the ones installed, registry ids to install.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfilePlan {
    pub install: Vec<(String, String)>,
    pub enable: Vec<(String, String)>,
    pub disable: Vec<(String, String)>,
    /// Names of addons only the computer that made the profile has (zips, loose folders), and
    /// GitHub repos to add by hand.
    pub not_here: Vec<String>,
}

impl ProfilePlan {
    pub fn is_empty(&self) -> bool {
        self.install.is_empty() && self.enable.is_empty() && self.disable.is_empty()
    }

    /// The plan in words, for the question before it is applied.
    pub fn describe(&self) -> String {
        let names = |list: &[(String, String)]| list.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
        let mut lines = Vec::new();
        if !self.install.is_empty() {
            lines.push(format!("Install: {}", names(&self.install)));
        }
        if !self.enable.is_empty() {
            lines.push(format!("Enable: {}", names(&self.enable)));
        }
        if !self.disable.is_empty() {
            lines.push(format!("Disable: {}", names(&self.disable)));
        }
        if !self.not_here.is_empty() {
            lines.push(format!("Not on this computer, add them by hand: {}", self.not_here.join(", ")));
        }
        if self.is_empty() {
            lines.insert(0, "The addons already match this profile.".into());
        }
        lines.join("\n\n")
    }
}

/// The installed row a profile's addon stands for, if this computer has it.
fn installed<'a>(addon: &ProfileAddon, rows: &'a [Row], state: &State) -> Option<&'a Row> {
    rows.iter().find(|row| {
        let source = state.get(&row.id).map(|e| &e.source);
        match (&addon.source, source) {
            (ProfileSource::Platform { id }, Some(Source::Verified { registry_id })) => id == registry_id,
            (ProfileSource::GitHub { repo }, Some(Source::GitHub { repo: r, .. })) => repo.eq_ignore_ascii_case(r),
            (ProfileSource::Local { folder }, _) => row.id.strip_prefix(UNMANAGED_PREFIX).unwrap_or(&row.id).eq_ignore_ascii_case(folder),
            _ => false,
        }
    })
}

/// What applying `profile` to the installed `rows` does. `on_platform` says whether the platform
/// still has a registry id; one it does not have is not installable.
pub fn plan(profile: &Profile, rows: &[Row], state: &State, on_platform: &dyn Fn(&str) -> bool) -> ProfilePlan {
    let mut plan = ProfilePlan::default();
    let mut kept: Vec<&str> = Vec::new();
    for addon in &profile.addons {
        match installed(addon, rows, state) {
            Some(row) => {
                kept.push(&row.id);
                if addon.enabled && row.disabled {
                    plan.enable.push((row.id.clone(), row.name.clone()));
                } else if !addon.enabled && !row.disabled {
                    plan.disable.push((row.id.clone(), row.name.clone()));
                }
            }
            None if !addon.enabled => {}
            None => match &addon.source {
                ProfileSource::Platform { id } if on_platform(id) => plan.install.push((id.clone(), addon.name.clone())),
                ProfileSource::GitHub { repo } => plan.not_here.push(format!("{} (github.com/{repo})", addon.name)),
                _ => plan.not_here.push(addon.name.clone()),
            },
        }
    }
    for row in rows {
        if !row.disabled && !kept.contains(&row.id.as_str()) {
            plan.disable.push((row.id.clone(), row.name.clone()));
        }
    }
    plan
}

/// Whether the installed addons still match `profile` (the active one shows "modified" if not).
pub fn matches(profile: &Profile, rows: &[Row], state: &State) -> bool {
    plan(profile, rows, state, &|_| true).is_empty() && profile.addons.iter().filter(|a| a.enabled).all(|a| installed(a, rows, state).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Badge, RowState};
    use crate::state::Entry;

    fn row(id: &str, disabled: bool) -> Row {
        Row {
            id: id.into(),
            name: id.to_uppercase(),
            author: String::new(),
            version: String::new(),
            badge: Badge::Verified,
            state: RowState::UpToDate,
            description: String::new(),
            repo: None,
            out_of_date: false,
            folders: vec![id.into()],
            disabled,
        }
    }

    fn state() -> State {
        let entry = |id: &str, source: Source| Entry { id: id.into(), name: id.to_uppercase(), source, folders: vec![id.into()], version: "v1".into(), installed_at: 0 };
        State {
            entries: vec![
                entry("lucky", Source::Verified { registry_id: "lucky".into() }),
                entry("plates", Source::GitHub { repo: "o/plates".into(), spec: crate::registry::SourceSpec::Tag }),
                entry("mine", Source::Zip { file_name: "mine.zip".into() }),
            ],
        }
    }

    #[test]
    fn a_snapshot_keeps_where_each_addon_comes_from() {
        let rows = vec![row("lucky", false), row("plates", true), row("mine", false), row("folder:Loose", false)];
        let list = snapshot(&rows, &state(), &["lucky".into()]);
        assert_eq!(list[0], ProfileAddon { name: "LUCKY".into(), source: ProfileSource::Platform { id: "lucky".into() }, enabled: true, version: Some("v1".into()) });
        assert_eq!(list[1].source, ProfileSource::GitHub { repo: "o/plates".into() });
        assert!(!list[1].enabled);
        assert_eq!(list[2].source, ProfileSource::Local { folder: "mine".into() });
        assert_eq!(list[3].source, ProfileSource::Local { folder: "Loose".into() });
    }

    #[test]
    fn applying_installs_enables_and_disables_but_deletes_nothing() {
        let rows = vec![row("lucky", true), row("plates", false), row("mine", false)];
        let addon = |name: &str, source: ProfileSource, enabled: bool| ProfileAddon { name: name.into(), source, enabled, version: None };
        let profile = Profile {
            name: "Raid".into(),
            updated_at: 0,
            addons: vec![
                addon("Lucky", ProfileSource::Platform { id: "lucky".into() }, true),
                addon("Dragon", ProfileSource::Platform { id: "dragon".into() }, true),
                addon("Gone", ProfileSource::Platform { id: "gone".into() }, true),
                addon("Bars", ProfileSource::GitHub { repo: "o/bars".into() }, true),
                addon("Theirs", ProfileSource::Local { folder: "Theirs".into() }, true),
                addon("Off", ProfileSource::Platform { id: "off".into() }, false),
            ],
        };
        let plan = plan(&profile, &rows, &state(), &|id| id != "gone");
        assert_eq!(plan.install, [("dragon".to_string(), "Dragon".to_string())]);
        assert_eq!(plan.enable, [("lucky".to_string(), "LUCKY".to_string())]);
        assert_eq!(plan.disable.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["plates", "mine"], "the ones the profile does not have");
        assert_eq!(plan.not_here, ["Gone", "Bars (github.com/o/bars)", "Theirs"]);
        assert!(plan.describe().contains("Install: Dragon"));
        assert!(!matches(&profile, &rows, &state()));
    }

    #[test]
    fn a_profile_made_here_matches_until_something_changes() {
        let rows = vec![row("lucky", false), row("plates", true)];
        let profile = Profile { name: "Solo".into(), addons: snapshot(&rows, &state(), &[]), updated_at: 0 };
        assert!(matches(&profile, &rows, &state()));
        assert!(plan(&profile, &rows, &state(), &|_| true).is_empty());
        assert!(plan(&profile, &rows, &state(), &|_| true).describe().starts_with("The addons already match"));
        let changed = vec![row("lucky", true), row("plates", true)];
        assert!(!matches(&profile, &changed, &state()), "modified");
    }

    #[test]
    fn names_are_trimmed_bounded_and_unique() {
        assert_eq!(valid_name("  Raid  "), Ok("Raid".into()));
        assert!(valid_name("   ").is_err());
        assert!(valid_name(&"x".repeat(41)).is_err());
        let mut profiles = Profiles::default();
        profiles.put(Profile { name: "Raid".into(), addons: Vec::new(), updated_at: 1 });
        assert_eq!(profiles.free_name("raid"), "raid (2)");
        assert_eq!(profiles.free_name("Solo"), "Solo");
        profiles.put(Profile { name: "RAID".into(), addons: Vec::new(), updated_at: 2 });
        assert_eq!(profiles.profiles.len(), 1, "the same name replaces");
        profiles.active = Some("raid".into());
        profiles.remove("Raid");
        assert!(profiles.profiles.is_empty() && profiles.active.is_none());
        assert_eq!(profiles.deleted, ["Raid"], "the account forgets it at the next sync");
    }

    #[test]
    fn profiles_round_trip_through_their_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Profiles::load(dir.path()), Profiles::default());
        let mut profiles = Profiles::default();
        profiles.put(Profile { name: "Raid".into(), addons: snapshot(&[row("lucky", false)], &state(), &[]), updated_at: 5 });
        profiles.save(dir.path()).unwrap();
        assert_eq!(Profiles::load(dir.path()), profiles);
    }
}
