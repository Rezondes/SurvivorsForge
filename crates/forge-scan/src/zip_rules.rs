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

/// What a path in a `.toc` or XML file opens, seen from the game folder.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    /// A file of this zip, as its lowercase zip path.
    InZip(String),
    /// A file of the game itself, like `..\..\FrameXML\Fonts.xml`.
    GameFile,
    /// A file in the folder of another addon, like a plugin's `..\Titan\...`: that addon is
    /// scanned on its own.
    OtherAddon(String),
    /// Above the game folder, or no file at all: nobody can say what the client opens.
    Unmappable,
}

/// Where the client finds the files of a zip: each addon installs into `Interface\AddOns\<name>`,
/// whatever its folder in the zip is called (GitHub source zips use `Repo-main/`).
pub struct Layout {
    /// Lowercase install name and folder in the zip, from the `.toc` files.
    addons: Vec<(String, String)>,
}

impl Layout {
    /// The addons of a zip, from the paths of its `.toc` files: `X/X.toc` installs as `X`, a
    /// `.toc` in a folder of another name as its own name.
    pub fn from_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Layout {
        let mut addons = Vec::new();
        for path in paths {
            let path = normal_path(path);
            let Some(stem) = path.strip_suffix(".toc") else { continue };
            let (dir, stem) = stem.rsplit_once('/').unwrap_or(("", stem));
            let folder = dir.rsplit('/').next().unwrap_or_default();
            let name = if folder == stem { folder } else { stem };
            addons.push((name.to_string(), dir.to_string()));
        }
        // The client reads only the .toc of a top-level folder: a library inside an addon, with
        // its own .toc, is part of that addon and installs with it.
        let inside = |dir: &str, other: &str| other != dir && (other.is_empty() || dir.starts_with(&format!("{other}/")));
        // In a folder with its own <Folder>.toc, other .toc files (V_TBC.toc) are for other clients.
        let own = |dir: &str| addons.iter().any(|(name, d)| d == dir && !dir.is_empty() && dir.rsplit('/').next() == Some(name.as_str()));
        let top: Vec<(String, String)> = addons
            .iter()
            .filter(|(_, dir)| !addons.iter().any(|(_, other)| inside(dir, other)))
            .filter(|(name, dir)| !own(dir) || dir.rsplit('/').next() == Some(name.as_str()))
            .cloned()
            .collect();
        Layout { addons: top }
    }

    /// The zip path of each top-level `.toc`, lowercase.
    pub fn tocs(&self) -> impl Iterator<Item = String> + '_ {
        self.addons.iter().map(|(name, dir)| if dir.is_empty() { format!("{name}.toc") } else { format!("{dir}/{name}.toc") })
    }

    /// What `path`, written in a file of the zip folder `from_dir`, opens. The client resolves it
    /// from the addon's install folder, `..` included, or takes `Interface\AddOns\...` as given.
    pub fn resolve(&self, from_dir: &str, path: &str) -> Resolved {
        let from_dir = normal_path(from_dir);
        let given = normal_path(path);
        let full = if given.starts_with("interface/addons/") {
            given
        } else {
            let owner = self.addons.iter().find(|(_, dir)| dir.is_empty() || from_dir == *dir || from_dir.starts_with(&format!("{dir}/")));
            let Some((name, dir)) = owner else {
                // No addon around it: the zip is all there is.
                let inside = normal_path(&format!("{from_dir}/{path}"));
                return if inside.is_empty() || inside.starts_with("..") { Resolved::Unmappable } else { Resolved::InZip(inside) };
            };
            let below = from_dir.strip_prefix(dir.as_str()).unwrap_or_default();
            normal_path(&format!("interface/addons/{name}/{below}/{path}"))
        };
        if full.is_empty() || full.starts_with("..") {
            return Resolved::Unmappable;
        }
        let Some(rest) = full.strip_prefix("interface/addons/") else { return Resolved::GameFile };
        let (folder, file) = rest.split_once('/').unwrap_or((rest, ""));
        if file.is_empty() {
            return Resolved::Unmappable;
        }
        // Blizzard_ folders are addons of the game itself, like Blizzard_AuctionUI.
        if folder.starts_with("blizzard_") {
            return Resolved::GameFile;
        }
        match self.addons.iter().find(|(name, _)| folder_key(name) == folder_key(folder)) {
            Some((_, dir)) if dir.is_empty() => Resolved::InZip(file.to_string()),
            Some((_, dir)) => Resolved::InZip(format!("{dir}/{file}")),
            None => Resolved::OtherAddon(folder.to_string()),
        }
    }
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
    let layout = Layout::from_paths(texts.iter().map(|t| t.path.as_str()));
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
        check_toc_files(addon, texts, &layout, report);
    }
    for folder in expected {
        if !detected.iter().any(|a| folder_key(&a.name) == folder_key(folder)) {
            report.add("zip.missing_folder", Severity::Warn, "", None, format!("The registered folder {folder} is not in the zip"));
        }
    }
}

/// The lines of a `.toc` that name files.
fn toc_lines(toc: &TextFile) -> Vec<(usize, String)> {
    let text = String::from_utf8_lossy(&toc.bytes);
    text.lines()
        .enumerate()
        .map(|(n, line)| (n, line.trim().trim_start_matches('\u{feff}').to_string()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The folder of a zip path, without the file.
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The lowercase paths the game loads: every file a `.toc` lists, then every file a loaded XML pulls
/// in with `<Script file>` or `<Include file>`. `None` when no addon with a `.toc` is found, or when
/// a path cannot be mapped: then everything counts as loaded, so nothing is played down.
pub fn loaded_files(texts: &[TextFile]) -> Option<HashSet<String>> {
    let layout = Layout::from_paths(texts.iter().map(|t| t.path.as_str()));
    let by_path: HashMap<String, &TextFile> = texts.iter().map(|t| (t.path.to_ascii_lowercase(), t)).collect();
    let mut loaded = HashSet::new();
    let mut queue = Vec::new();
    for toc in layout.tocs().filter_map(|path| by_path.get(&path).copied()) {
        for (_, line) in toc_lines(toc) {
            queue.push(layout.resolve(dir_of(&toc.path), &line));
        }
    }
    if queue.is_empty() {
        return None;
    }
    while let Some(resolved) = queue.pop() {
        let path = match resolved {
            Resolved::InZip(path) => path,
            Resolved::GameFile | Resolved::OtherAddon(_) => continue,
            Resolved::Unmappable => return None,
        };
        if !loaded.insert(path.clone()) {
            continue;
        }
        if let Some(xml) = by_path.get(&path).filter(|_| path.ends_with(".xml")) {
            for file in crate::xml_rules::referenced_files(&xml.bytes) {
                queue.push(layout.resolve(dir_of(&xml.path), &file));
            }
        }
    }
    Some(loaded)
}

/// Every file the `.toc` loads exists in the addon folder.
fn check_toc_files(addon: &forge_core::archive::DetectedAddon, texts: &[TextFile], layout: &Layout, report: &mut Report) {
    let dir = if addon.prefix.is_empty() { String::new() } else { format!("{}/", addon.prefix) };
    // Whole paths compared, never cut at a byte count: a name from the zip may have a multibyte
    // character exactly there.
    let wanted = format!("{dir}{}.toc", addon.name);
    let toc = texts.iter().find(|t| t.path.eq_ignore_ascii_case(&wanted));
    let Some(toc) = toc else { return };
    let all: Vec<String> = texts.iter().map(|t| t.path.to_ascii_lowercase()).collect();
    for (n, line) in toc_lines(toc) {
        let line_no = Some(n as u32 + 1);
        match layout.resolve(dir_of(&toc.path), &line) {
            Resolved::InZip(wanted) if !all.contains(&wanted) => {
                report.add_with_snippet("zip.toc_missing_file", Severity::Warn, &toc.path, line_no, "The .toc loads a file that is not in the zip", &line);
            }
            Resolved::InZip(_) | Resolved::GameFile => {}
            Resolved::OtherAddon(folder) => {
                let message = format!("The .toc loads a file of the addon folder {folder}");
                report.add_with_snippet("zip.unresolved_path", Severity::Notice, &toc.path, line_no, message, &line);
            }
            Resolved::Unmappable => {
                report.add_with_snippet("zip.unresolved_path", Severity::Warn, &toc.path, line_no, "The .toc loads a path outside the game's addon folders", &line);
            }
        }
    }
}
