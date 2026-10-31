//! Updates Azeroth Survivors itself, the way the game does: from the indexes on its server
//! (`exe/latest.json` for the Windows exe, `exe/linux/latest.json` for the native Linux build,
//! `exe/data/patches.json` for the files in `Data`, shared by both). Each file there is a gzip cut
//! into parts, with the size and SHA-256 of the unpacked file.

use crate::{Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Where the game's indexes and parts live; part URLs are relative to it.
pub const GAME_SERVER: &str = "https://wow.aethus.pl/exe/";
pub const GAME_EXE: &str = "AzerothSurvivors.exe";

/// Which build of the game a folder runs, and where the server keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// `AzerothSurvivors.exe`: on Windows, and on Linux through Proton.
    Windows,
    /// The native Linux build (`AzerothSurvivors`, no extension).
    Linux,
}

impl Build {
    /// The folder of its index and parts, relative to `GAME_SERVER`.
    pub fn base(self) -> &'static str {
        match self {
            Build::Windows => "",
            Build::Linux => "linux/",
        }
    }
}

/// The game's exe in `game_dir` and its build. On Linux a native build comes first; without one,
/// the Windows exe that Proton runs.
pub fn find_exe(game_dir: &Path, linux: bool) -> Option<(PathBuf, Build)> {
    let windows = game_dir.join(GAME_EXE);
    let native = game_dir.join("AzerothSurvivors");
    if linux && native.is_file() {
        return Some((native, Build::Linux));
    }
    windows.is_file().then_some((windows, Build::Windows))
}
/// The game writes its version right before this text in its exe ("0.133.0A new version is out!").
const VERSION_MARK: &[u8] = b"A new version is out!";

/// Reads a file of the game server, by its path relative to `GAME_SERVER`, telling `progress` the
/// bytes of each piece as it arrives.
pub trait GameServer {
    fn get(&self, path: &str, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>>;
}

/// The real server, through the launcher's HTTP client.
pub struct WebServer(pub crate::github::Client);

impl GameServer for WebServer {
    fn get(&self, path: &str, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>> {
        // A part is 20 MiB; the indexes are small.
        self.0.get_file_with(&format!("{GAME_SERVER}{path}"), 64 * 1024 * 1024, progress)
    }
}

/// Where an update stands, for the launcher to show.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Bytes of the whole plan so far, and of all of it.
    Downloading { done: u64, total: u64 },
    /// A file is unpacked and compared with the server's size and SHA-256.
    Checking,
    /// A checked file goes in place of the old one.
    Installing,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Part {
    pub url: String,
    pub size: u64,
}

/// One file of an index: its name (the exe has none in `latest.json`), size, SHA-256 and parts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GameFile {
    #[serde(default)]
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Latest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(flatten)]
    pub exe: GameFile,
}

#[derive(Deserialize)]
struct Patches {
    patches: Vec<GameFile>,
}

impl GameFile {
    /// Refuses what the server should never list: names with a path, parts outside the server's
    /// folder, a hash that is no SHA-256.
    fn check(&self, base: &str) -> Result<()> {
        let odd = |what: &str| Err(Error::Parse(format!("the game's index lists an odd {what}")));
        if self.name.contains(['/', '\\']) || self.name.contains("..") {
            return odd(&format!("file: {}", self.name));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) || self.size == 0 || self.parts.is_empty() {
            return odd(&format!("entry: {}", self.name));
        }
        for part in &self.parts {
            let url = &part.url;
            if url.is_empty() || url.starts_with('/') || url.contains("..") || url.contains([':', '\\', '?', '#']) {
                return odd(&format!("part: {base}{url}"));
            }
        }
        Ok(())
    }
}

pub fn parse_latest(text: &str) -> Result<Latest> {
    let latest: Latest = serde_json::from_str(text).map_err(|e| Error::Parse(format!("latest.json: {e}")))?;
    latest.exe.check("")?;
    Ok(latest)
}

pub fn parse_patches(text: &str) -> Result<Vec<GameFile>> {
    let patches: Patches = serde_json::from_str(text).map_err(|e| Error::Parse(format!("patches.json: {e}")))?;
    for patch in &patches.patches {
        if patch.name.is_empty() {
            return Err(Error::Parse("patches.json lists a file without a name".into()));
        }
        patch.check("data/")?;
    }
    Ok(patches.patches)
}

/// SHA-256 of an exe, and the version it says it is (`None` when it does not), in one read.
pub fn read_exe(path: &Path) -> std::io::Result<(String, Option<String>)> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut version = None;
    let mut buffer = vec![0u8; 1 << 20];
    // The end of the last read, so a mark across two reads is found.
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        if version.is_none() {
            let mut window = std::mem::take(&mut tail);
            window.extend_from_slice(&buffer[..read]);
            version = version_in(&window);
            let keep = window.len().min(VERSION_MARK.len() + 24);
            tail = window[window.len() - keep..].to_vec();
        }
    }
    Ok((crate::install::hex(&hash.finalize()), version))
}

fn version_in(data: &[u8]) -> Option<String> {
    let at = data.windows(VERSION_MARK.len()).position(|w| w == VERSION_MARK)?;
    let before = &data[at.saturating_sub(24)..at];
    let text: String = before.iter().rev().take_while(|b| b.is_ascii_digit() || **b == b'.').map(|&b| b as char).collect();
    let version: String = text.chars().rev().collect::<String>().trim_start_matches('.').to_string();
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() == 3 && parts.iter().all(|p| !p.is_empty())).then_some(version)
}

/// What the launcher last read of the exe, so it is hashed again only once it changed.
#[derive(Debug, Clone, PartialEq)]
pub struct ExeSeen {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub sha256: String,
    pub version: Option<String>,
}

/// The exe at `path` as `known` saw it when its size and time are unchanged, else read again;
/// `None` without one.
pub fn look_at_exe(path: &Path, known: Option<&ExeSeen>) -> Option<ExeSeen> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let modified = meta.modified().ok();
    if let Some(known) = known.filter(|k| k.path == path && k.size == meta.len() && k.modified == modified) {
        return Some(known.clone());
    }
    let (sha256, version) = read_exe(path).ok()?;
    Some(ExeSeen { path: path.to_path_buf(), size: meta.len(), modified, sha256, version })
}

/// Asks the server what is newest for `build` and compares it with the game in `game_dir`.
pub fn check(server: &dyn GameServer, game_dir: &Path, build: Build, exe: Option<&ExeSeen>) -> Result<Plan> {
    let text = |path: &str| -> Result<String> { String::from_utf8(server.get(path, &mut |_| {})?).map_err(|e| Error::Parse(format!("{path}: {e}"))) };
    let latest = parse_latest(&text(&format!("{}latest.json", build.base()))?)?;
    let patches = parse_patches(&text("data/patches.json")?)?;
    Ok(plan(latest, patches, exe.map(|e| (e, build)), &game_dir.join("Data")))
}

/// What an update would bring: the newest exe when the installed one differs, and the data patches
/// that are missing or of another size.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub version: String,
    pub notes: String,
    /// The newest exe, its build and the file it replaces.
    pub exe: Option<(GameFile, Build, PathBuf)>,
    pub patches: Vec<GameFile>,
}

impl Plan {
    pub fn behind(&self) -> bool {
        self.exe.is_some() || !self.patches.is_empty()
    }

    /// Bytes to download.
    pub fn download_size(&self) -> u64 {
        self.exe.iter().map(|(f, _, _)| f).chain(&self.patches).flat_map(|f| &f.parts).map(|p| p.size).sum()
    }
}

/// `exe` is the game's exe as seen, with its build; without one only the data patches are looked at.
pub fn plan(latest: Latest, patches: Vec<GameFile>, exe: Option<(&ExeSeen, Build)>, data_dir: &Path) -> Plan {
    let exe = exe
        .filter(|(seen, _)| !seen.sha256.eq_ignore_ascii_case(&latest.exe.sha256))
        .map(|(seen, build)| (latest.exe.clone(), build, seen.path.clone()));
    let patches = patches
        .into_iter()
        .filter(|p| std::fs::metadata(data_dir.join(&p.name)).map(|m| m.len() != p.size).unwrap_or(true))
        .collect();
    Plan { version: latest.version, notes: latest.notes, exe, patches }
}

/// Downloads `file` from `base` (relative to the server), unpacks it next to `target`, checks its
/// size and SHA-256 and only then puts it in place of `target`. `downloaded` gets the bytes as they
/// arrive, `step` the checking and installing.
pub fn install(
    server: &dyn GameServer,
    base: &str,
    file: &GameFile,
    target: &Path,
    downloaded: &mut dyn FnMut(u64),
    step: &mut dyn FnMut(Step),
) -> Result<()> {
    let fresh = sibling(target, "survivorsforge-new");
    let result = (|| -> Result<()> {
        let mut packed = Vec::new();
        for part in &file.parts {
            packed.extend_from_slice(&server.get(&format!("{base}{}", part.url), downloaded)?);
        }
        step(Step::Checking);
        let mut output = std::fs::File::create(&fresh)?;
        let mut hash = Sha256::new();
        let mut unpack = flate2::read::GzDecoder::new(&packed[..]);
        let mut buffer = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let read = unpack.read(&mut buffer).map_err(|e| Error::Zip(format!("{}: {e}", label(file))))?;
            if read == 0 {
                break;
            }
            size += read as u64;
            if size > file.size {
                break;
            }
            hash.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        drop(output);
        step(Step::Installing);
        // The old file's permissions: the native Linux build stays executable.
        if let Ok(meta) = std::fs::metadata(target) {
            std::fs::set_permissions(&fresh, meta.permissions())?;
        }
        let actual = crate::install::hex(&hash.finalize());
        if size != file.size || !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(Error::HashMismatch { expected: file.sha256.clone(), actual });
        }
        std::fs::rename(&fresh, target).map_err(|e| Error::Io(format!("{} cannot be replaced (is the game still running?): {e}", target.display())))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&fresh);
    }
    result
}

fn label(file: &GameFile) -> &str {
    if file.name.is_empty() { GAME_EXE } else { &file.name }
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    target.with_file_name(name)
}

/// Brings the game in `game_dir` to `plan`: the exe, then each data patch. `step` hears where it stands.
pub fn update(server: &dyn GameServer, game_dir: &Path, plan: &Plan, step: &mut dyn FnMut(Step)) -> Result<()> {
    let total = plan.download_size();
    let mut done = 0u64;
    // Two borrows of `step` in one call would not do: downloads report through this cell.
    let step = std::cell::RefCell::new(step);
    let mut downloaded = |n: u64| {
        done += n;
        (step.borrow_mut())(Step::Downloading { done, total });
    };
    let mut steps = |s: Step| (step.borrow_mut())(s);
    if let Some((exe, build, target)) = &plan.exe {
        install(server, build.base(), exe, target, &mut downloaded, &mut steps)?;
    }
    let data = game_dir.join("Data");
    std::fs::create_dir_all(&data)?;
    for patch in &plan.patches {
        install(server, "data/", patch, &data.join(&patch.name), &mut downloaded, &mut steps)?;
    }
    Ok(())
}

/// What the launch button says for `step` (short: the button is narrow), and how full its bar is.
pub fn step_label(step: &Step) -> (String, f32) {
    match step {
        Step::Downloading { done, total } => (
            format!("{} / {} MB", done / 1_048_576, total.div_ceil(1_048_576)),
            if *total == 0 { 0.0 } else { (*done as f64 / *total as f64).min(1.0) as f32 },
        ),
        Step::Checking => ("Checking".into(), 1.0),
        Step::Installing => ("Installing".into(), 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A server of files in memory; it records what was asked for.
    #[derive(Default)]
    struct Fake {
        files: HashMap<String, Vec<u8>>,
        asked: RefCell<Vec<String>>,
    }

    impl GameServer for Fake {
        fn get(&self, path: &str, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>> {
            self.asked.borrow_mut().push(path.to_string());
            let bytes = self.files.get(path).cloned().ok_or(Error::NotFound)?;
            // In two pieces, as a network read would bring them.
            progress(bytes.len() as u64 / 2);
            progress(bytes.len() as u64 - bytes.len() as u64 / 2);
            Ok(bytes)
        }
    }

    fn sha(bytes: &[u8]) -> String {
        crate::install::hex(&Sha256::digest(bytes))
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// `content` as a gzip in two parts under `dir/` on `server`, and its index entry.
    fn publish(server: &mut Fake, base: &str, dir: &str, name: &str, content: &[u8]) -> GameFile {
        let packed = gzip(content);
        let (a, b) = packed.split_at(packed.len() / 2);
        let mut parts = Vec::new();
        for (i, chunk) in [a, b].iter().enumerate() {
            let url = format!("{dir}/part-00{i}.bin");
            server.files.insert(format!("{base}{url}"), chunk.to_vec());
            parts.push(Part { url, size: chunk.len() as u64 });
        }
        GameFile { name: name.into(), size: content.len() as u64, sha256: sha(content), parts }
    }

    #[test]
    fn reads_the_indexes_and_refuses_odd_entries() {
        let latest = parse_latest(&format!(
            r#"{{"version": "0.133.0", "notes": "New", "size": 3, "sha256": "{}", "parts": [{{"url": "0.133.0/part-000.bin", "size": 2}}]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        assert_eq!((latest.version.as_str(), latest.notes.as_str(), latest.exe.size, latest.exe.parts.len()), ("0.133.0", "New", 3, 1));

        let entry = |name: &str, url: &str, sha: &str| {
            format!(r#"{{"patches": [{{"name": "{name}", "size": 3, "sha256": "{sha}", "parts": [{{"url": "{url}", "size": 2}}]}}]}}"#)
        };
        let good = "b".repeat(64);
        assert_eq!(parse_patches(&entry("patch-2.MPQ", "patch-2/part-000.bin", &good)).unwrap()[0].name, "patch-2.MPQ");
        for (name, url, sha) in [
            ("../evil.dll", "p/part-000.bin", good.as_str()),
            ("x\\\\y.MPQ", "p/part-000.bin", good.as_str()),
            ("p.MPQ", "../../other/part.bin", good.as_str()),
            ("p.MPQ", "https://evil.test/part.bin", good.as_str()),
            ("p.MPQ", "/root/part.bin", good.as_str()),
            ("p.MPQ", "p/part-000.bin", "not-a-hash"),
            ("", "p/part-000.bin", good.as_str()),
        ] {
            assert!(parse_patches(&entry(name, url, sha)).is_err(), "{name} {url} {sha}");
        }
        assert!(parse_latest(r#"{"version": "1"}"#).is_err());
    }

    #[test]
    fn finds_the_version_the_exe_names_also_across_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(GAME_EXE);
        // The mark right at the end of the first 1 MiB read.
        let mut bytes = vec![b'x'; (1 << 20) - 10];
        bytes.extend_from_slice(b"..0.133.0A new version is out!..");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_exe(&path).unwrap(), (sha(&bytes), Some("0.133.0".into())));
        std::fs::write(&path, b"no version here").unwrap();
        assert_eq!(read_exe(&path).unwrap().1, None);
    }

    #[test]
    fn plans_only_what_differs() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("patch-2.MPQ"), b"abc").unwrap();
        std::fs::write(data.join("patch-3.MPQ"), b"old").unwrap();
        let file = |name: &str, size: u64| GameFile { name: name.into(), size, sha256: "c".repeat(64), parts: vec![Part { url: "p".into(), size: 1 }] };
        let latest = Latest { version: "0.133.0".into(), notes: String::new(), exe: file("", 9) };
        let patches = vec![file("patch-2.MPQ", 3), file("patch-3.MPQ", 5), file("patch-4.MPQ", 1)];
        let seen = |sha: &str| ExeSeen { path: dir.path().join("AzerothSurvivors"), size: 1, modified: None, sha256: sha.into(), version: None };

        let plan_for = |exe: Option<ExeSeen>| plan(latest.clone(), patches.clone(), exe.as_ref().map(|e| (e, Build::Linux)), &data);
        let names = |p: &Plan| p.patches.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        let behind = plan_for(Some(seen(&"d".repeat(64))));
        let (_, build, target) = behind.exe.clone().unwrap();
        assert_eq!((build, target), (Build::Linux, dir.path().join("AzerothSurvivors")), "replaces the exe that is there");
        assert_eq!(names(&behind), ["patch-3.MPQ", "patch-4.MPQ"], "a size that differs, a file that is missing");
        assert_eq!(behind.download_size(), 3);
        assert!(plan_for(Some(seen(&"C".repeat(64)))).exe.is_none(), "the same exe, in any case");
        assert!(plan_for(None).exe.is_none(), "no exe in the folder: only the data");
    }

    #[test]
    fn check_asks_both_indexes_and_hashes_the_exe_once() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(GAME_EXE);
        std::fs::write(&exe, b"0.131.0A new version is out!").unwrap();
        let mut server = Fake::default();
        let newest = publish(&mut server, "", "0.133.0", "", b"0.133.0A new version is out!");
        let latest = serde_json::json!({ "version": "0.133.0", "notes": "Better", "size": newest.size, "sha256": newest.sha256,
                                          "parts": newest.parts.iter().map(|p| serde_json::json!({ "url": p.url, "size": p.size })).collect::<Vec<_>>() });
        server.files.insert("latest.json".into(), latest.to_string().into_bytes());
        server.files.insert("data/patches.json".into(), br#"{"patches": []}"#.to_vec());

        let seen = look_at_exe(&exe, None).unwrap();
        assert_eq!(seen.version.as_deref(), Some("0.131.0"));
        let plan = check(&server, dir.path(), Build::Windows, Some(&seen)).unwrap();
        assert_eq!((plan.version.as_str(), plan.notes.as_str(), plan.exe.is_some()), ("0.133.0", "Better", true));
        assert!(check(&server, dir.path(), Build::Linux, Some(&seen)).is_err(), "the Linux build has its own index");

        // Unchanged on disk: the stored hash stands, even a made-up one.
        let pretend = ExeSeen { sha256: newest.sha256.clone(), ..seen.clone() };
        assert_eq!(look_at_exe(&exe, Some(&pretend)), Some(pretend.clone()));
        assert!(!check(&server, dir.path(), Build::Windows, Some(&pretend)).unwrap().behind());
        assert_eq!(look_at_exe(&dir.path().join("missing.exe"), None), None);
    }

    #[test]
    fn update_puts_checked_files_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(GAME_EXE), b"old exe").unwrap();
        let mut server = Fake::default();
        let exe = publish(&mut server, "", "0.133.0", "", b"new exe, a bit longer");
        let patch = publish(&mut server, "data/", "patch-4", "patch-4.MPQ", b"patch four");
        let plan = Plan { version: "0.133.0".into(), notes: String::new(), exe: Some((exe, Build::Windows, dir.path().join(GAME_EXE))), patches: vec![patch] };
        let mut steps = Vec::new();
        update(&server, dir.path(), &plan, &mut |s| steps.push(s)).unwrap();
        assert_eq!(std::fs::read(dir.path().join(GAME_EXE)).unwrap(), b"new exe, a bit longer");
        assert_eq!(std::fs::read(dir.path().join("Data").join("patch-4.MPQ")).unwrap(), b"patch four");
        let total = plan.download_size();
        let downloads: Vec<u64> = steps.iter().filter_map(|s| match s { Step::Downloading { done, total: t } => { assert_eq!(*t, total); Some(*done) } _ => None }).collect();
        assert!(downloads.windows(2).all(|w| w[0] <= w[1]) && downloads.last() == Some(&total), "{downloads:?}");
        let named: Vec<&Step> = steps.iter().filter(|s| !matches!(s, Step::Downloading { .. })).collect();
        assert_eq!(named, [&Step::Checking, &Step::Installing, &Step::Checking, &Step::Installing], "per file: the exe, then the patch");
        assert_eq!(step_label(&Step::Downloading { done: 1_048_576, total: 4_194_304 }), ("1 / 4 MB".to_string(), 0.25));
        assert_eq!(server.asked.borrow().as_slice(), ["0.133.0/part-000.bin", "0.133.0/part-001.bin", "data/patch-4/part-000.bin", "data/patch-4/part-001.bin"]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no leftovers: the exe and Data");
    }

    #[test]
    fn the_linux_build_comes_from_its_own_folder_and_stays_executable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_exe(dir.path(), true), None);
        std::fs::write(dir.path().join(GAME_EXE), b"windows").unwrap();
        assert_eq!(find_exe(dir.path(), true), Some((dir.path().join(GAME_EXE), Build::Windows)), "Proton runs the Windows exe");
        let native = dir.path().join("AzerothSurvivors");
        std::fs::write(&native, b"old linux build").unwrap();
        assert_eq!(find_exe(dir.path(), true), Some((native.clone(), Build::Linux)), "a native build first");
        assert_eq!(find_exe(dir.path(), false).map(|(_, b)| b), Some(Build::Windows), "Windows never runs the native one");

        #[cfg(unix)]
        std::fs::set_permissions(&native, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut server = Fake::default();
        let file = publish(&mut server, "linux/", "0.133.0", "", b"new linux build");
        let plan = Plan { version: "0.133.0".into(), notes: String::new(), exe: Some((file, Build::Linux, native.clone())), patches: Vec::new() };
        update(&server, dir.path(), &plan, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), b"new linux build");
        assert_eq!(server.asked.borrow()[0], "linux/0.133.0/part-000.bin");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&native).unwrap().permissions()) & 0o777, 0o755);
    }

    #[test]
    fn a_wrong_or_missing_file_leaves_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(GAME_EXE);
        std::fs::write(&target, b"old exe").unwrap();
        let mut server = Fake::default();
        let mut exe = publish(&mut server, "", "0.133.0", "", b"new exe");
        exe.sha256 = "e".repeat(64);
        assert!(matches!(install(&server, "", &exe, &target, &mut |_| {}, &mut |_| {}), Err(Error::HashMismatch { .. })));
        exe.parts[1].url = "gone/part.bin".into();
        assert_eq!(install(&server, "", &exe, &target, &mut |_| {}, &mut |_| {}), Err(Error::NotFound));
        assert_eq!(std::fs::read(&target).unwrap(), b"old exe");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "nothing left behind");
    }
}
