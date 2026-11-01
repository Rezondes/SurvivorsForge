//! Rules for the zip itself: paths, file types, sizes and addon folders. Works on bytes in memory.

use crate::finding::{Report, Severity};
use forge_core::archive::{MAX_TOC_BYTES, analyze_reader, safe_components};
use forge_core::folder::{folder_key, windows_safe_folder};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

pub const MAX_ENTRIES: usize = 5000;
pub const MAX_ENTRY_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_RATIO: u64 = 100;
/// The ratio rule only applies from this size on: small text files compress well and are harmless.
pub const RATIO_MIN_BYTES: u64 = 1024 * 1024;
pub const MAX_DEPTH: usize = 10;

pub const ALLOWED_EXTENSIONS: &[&str] =
    &["lua", "toc", "xml", "tga", "blp", "png", "jpg", "jpeg", "ttf", "otf", "mp3", "ogg", "wav", "txt", "md"];
const LICENSE_NAMES: &[&str] = &["license", "licence", "copying", "readme", "changelog", "credits"];
/// Files whose content later rules read. Everything else is only sniffed.
const KEPT_EXTENSIONS: &[&str] = &["lua", "toc", "xml"];

/// A file of the zip whose content the content rules need.
pub struct TextFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// `path` in lowercase with `/` separators and without `.`, empty parts and `dir/..`, the way the
/// client opens it: `./A.lua`, `Sub//B.lua` and `x/../C.lua` are files of the zip.
pub(crate) fn normal_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let path = path.replace('\\', "/").to_ascii_lowercase();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// The zip path that `path`, written in a file of the folder `dir`, opens: relative to `dir`, or
/// from the game folder (`Interface\AddOns\Name\a.lua`). `None` for a file of the game itself,
/// like `..\..\FrameXML\Fonts.xml`, which is not in the zip.
pub(crate) fn resolve(dir: &str, path: &str) -> Option<String> {
    let from_game = normal_path(path);
    let resolved = match from_game.strip_prefix("interface/addons/") {
        Some(rest) => rest.to_string(),
        None => normal_path(&format!("{dir}/{path}")),
    };
    (!resolved.starts_with("..")).then_some(resolved)
}

fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase())
}

fn is_hidden(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("__macosx/")
        || lower.split('/').any(|p| p == ".git" || p == ".github")
        || name.starts_with(".git")
        || name == ".editorconfig"
        || name == ".pkgmeta"
        || name.ends_with(".ds_store")
        || name == "thumbs.db"
}

/// What the first bytes say a file is, if it is something an addon must never contain.
pub fn sniff(head: &[u8]) -> Option<(&'static str, &'static str)> {
    const EXECUTABLES: &[(&[u8], &str)] = &[
        (b"MZ", "a Windows program"),
        (b"\x7fELF", "a Linux program"),
        (b"\xfe\xed\xfa\xce", "a macOS program"),
        (b"\xfe\xed\xfa\xcf", "a macOS program"),
        (b"\xce\xfa\xed\xfe", "a macOS program"),
        (b"\xcf\xfa\xed\xfe", "a macOS program"),
        (b"\xca\xfe\xba\xbe", "a macOS program"),
        (b"#!", "a script"),
    ];
    const ARCHIVES: &[(&[u8], &str)] = &[
        (b"PK\x03\x04", "a zip archive"),
        (b"PK\x05\x06", "a zip archive"),
        (b"Rar!", "a RAR archive"),
        (b"7z\xbc\xaf\x27\x1c", "a 7z archive"),
        (b"\x1f\x8b", "a gzip archive"),
        (b"BZh", "a bzip2 archive"),
        (b"\xfd7zXZ\x00", "an xz archive"),
    ];
    if let Some((_, what)) = EXECUTABLES.iter().find(|(magic, _)| head.starts_with(magic)) {
        return Some(("zip.executable", what));
    }
    if let Some((_, what)) = ARCHIVES.iter().find(|(magic, _)| head.starts_with(magic)) {
        return Some(("zip.nested_archive", what));
    }
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Some(("zip.nested_archive", "a tar archive"));
    }
    None
}

/// Runs the zip rules and returns the `.lua`, `.toc` and `.xml` files for the content rules.
pub fn check(zip: &[u8], expected_folders: &[String], foreign_folders: &[String], report: &mut Report) -> Vec<TextFile> {
    let mut archive = match zip::ZipArchive::new(Cursor::new(zip)) {
        Ok(archive) => archive,
        Err(e) => {
            report.add("zip.unreadable", Severity::Critical, "", None, format!("The zip cannot be opened: {e}"));
            return Vec::new();
        }
    };
    if archive.len() > MAX_ENTRIES {
        report.add("zip.bomb", Severity::Critical, "", None, format!("The zip has {} entries, more than {MAX_ENTRIES}", archive.len()));
        return Vec::new();
    }

    let mut texts = Vec::new();
    let mut total: u64 = 0;
    let mut unsafe_paths = false;
    let mut bomb = false;
    let mut deep_reported = false;
    for i in 0..archive.len() {
        let (path, is_dir, encrypted, mode, compressed) = match archive.by_index_raw(i) {
            Ok(entry) => (
                String::from_utf8_lossy(entry.name_raw()).into_owned(),
                entry.is_dir(),
                entry.encrypted(),
                entry.unix_mode(),
                entry.compressed_size(),
            ),
            Err(e) => {
                report.add("zip.unreadable", Severity::Critical, "", None, format!("Entry {i} cannot be read: {e}"));
                continue;
            }
        };
        // Zips packed on Windows may use `\` as separator: a separator, not an escape. `..`,
        // absolute paths and drive letters stay forbidden in either form.
        let path = match safe_components(&path) {
            Ok(parts) if !path.contains('\0') => parts.join("/"),
            _ => {
                report.add("zip.path_traversal", Severity::Critical, &path, None, "The path could write outside the addon folder");
                unsafe_paths = true;
                continue;
            }
        };
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
            report.add("zip.symlink", Severity::Critical, &path, None, "Symbolic links are not allowed");
            continue;
        }
        if encrypted {
            report.add("zip.unreadable", Severity::Critical, &path, None, "The entry is encrypted and cannot be checked");
            continue;
        }
        if is_dir {
            continue;
        }
        let depth = path.trim_end_matches('/').split('/').count().saturating_sub(1);
        if depth > MAX_DEPTH && !deep_reported {
            report.add("zip.deep_nesting", Severity::Warn, &path, None, format!("More than {MAX_DEPTH} folder levels"));
            deep_reported = true;
        }

        let ext = extension(&path);
        let keep = ext.as_deref().is_some_and(|e| KEPT_EXTENSIONS.contains(&e));
        let mut entry = match archive.by_index(i) {
            Ok(entry) => entry,
            Err(e) => {
                report.add("zip.unreadable", Severity::Critical, &path, None, format!("The entry cannot be read: {e}"));
                continue;
            }
        };
        // Count what really comes out, never trust the sizes in the header.
        let mut head = Vec::with_capacity(512);
        let mut kept = Vec::new();
        let mut size: u64 = 0;
        let mut buf = [0u8; 64 * 1024];
        let mut too_big = false;
        loop {
            let n = match entry.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    report.add("zip.unreadable", Severity::Critical, &path, None, format!("The entry cannot be read: {e}"));
                    break;
                }
            };
            if head.len() < 512 {
                head.extend_from_slice(&buf[..n.min(512 - head.len())]);
            }
            size += n as u64;
            if size > MAX_ENTRY_BYTES || total + size > MAX_TOTAL_BYTES {
                too_big = true;
                break;
            }
            if keep {
                kept.extend_from_slice(&buf[..n]);
            }
        }
        total += size;
        if too_big {
            let message = if size > MAX_ENTRY_BYTES {
                format!("A single file larger than {} MB", MAX_ENTRY_BYTES / 1024 / 1024)
            } else {
                format!("The files add up to more than {} MB", MAX_TOTAL_BYTES / 1024 / 1024)
            };
            report.add("zip.bomb", Severity::Critical, &path, None, message);
            bomb = true;
            if total > MAX_TOTAL_BYTES {
                return texts;
            }
            continue;
        }
        if size >= RATIO_MIN_BYTES && size / compressed.max(1) > MAX_RATIO {
            // Only a notice: the limits above count what really comes out, and mostly empty
            // textures compress this well too.
            report.add("zip.bomb", Severity::Notice, &path, None, format!("Compressed more than {MAX_RATIO} to 1"));
        }
        if let Some((rule, what)) = sniff(&head) {
            report.add(rule, Severity::Critical, &path, None, format!("The file is {what}"));
            continue;
        }
        if is_hidden(&path) {
            report.add("zip.hidden", Severity::Info, &path, None, "System or version control file");
            continue;
        }
        let stem = path.rsplit('/').next().unwrap_or(&path).to_ascii_lowercase();
        let allowed = match &ext {
            Some(ext) => ALLOWED_EXTENSIONS.contains(&ext.as_str()),
            None => LICENSE_NAMES.contains(&stem.as_str()),
        };
        if !allowed {
            report.add("zip.extension", Severity::Notice, &path, None, "This file type does not belong in an addon");
        }
        if keep {
            texts.push(TextFile { path, bytes: kept });
        }
    }
    // After a bomb, nothing reads the raw zip again: the size limits apply only in the loop above.
    if !unsafe_paths && !bomb {
        check_folders(zip, expected_folders, foreign_folders, &texts, report);
    }
    texts
}

/// The addon folders in the zip against the folders the addon registered.
fn check_folders(zip: &[u8], expected: &[String], foreign: &[String], texts: &[TextFile], report: &mut Report) {
    let detected = match analyze_reader(Cursor::new(zip)) {
        Ok(detected) => detected,
        Err(forge_core::Error::TocTooLarge(path)) => {
            report.add("zip.bomb", Severity::Critical, &path, None, format!("The .toc is larger than {} KB", MAX_TOC_BYTES / 1024));
            return;
        }
        Err(_) => Vec::new(),
    };
    // Folders compare the way Windows opens them: "Bagnon." is the folder Bagnon.
    for addon in &detected {
        if !windows_safe_folder(&addon.name) {
            report.add("zip.invalid_folder", Severity::Critical, &addon.prefix, None,
                format!("The folder name {} is read differently on Windows and could stand in for another folder", addon.name));
        }
        if foreign.iter().any(|f| folder_key(f) == folder_key(&addon.name)) {
            report.add("zip.foreign_folder", Severity::Critical, &addon.prefix, None,
                format!("The folder {} belongs to another addon and would overwrite it", addon.name));
        } else if !expected.iter().any(|e| folder_key(e) == folder_key(&addon.name)) {
            report.add("zip.undeclared_folder", Severity::Critical, &addon.prefix, None,
                format!("The folder {} is not registered for this addon", addon.name));
        }
        if addon.toc.interface.as_deref().map(str::trim) != Some(forge_core::toc::INTERFACE) {
            report.add("zip.toc_interface", Severity::Info, &addon.prefix, None,
                format!("The .toc is not for interface {}", forge_core::toc::INTERFACE));
        }
        check_toc_files(addon, texts, report);
    }
    for folder in expected {
        if !detected.iter().any(|a| folder_key(&a.name) == folder_key(folder)) {
            report.add("zip.missing_folder", Severity::Warn, "", None, format!("The registered folder {folder} is not in the zip"));
        }
    }
}

/// The lowercase paths the game loads: every file a `.toc` lists, then every file a loaded XML pulls
/// in with `<Script file>` or `<Include file>`. `None` when no addon with a `.toc` is found; then
/// everything counts as loaded.
pub fn loaded_files(zip: &[u8], texts: &[TextFile]) -> Option<HashSet<String>> {
    let detected = analyze_reader(Cursor::new(zip)).ok()?;
    let by_path: HashMap<String, &TextFile> = texts.iter().map(|t| (t.path.to_ascii_lowercase(), t)).collect();
    let mut loaded = HashSet::new();
    let mut queue = Vec::new();
    for addon in &detected {
        let dir = if addon.prefix.is_empty() { String::new() } else { format!("{}/", addon.prefix) };
        let Some(toc) = by_path.get(&format!("{dir}{}.toc", addon.name).to_ascii_lowercase()) else { continue };
        for line in String::from_utf8_lossy(&toc.bytes).lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if !line.is_empty() && !line.starts_with('#') {
                queue.extend(resolve(&dir, line));
            }
        }
    }
    if queue.is_empty() {
        return None;
    }
    while let Some(path) = queue.pop() {
        if !loaded.insert(path.clone()) {
            continue;
        }
        if let Some(xml) = by_path.get(&path).filter(|_| path.ends_with(".xml")) {
            queue.extend(crate::xml_rules::referenced_files(&xml.path, &xml.bytes));
        }
    }
    Some(loaded)
}

/// Every file the `.toc` loads exists in the addon folder.
fn check_toc_files(addon: &forge_core::archive::DetectedAddon, texts: &[TextFile], report: &mut Report) {
    let dir = if addon.prefix.is_empty() { String::new() } else { format!("{}/", addon.prefix) };
    // Whole paths compared, never cut at a byte count: a name from the zip may have a multibyte
    // character exactly there.
    let wanted = format!("{dir}{}.toc", addon.name);
    let toc = texts.iter().find(|t| t.path.eq_ignore_ascii_case(&wanted));
    let Some(toc) = toc else { return };
    let all: Vec<String> = texts.iter().map(|t| t.path.to_ascii_lowercase()).collect();
    for (n, line) in String::from_utf8_lossy(&toc.bytes).lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A file of the game itself is not in the zip.
        let Some(wanted) = resolve(&dir, line) else { continue };
        if !all.contains(&wanted) {
            report.add_with_snippet("zip.toc_missing_file", Severity::Warn, &toc.path, Some(n as u32 + 1),
                "The .toc loads a file that is not in the zip", line);
        }
    }
}
