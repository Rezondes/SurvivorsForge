//! Scans addons on this computer with forge-scan: no server, no internet.
//!
//! The same rules as the platform, so a clean result means "nothing found", never "safe".

use forge_scan::{Report, Severity};
use std::io::Write;
use std::path::Path;

/// How many findings the summary lists before it only counts.
const LISTED: usize = 8;

/// Scans the zip at `zip` for an addon that installs `folders`.
pub fn scan_zip(zip: &Path, folders: &[String]) -> crate::Result<Report> {
    let bytes = std::fs::read(zip)?;
    Ok(run(bytes, folders.to_vec()))
}

/// Scans the installed `folders` in `addons_dir`, packed into a zip in memory first.
pub fn scan_folders(addons_dir: &Path, folders: &[String]) -> crate::Result<Report> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for folder in folders {
        add_dir(&mut zip, &addons_dir.join(folder), folder, options)?;
    }
    let bytes = zip.finish().map_err(|e| crate::Error::Zip(e.to_string()))?.into_inner();
    Ok(run(bytes, folders.to_vec()))
}

fn add_dir(
    zip: &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    dir: &Path,
    name: &str,
    options: zip::write::SimpleFileOptions,
) -> crate::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = format!("{name}/{}", entry.file_name().to_string_lossy());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            add_dir(zip, &entry.path(), &path, options)?;
        } else if kind.is_file() {
            zip.start_file(path, options).map_err(|e| crate::Error::Zip(e.to_string()))?;
            zip.write_all(&std::fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

/// full_moon parses recursively, so the scan needs its own big stack.
fn run(bytes: Vec<u8>, folders: Vec<String>) -> Report {
    std::thread::Builder::new()
        .stack_size(forge_scan::SCAN_STACK_BYTES)
        .spawn(move || forge_scan::scan(&bytes, &folders, &[]))
        .ok()
        .and_then(|t| t.join().ok())
        .unwrap_or_else(failed)
}

/// A scan that crashed counts as a critical finding, so it never reads as clean.
fn failed() -> Report {
    Report { findings: vec![finding("scan.crashed", Severity::Critical, "", None, "The local scan stopped on this addon")] }
}

fn finding(rule: &str, severity: Severity, file: &str, line: Option<u32>, message: &str) -> forge_scan::Finding {
    forge_scan::Finding { rule: rule.into(), severity, file: file.into(), line, message: message.into(), snippet: None }
}

/// Whether the report holds anything worth a warning.
pub fn flagged(report: &Report) -> bool {
    report.flagged()
}

/// The result in a few lines for a dialog: counts by severity, then the most severe findings.
pub fn summary(report: &Report) -> String {
    let shown: Vec<_> = report.findings.iter().filter(|f| f.severity > Severity::Info).collect();
    if shown.is_empty() {
        return "Nothing found. The local scan uses the same rules as the platform. A clean result means nothing was found, not that the addon is safe.".into();
    }
    let count = |s: Severity| shown.iter().filter(|f| f.severity == s).count();
    let counts: Vec<String> = [(Severity::Critical, "critical"), (Severity::Warn, "warning"), (Severity::Notice, "notice")]
        .into_iter()
        .filter(|(s, _)| count(*s) > 0)
        .map(|(s, label)| format!("{} {label}", count(s)))
        .collect();
    let mut sorted = shown.clone();
    sorted.sort_by_key(|f| std::cmp::Reverse(f.severity));
    let mut lines = vec![format!("Found: {}.", counts.join(", "))];
    for f in sorted.iter().take(LISTED) {
        let place = match (f.file.as_str(), f.line) {
            ("", _) => String::new(),
            (file, Some(line)) => format!(" ({file}:{line})"),
            (file, None) => format!(" ({file})"),
        };
        lines.push(format!("{}: {}{place}", label(f.severity), f.message));
    }
    if sorted.len() > LISTED {
        lines.push(format!("And {} more.", sorted.len() - LISTED));
    }
    lines.join("\n")
}

fn label(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "Critical",
        Severity::Warn => "Warning",
        Severity::Notice => "Notice",
        Severity::Info => "Info",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn folders_are_zipped_and_scanned() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Lucky/sub")).unwrap();
        fs::write(dir.path().join("Lucky/Lucky.toc"), "## Interface: 11200\nLucky.lua\nsub/x.lua\n").unwrap();
        fs::write(dir.path().join("Lucky/Lucky.lua"), "print('hi')\n").unwrap();
        fs::write(dir.path().join("Lucky/sub/x.lua"), "os.execute('x')\n").unwrap();
        let report = scan_folders(dir.path(), &["Lucky".into()]).unwrap();
        assert!(flagged(&report), "{report:?}");
        assert!(report.findings.iter().any(|f| f.severity == Severity::Critical && f.file == "Lucky/sub/x.lua"));
    }

    #[test]
    fn clean_folders_are_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Lucky")).unwrap();
        fs::write(dir.path().join("Lucky/Lucky.toc"), "## Interface: 11200\nLucky.lua\n").unwrap();
        fs::write(dir.path().join("Lucky/Lucky.lua"), "print('hi')\n").unwrap();
        let report = scan_folders(dir.path(), &["Lucky".into()]).unwrap();
        assert!(!flagged(&report), "{report:?}");
        assert!(summary(&report).starts_with("Nothing found"));
    }

    #[test]
    fn summary_names_counts_and_findings() {
        let report = Report {
            findings: vec![
                finding("lua.os", Severity::Critical, "A/a.lua", Some(3), "Calls os.execute"),
                finding("zip.x", Severity::Warn, "", None, "Odd zip"),
                finding("zip.y", Severity::Info, "A/b.lua", None, "Just info"),
            ],
        };
        let text = summary(&report);
        assert!(text.starts_with("Found: 1 critical, 1 warning."), "{text}");
        assert!(text.contains("Critical: Calls os.execute (A/a.lua:3)"), "{text}");
        assert!(text.contains("Warning: Odd zip"), "{text}");
        assert!(!text.contains("Just info"), "{text}");
    }
}
