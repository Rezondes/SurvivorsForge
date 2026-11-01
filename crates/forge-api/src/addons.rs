//! Addon rules that need no database: field validation and reserved folder names.

use crate::error::ApiError;
use axum::http::StatusCode;

/// Folder names of the game's own interface. Every `Blizzard_*` folder is reserved too.
// ponytail: only the Blizzard_ prefix so far; add the client's stock folders here once known.
pub const RESERVED_FOLDERS: &[&str] = &[];

pub const MAX_FOLDER_LEN: usize = 64;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_SUMMARY_LEN: usize = 200;
pub const MAX_DESCRIPTION_LEN: usize = 50_000;

fn invalid(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

/// `^[a-z0-9][a-z0-9-]{1,48}$`
pub fn validate_slug(slug: &str) -> Result<(), ApiError> {
    let bytes = slug.as_bytes();
    let ok = (2..=49).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    if ok { Ok(()) } else { Err(invalid("invalid_slug", "The slug must be 2 to 49 of a-z, 0-9 and -, starting with a letter or digit")) }
}

pub fn validate_name(name: &str) -> Result<(), ApiError> {
    let len = name.trim().chars().count();
    if (1..=MAX_NAME_LEN).contains(&len) && !name.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(invalid("invalid_name", format!("The name must be 1 to {MAX_NAME_LEN} characters")))
    }
}

pub fn validate_summary(summary: &str) -> Result<(), ApiError> {
    if summary.chars().count() <= MAX_SUMMARY_LEN && !summary.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(invalid("invalid_summary", format!("The summary must be at most {MAX_SUMMARY_LEN} characters on one line")))
    }
}

pub fn validate_description(description: &str) -> Result<(), ApiError> {
    if description.chars().count() <= MAX_DESCRIPTION_LEN {
        Ok(())
    } else {
        Err(invalid("description_too_long", format!("The description must be at most {MAX_DESCRIPTION_LEN} characters")))
    }
}

pub fn validate_source(source_type: &str, asset_pattern: Option<&str>) -> Result<(), ApiError> {
    match (source_type, asset_pattern) {
        ("tag", _) => Ok(()),
        ("release", Some(pattern))
            if pattern.to_ascii_lowercase().ends_with(".zip")
                && pattern.len() <= 128
                && !pattern.contains(['/', '\\'])
                && !pattern.chars().any(char::is_control) =>
        {
            Ok(())
        }
        ("release", _) => Err(invalid("invalid_asset_pattern", "A release addon needs an asset pattern ending in .zip, like MyAddon-v*.zip")),
        _ => Err(invalid("invalid_source_type", "The source type must be release or tag")),
    }
}

pub fn validate_folder(folder: &str) -> Result<(), ApiError> {
    let bad = folder.is_empty()
        || folder.chars().count() > MAX_FOLDER_LEN
        || folder.contains(['/', '\\'])
        || folder.contains("..")
        || folder.trim() != folder
        || folder.chars().any(char::is_control)
        // Windows reads "Bagnon." as "Bagnon": one folder must not stand in for another.
        || !forge_core::folder::windows_safe_folder(folder);
    if bad {
        return Err(invalid("invalid_folder", format!("\"{}\" is not a valid folder name", folder.escape_default())));
    }
    let lower = folder.to_ascii_lowercase();
    if lower.starts_with("blizzard_") || RESERVED_FOLDERS.iter().any(|r| r.eq_ignore_ascii_case(folder)) {
        return Err(invalid("folder_reserved", format!("\"{folder}\" is a folder of the game itself")));
    }
    Ok(())
}

/// At least one folder, each valid, none twice.
pub fn validate_folders(folders: &[String]) -> Result<(), ApiError> {
    if folders.is_empty() {
        return Err(invalid("invalid_folder", "An addon needs at least one folder"));
    }
    for (i, folder) in folders.iter().enumerate() {
        validate_folder(folder)?;
        if folders[..i].iter().any(|f| forge_core::folder::folder_key(f) == forge_core::folder::folder_key(folder)) {
            return Err(invalid("invalid_folder", format!("\"{folder}\" is listed twice")));
        }
    }
    Ok(())
}

/// What a user may do with one addon. Every endpoint asks this, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Permissions {
    /// Repo, asset pattern, folders, dependencies, compatibility, contributors, deleting.
    pub owner: bool,
    /// Summary and description.
    pub edit_description: bool,
    pub edit_images: bool,
    pub admin: bool,
}

/// `member` holds the contributor flags `(can_edit_description, can_edit_images)` when the user is one.
/// `owner_id` is `None` for an imported addon nobody claimed yet: then only admins own it.
pub fn permissions(user_id: i64, is_admin: bool, owner_id: Option<i64>, member: Option<(bool, bool)>) -> Permissions {
    let owner = is_admin || owner_id == Some(user_id);
    let (description, images) = member.unwrap_or((false, false));
    Permissions { owner, edit_description: owner || description, edit_images: owner || images, admin: is_admin }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_matrix() {
        let all = Permissions { owner: true, edit_description: true, edit_images: true, admin: false };
        assert_eq!(permissions(1, false, Some(1), None), all, "owner");
        assert_eq!(permissions(2, true, Some(1), None), Permissions { admin: true, ..all }, "admin may do what the owner may");
        assert_eq!(permissions(2, false, Some(1), None), Permissions::default(), "stranger");
        assert_eq!(permissions(2, false, Some(1), Some((false, false))), Permissions::default(), "member without flags");
        assert_eq!(permissions(2, false, Some(1), Some((true, false))), Permissions { edit_description: true, ..Default::default() });
        assert_eq!(permissions(2, false, Some(1), Some((false, true))), Permissions { edit_images: true, ..Default::default() });
        assert_eq!(permissions(2, false, None, None), Permissions::default(), "unclaimed: nobody but admins");
    }

    fn code<T>(result: Result<T, ApiError>) -> &'static str {
        result.err().map(|e| e.code).unwrap_or("ok")
    }

    #[test]
    fn invalid_or_reserved_folders_rejected() {
        assert_eq!(code(validate_folder("LuckyOdds")), "ok");
        assert_eq!(code(validate_folder("Blizzard_UI")), "folder_reserved");
        assert_eq!(code(validate_folder("blizzard_raidui")), "folder_reserved");
        for bad in ["a/b", "a\\b", "..", "x..y", "x\0", "", " Lead", &"x".repeat(65)] {
            assert_eq!(code(validate_folder(bad)), "invalid_folder", "{bad:?}");
        }
        assert_eq!(code(validate_folder(&"x".repeat(64))), "ok");
        assert_eq!(code(validate_folders(&["A".into(), "a".into()])), "invalid_folder");
        assert_eq!(code(validate_folders(&[])), "invalid_folder");
    }

    #[test]
    fn field_validation() {
        assert_eq!(code(validate_slug("lucky-odds")), "ok");
        assert_eq!(code(validate_slug("9lives")), "ok");
        for bad in ["a", "-lead", "Upper", "under_score", "space s", &"a".repeat(50)] {
            assert_eq!(code(validate_slug(bad)), "invalid_slug", "{bad}");
        }
        assert_eq!(code(validate_name("Lucky Odds")), "ok");
        assert_eq!(code(validate_name("  ")), "invalid_name");
        assert_eq!(code(validate_name(&"n".repeat(65))), "invalid_name");
        assert_eq!(code(validate_summary(&"s".repeat(200))), "ok");
        assert_eq!(code(validate_summary(&"s".repeat(201))), "invalid_summary");
        assert_eq!(code(validate_summary("two\nlines")), "invalid_summary");
        assert_eq!(code(validate_source("release", Some("Lucky-v*.zip"))), "ok");
        assert_eq!(code(validate_source("release", Some("Lucky-v*.ZIP"))), "ok");
        assert_eq!(code(validate_source("release", Some("Lucky-v*.rar"))), "invalid_asset_pattern");
        assert_eq!(code(validate_source("release", None)), "invalid_asset_pattern");
        assert_eq!(code(validate_source("tag", None)), "ok");
        assert_eq!(code(validate_source("branch", None)), "invalid_source_type");
    }
}
