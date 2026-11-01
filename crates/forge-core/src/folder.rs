//! Addon folder names as Windows sees them. Windows drops dots and spaces at the end of a path
//! part and ignores case, so `Bagnon.` and `bagnon` are the folder `Bagnon`; and it keeps device
//! names like `CON` for itself. Registration, scanner and launcher all compare folders by
//! `folder_key` and accept only names `windows_safe_folder` allows, so one name cannot stand in
//! for another.

/// Device names Windows reserves in every folder, with or without an extension.
const DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2",
    "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The folder Windows opens for `name`: lowercase, without trailing dots and spaces.
pub fn folder_key(name: &str) -> String {
    name.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// `name` means the same folder on every system: no trailing dot or space, no character Windows
/// forbids, no device name, and no `~` before a digit. Windows opens the 8.3 short name
/// `SHAGUT~1` as the folder `ShaguTweaks`, and every short name it makes has a `~` and a number.
pub fn windows_safe_folder(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_lowercase();
    !name.is_empty()
        && !name.ends_with(['.', ' '])
        && !name.contains(['<', '>', ':', '"', '|', '?', '*', '/', '\\'])
        && !name.chars().any(char::is_control)
        && !DEVICES.contains(&stem.as_str())
        && !name.as_bytes().windows(2).any(|w| w[0] == b'~' && w[1].is_ascii_digit())
}
