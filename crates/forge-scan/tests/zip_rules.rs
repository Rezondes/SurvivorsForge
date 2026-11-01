mod common;

use common::{clean_addon, folders, zip};
use forge_scan::{Report, Severity, scan};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;

fn lucky(report_of: &[u8]) -> Report {
    scan(report_of, &folders(&["Lucky"]), &folders(&["LuckyOdds"]))
}

fn with(extra: &[(&'static str, &'static [u8])]) -> Vec<u8> {
    let mut entries = clean_addon();
    entries.extend_from_slice(extra);
    zip(&entries)
}

/// The single finding of `rule`, asserting its severity and file.
fn finding<'a>(report: &'a Report, rule: &str) -> &'a forge_scan::Finding {
    let hits: Vec<_> = report.findings.iter().filter(|f| f.rule == rule).collect();
    assert!(!hits.is_empty(), "no {rule} in {:#?}", report.findings);
    hits[0]
}

#[test]
fn clean_addon_has_no_warnings() {
    let report = lucky(&zip(&clean_addon()));
    assert!(!report.flagged(), "{:#?}", report.findings);
}

#[test]
fn path_traversal_is_critical() {
    for bad in ["../evil.lua", "/abs.lua", "C:/win.lua", "Lucky\\..\\x.lua", "\\abs.lua", "Lucky\\C:\\x.lua"] {
        let report = lucky(&with(&[(bad, b"x")]));
        let f = finding(&report, "zip.path_traversal");
        assert_eq!((f.severity, f.file.as_str()), (Severity::Critical, bad));
    }
}

#[test]
fn windows_separators_are_paths_not_attacks() {
    // Zips packed with Windows tools: the same addon, `\` as separator.
    let report = lucky(&zip(&[
        ("Lucky\\Lucky.toc", b"## Interface: 11200\nLucky.lua\n"),
        ("Lucky\\Lucky.lua", b"local f = CreateFrame(\"Frame\")\n"),
        ("Lucky\\media\\icon.tga", b"\x00\x00\x02\x00"),
    ]));
    assert!(report.findings.iter().all(|f| f.rule != "zip.path_traversal"), "{:#?}", report.findings);
    assert!(report.findings.iter().all(|f| f.rule != "zip.toc_missing_file"), "the .toc finds its files: {:#?}", report.findings);
    assert!(!report.flagged(), "{:#?}", report.findings);
}

#[test]
fn symlink_is_critical() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("Lucky/Lucky.toc", SimpleFileOptions::default()).unwrap();
    writer.write_all(b"## Interface: 11200\n").unwrap();
    writer.add_symlink("Lucky/link", "/etc/passwd", SimpleFileOptions::default()).unwrap();
    let report = lucky(&writer.finish().unwrap().into_inner());
    assert_eq!(finding(&report, "zip.symlink").severity, Severity::Critical);
}

#[test]
fn renamed_exe_is_critical() {
    let report = lucky(&with(&[("Lucky/Textures/bg.tga", b"MZ\x90\x00\x03\x00\x00\x00")]));
    let f = finding(&report, "zip.executable");
    assert_eq!((f.severity, f.file.as_str()), (Severity::Critical, "Lucky/Textures/bg.tga"));
    for magic in [&b"\x7fELF\x02"[..], b"\xcf\xfa\xed\xfe", b"#!/bin/sh\n"] {
        let report = lucky(&with(&[("Lucky/x.lua", Box::leak(magic.to_vec().into_boxed_slice()))]));
        assert_eq!(finding(&report, "zip.executable").severity, Severity::Critical);
    }
}

#[test]
fn nested_zip_is_critical() {
    let inner: &'static [u8] = Box::leak(zip(&[("a.txt", b"x")]).into_boxed_slice());
    let report = lucky(&with(&[("Lucky/data.tga", inner)]));
    assert_eq!(finding(&report, "zip.nested_archive").severity, Severity::Critical);
    let report = lucky(&with(&[("Lucky/data.blp", b"\x1f\x8b\x08\x00")]));
    assert_eq!(finding(&report, "zip.nested_archive").severity, Severity::Critical);
}

#[test]
fn high_ratio_is_only_a_notice() {
    // The size limits count the bytes that come out; a mostly empty texture compresses this well too.
    let zeros: &'static [u8] = Box::leak(vec![0u8; 2 * 1024 * 1024].into_boxed_slice());
    let report = lucky(&with(&[("Lucky/padding.txt", zeros)]));
    assert_eq!(finding(&report, "zip.bomb").severity, Severity::Notice);
    let small: &'static [u8] = Box::leak(vec![b' '; 100 * 1024].into_boxed_slice());
    assert!(lucky(&with(&[("Lucky/notes.txt", small)])).findings.iter().all(|f| f.rule != "zip.bomb"), "small files may compress well");
}

#[test]
fn bomb_detected_with_bounded_memory() {
    // 1 GB of zeros compresses to about 1 MB; reading stops at the per-file limit.
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("Lucky/huge.txt", SimpleFileOptions::default().large_file(true)).unwrap();
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..1024 {
        writer.write_all(&chunk).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let started = std::time::Instant::now();
    let report = lucky(&bytes);
    assert_eq!(finding(&report, "zip.bomb").severity, Severity::Critical);
    assert!(started.elapsed().as_secs() < 30, "stopped early");
}

#[test]
fn too_many_entries_is_critical() {
    let names: Vec<String> = (0..5001).map(|i| format!("Lucky/f{i}.txt")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
    let report = lucky(&zip(&entries));
    assert_eq!(finding(&report, "zip.bomb").severity, Severity::Critical);
}

#[test]
fn unknown_extension_is_a_notice() {
    let report = lucky(&with(&[("Lucky/tool.dll.bak", b"hello"), ("Lucky/run.ps1", b"Write-Host hi")]));
    let hits: Vec<_> = report.findings.iter().filter(|f| f.rule == "zip.extension").map(|f| f.file.as_str()).collect();
    assert_eq!(hits, ["Lucky/tool.dll.bak", "Lucky/run.ps1"]);
    assert!(report.findings.iter().filter(|f| f.rule == "zip.extension").all(|f| f.severity == Severity::Notice));
    assert!(!lucky(&with(&[("Lucky/LICENSE", b"MIT"), ("Lucky/README.md", b"# x")])).flagged(), "license and readme are fine");
    // The CurseForge packager file and a credits file are no strange types.
    let report = lucky(&with(&[("Lucky/.pkgmeta", b"package-as: Lucky"), ("Lucky/CREDITS", b"thanks")]));
    assert!(report.findings.iter().all(|f| f.rule != "zip.extension"), "{:#?}", report.findings);
}

#[test]
fn foreign_folder_is_critical() {
    let report = lucky(&with(&[("LuckyOdds/LuckyOdds.toc", b"## Interface: 11200\n")]));
    let f = finding(&report, "zip.foreign_folder");
    assert_eq!(f.severity, Severity::Critical);
    assert!(f.message.contains("LuckyOdds"));
}

#[test]
fn undeclared_folder_is_critical() {
    let report = lucky(&with(&[("Extra/Extra.toc", b"## Interface: 11200\n")]));
    assert_eq!(finding(&report, "zip.undeclared_folder").severity, Severity::Critical);
}

#[test]
fn missing_folder_warns() {
    let report = scan(&zip(&clean_addon()), &folders(&["Lucky", "LuckyPlates"]), &[]);
    let f = finding(&report, "zip.missing_folder");
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.message.contains("LuckyPlates"));
}

#[test]
fn toc_missing_file_warns() {
    let report = lucky(&zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\nLucky.lua\nLibs\\Gone.lua\n# comment.lua\n"),
        ("Lucky/Lucky.lua", b"x = 1"),
    ]));
    let f = finding(&report, "zip.toc_missing_file");
    assert_eq!((f.severity, f.line, f.snippet.as_deref()), (Severity::Warn, Some(3), Some("Libs\\Gone.lua")));
    assert_eq!(report.findings.iter().filter(|f| f.rule == "zip.toc_missing_file").count(), 1);
}

#[test]
fn interface_mismatch_is_info() {
    let report = lucky(&zip(&[("Lucky/Lucky.toc", b"## Interface: 30300\n")]));
    assert_eq!(finding(&report, "zip.toc_interface").severity, Severity::Info);
    assert!(!report.flagged());
}

#[test]
fn macosx_is_info() {
    let report = lucky(&with(&[("__MACOSX/Lucky/._Lucky.lua", b"\x00\x05\x16\x07"), ("Lucky/.DS_Store", b"Bud1")]));
    assert_eq!(finding(&report, "zip.hidden").severity, Severity::Info);
    assert!(!report.flagged(), "{:#?}", report.findings);
}

#[test]
fn git_files_are_info() {
    let files: &[(&str, &[u8])] = &[("Lucky/.gitignore", b"*.bak"), ("Lucky/.gitattributes", b"* text=auto"), ("Lucky/.github/workflows/ci.yml", b"on: push")];
    let report = lucky(&with(files));
    assert!(report.findings.iter().all(|f| f.rule != "zip.extension"), "{:#?}", report.findings);
    assert!(!report.flagged(), "{:#?}", report.findings);
}

#[test]
fn deep_nesting_warns() {
    let report = lucky(&with(&[("Lucky/a/b/c/d/e/f/g/h/i/j/k.lua", b"x")]));
    assert_eq!(finding(&report, "zip.deep_nesting").severity, Severity::Warn);
}

#[test]
fn encrypted_entry_is_critical() {
    let mut bytes = zip(&clean_addon());
    // Set the "encrypted" flag (bit 0) of every local and central header.
    for (sig, offset) in [(b"PK\x03\x04", 6), (b"PK\x01\x02", 8)] {
        let mut i = 0;
        while let Some(pos) = bytes[i..].windows(4).position(|w| w == sig) {
            bytes[i + pos + offset] |= 1;
            i += pos + 4;
        }
    }
    let report = lucky(&bytes);
    assert_eq!(finding(&report, "zip.unreadable").severity, Severity::Critical);
}

#[test]
fn garbage_is_unreadable() {
    let report = lucky(b"definitely not a zip");
    assert_eq!(finding(&report, "zip.unreadable").severity, Severity::Critical);
}

#[test]
fn only_files_the_game_loads_can_flag() {
    // The author's tests use io and dofile, but no .toc or XML loads them.
    let tests: &[(&str, &[u8])] = &[("Lucky/tests/runner.lua", b"for l in io.lines('x') do end
dofile('mock.lua')
")];
    let report = lucky(&with(tests));
    let f = finding(&report, "lua.forbidden_global");
    assert_eq!(f.severity, Severity::Info);
    assert!(f.message.ends_with("(in a file the game does not load)"), "{}", f.message);
    assert!(!report.flagged(), "{:#?}", report.findings);

    // Loaded through the .toc: still critical.
    let loaded = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200
Lucky.lua
Frames.xml
"),
        ("Lucky/Lucky.lua", b"os.execute('x')
"),
        ("Lucky/Frames.xml", b"<Ui><Include file=\"UI\\Inner.xml\"/></Ui>"),
        ("Lucky/UI/Inner.xml", b"<Ui><Script file=\"Inner.lua\"/></Ui>"),
        ("Lucky/UI/Inner.lua", b"io.open('x')
"),
    ]);
    let report = lucky(&loaded);
    let critical: Vec<&str> = report.findings.iter().filter(|f| f.severity == Severity::Critical).map(|f| f.file.as_str()).collect();
    assert_eq!(critical, ["Lucky/Lucky.lua", "Lucky/UI/Inner.lua"], "reached through Include and Script file");
}

#[test]
fn paths_with_dots_and_double_slashes_are_loaded() {
    let loaded = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\n./A.lua\nSub//B.lua\nx/../C.lua\nFrames.xml\n"),
        ("Lucky/A.lua", b"os.execute('a')\n"),
        ("Lucky/Sub/B.lua", b"os.execute('b')\n"),
        ("Lucky/C.lua", b"os.execute('c')\n"),
        ("Lucky/Frames.xml", b"<Ui><Script file=\"./D.lua\"/><Script file=\"Sub/../E.lua\"/></Ui>"),
        ("Lucky/D.lua", b"os.execute('d')\n"),
        ("Lucky/E.lua", b"os.execute('e')\n"),
    ]);
    let report = lucky(&loaded);
    let critical: Vec<&str> = report.findings.iter().filter(|f| f.severity == Severity::Critical).map(|f| f.file.as_str()).collect();
    assert_eq!(critical.len(), 5, "{:#?}", report.findings);
    assert!(report.findings.iter().all(|f| !f.rule.ends_with("missing_file") && !f.rule.ends_with("missing_script_file")), "{:#?}", report.findings);
}

#[test]
fn game_files_and_paths_from_the_game_folder() {
    let addon = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\n..\\..\\FrameXML\\Fonts.xml\nFrames.xml\n"),
        ("Lucky/Frames.xml", b"<Ui><Script file=\"Interface\\AddOns\\Lucky\\Sub\\A.lua\"/></Ui>"),
        ("Lucky/Sub/A.lua", b"os.execute('a')\n"),
    ]);
    let report = lucky(&addon);
    // The client's own FrameXML is no file of the zip; a path from the game folder is.
    assert!(report.findings.iter().all(|f| !f.rule.ends_with("missing_file") && !f.rule.ends_with("missing_script_file")), "{:#?}", report.findings);
    assert!(report.findings.iter().any(|f| f.file == "Lucky/Sub/A.lua" && f.severity == Severity::Critical), "{:#?}", report.findings);
}

#[test]
fn without_a_toc_everything_counts() {
    let report = scan(&zip(&[("Loose/Loose.lua", b"os.execute('x')
")]), &folders(&["Loose"]), &[]);
    assert!(report.flagged(), "{:#?}", report.findings);
}
