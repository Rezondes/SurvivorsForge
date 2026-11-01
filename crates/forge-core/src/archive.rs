//! Finds addon folders inside a zip and extracts them safely.
//!
//! Rule: a folder `D` holding `D/D.toc` is an addon named `D`. A `.toc` lying in a folder
//! of another name (a GitHub source zip like `owner-repo-sha/BordoUI.toc`) makes that folder
//! an addon named after the `.toc` stem. Addons nested inside an addon (libraries) don't count.

use crate::toc::Toc;
use crate::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct DetectedAddon {
    /// The folder name it gets in AddOns.
    pub name: String,
    /// Its folder inside the zip, `/`-separated, without trailing slash; empty for the zip root.
    pub prefix: String,
    pub toc: Toc,
}

/// Normalizes a zip entry name to `/`-separated components and rejects anything that
/// could escape the target folder.
pub fn safe_components(raw: &str) -> Result<Vec<String>> {
    let name = raw.replace('\\', "/");
    if name.starts_with('/') || name.contains(':') {
        return Err(Error::UnsafePath(raw.to_string()));
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(Error::UnsafePath(raw.to_string())),
            p => parts.push(p.to_string()),
        }
    }
    Ok(parts)
}

fn open(zip_path: &Path) -> Result<zip::ZipArchive<File>> {
    zip::ZipArchive::new(File::open(zip_path)?).map_err(|e| Error::Zip(e.to_string()))
}

pub fn analyze(zip_path: &Path) -> Result<Vec<DetectedAddon>> {
    analyze_reader(File::open(zip_path)?)
}

/// A `.toc` lists a title, a few tags and the files to load: a few KB. Reading no more keeps a
/// `.toc` that inflates to gigabytes from filling the memory.
pub const MAX_TOC_BYTES: u64 = 64 * 1024;

/// `analyze` for a zip that is not a file, e.g. one in memory.
pub fn analyze_reader<R: Read + Seek>(reader: R) -> Result<Vec<DetectedAddon>> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| Error::Zip(e.to_string()))?;
    // (depth, folder name doesn't match stem, prefix, name, toc). Several `.toc` files in one
    // folder (`pfUI.toc`, `pfUI-tbc.toc`) are client variants: the 1.12 one wins, then the shortest name.
    let mut candidates: Vec<(usize, bool, String, String, Toc)> = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| Error::Zip(e.to_string()))?;
        let parts = safe_components(file.name())?;
        let Some((last, dirs)) = parts.split_last() else { continue };
        if file.is_dir() || !last.to_ascii_lowercase().ends_with(".toc") {
            continue;
        }
        let stem = &last[..last.len() - 4];
        let matching = dirs.last().is_some_and(|d| d.eq_ignore_ascii_case(stem));
        let name = if matching { dirs.last().unwrap().clone() } else { stem.to_string() };
        let mut bytes = Vec::new();
        (&mut file).take(MAX_TOC_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TOC_BYTES {
            return Err(Error::TocTooLarge(parts.join("/")));
        }
        candidates.push((dirs.len(), !matching, dirs.join("/"), name.clone(), Toc::parse_bytes(&bytes, &name)));
    }
    candidates.sort_by_key(|(depth, other_name, prefix, name, toc)| {
        (*depth, *other_name, prefix.clone(), toc.is_out_of_date() || toc.interface.is_none(), name.len())
    });

    let mut found: Vec<DetectedAddon> = Vec::new();
    for (_, _, prefix, name, toc) in candidates {
        let inside_found = found.iter().any(|f| {
            f.prefix.is_empty() || prefix == f.prefix || prefix.starts_with(&format!("{}/", f.prefix))
        });
        let duplicate = found.iter().any(|f| f.name.eq_ignore_ascii_case(&name));
        if !inside_found && !duplicate {
            found.push(DetectedAddon { name, prefix, toc });
        }
    }
    if found.is_empty() {
        return Err(Error::NoAddonFound);
    }
    Ok(found)
}

/// Writes each detected addon to `target/<name>/`. Nothing outside the addon folders is written.
pub fn extract_addons(zip_path: &Path, addons: &[DetectedAddon], target: &Path) -> Result<()> {
    let mut zip = open(zip_path)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| Error::Zip(e.to_string()))?;
        let parts = safe_components(file.name())?;
        let path = parts.join("/");
        for addon in addons {
            let rel = if addon.prefix.is_empty() {
                Some(path.as_str())
            } else {
                path.strip_prefix(&addon.prefix).and_then(|r| r.strip_prefix('/'))
            };
            let Some(rel) = rel.filter(|r| !r.is_empty()) else { continue };
            let out = target.join(&addon.name).join(rel);
            if file.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                std::fs::create_dir_all(out.parent().unwrap())?;
                std::io::copy(&mut file, &mut File::create(&out)?)?;
            }
            break;
        }
    }
    for addon in addons {
        std::fs::create_dir_all(target.join(&addon.name))?;
    }
    Ok(())
}
