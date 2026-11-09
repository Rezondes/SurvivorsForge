//! Which addons the game loads: `account.txt` in the AddOns folder, one `<Folder>: enabled` or
//! `<Folder>: disabled` per line, for every hero alike. The game writes it when its AddOns window
//! is closed with Okay and keeps lines of addons that are gone. An addon without a line takes the
//! `## DefaultState` of its `.toc`.
//!
//! Before the launcher starts the game, every addon it has on (in the AddOns folder, not in
//! `AddOns.disabled`) is switched on here too, so one switched off in the game does not stay off.

use std::path::Path;

pub const FILE: &str = "account.txt";

/// `text` with each of `active` set to `enabled`: an existing line (any case) is changed in place,
/// a missing one is added at the end; every other line stays as it was. None when nothing changes.
pub fn enable_in(text: &str, active: &[String]) -> Option<String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut changed = false;
    for folder in active {
        let at = lines.iter().position(|l| l.split_once(':').is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(folder)));
        match at {
            Some(i) => {
                let (name, value) = lines[i].split_once(':').expect("found by its colon");
                if !value.trim().eq_ignore_ascii_case("enabled") {
                    lines[i] = format!("{}: enabled", name.trim());
                    changed = true;
                }
            }
            None => {
                lines.push(format!("{folder}: enabled"));
                changed = true;
            }
        }
    }
    changed.then(|| {
        let mut out = lines.join(newline);
        out.push_str(newline);
        out
    })
}

/// Switches `active` on in the game's `account.txt` of `addons_dir`. A missing file stays missing:
/// then the game loads every addon. Ok(true) when the file was changed.
pub fn enable(addons_dir: &Path, active: &[String]) -> std::io::Result<bool> {
    let path = addons_dir.join(FILE);
    let text = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "account.txt is not text"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match enable_in(&text, active) {
        Some(new) => crate::write_atomic(&path, new.as_bytes()).map(|_| true),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn active_addons_are_switched_on_and_the_rest_stays() {
        let text = "SurvivorUI: enabled\nASBuilder: disabled\nazerloot: disabled\nVulchansCombatText: enabled\n";
        let new = enable_in(text, &names(&["ASBuilder", "Azerloot", "AZSGW2UI"])).unwrap();
        assert_eq!(new, "SurvivorUI: enabled\nASBuilder: enabled\nazerloot: enabled\nVulchansCombatText: enabled\nAZSGW2UI: enabled\n");
    }

    #[test]
    fn nothing_to_change_writes_nothing() {
        assert_eq!(enable_in("ASBuilder: enabled\n", &names(&["asbuilder"])), None);
        assert_eq!(enable_in("ASBuilder: disabled\n", &[]), None, "the game's own choice for the others");
    }

    #[test]
    fn windows_line_ends_stay() {
        assert_eq!(enable_in("A: disabled\r\nB: disabled\r\n", &names(&["B"])).unwrap(), "A: disabled\r\nB: enabled\r\n");
    }

    #[test]
    fn the_file_is_changed_in_place_and_a_missing_one_stays_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!enable(dir.path(), &names(&["A"])).unwrap());
        assert!(!dir.path().join(FILE).exists());
        std::fs::write(dir.path().join(FILE), "A: disabled\n").unwrap();
        assert!(enable(dir.path(), &names(&["A"])).unwrap());
        assert_eq!(std::fs::read_to_string(dir.path().join(FILE)).unwrap(), "A: enabled\n");
        assert!(!enable(dir.path(), &names(&["A"])).unwrap(), "already on");
        std::fs::write(dir.path().join(FILE), [0xff, 0xfe, 0x00]).unwrap();
        assert!(enable(dir.path(), &names(&["A"])).is_err(), "not text: left alone");
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), [0xff, 0xfe, 0x00]);
    }
}
