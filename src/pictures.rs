//! Icons and screenshots of the platform in the launcher: decoded with limits and scaled down, so a
//! broken or huge file cannot take the memory.

use image::{ImageReader, Limits, imageops::FilterType};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::Path;

/// No platform image comes near this; it bounds what a download may take.
pub const MAX_BYTES: u64 = 8 << 20;
const MAX_SIDE: u32 = 8192;

/// RGBA pixels, ready for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// PNG, JPEG or WebP `bytes` as a picture no larger than `max_edge` on its longer side.
pub fn decode(bytes: &[u8], max_edge: u32) -> Option<Picture> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let image = if image.width().max(image.height()) > max_edge { image.resize(max_edge, max_edge, FilterType::Triangle) } else { image };
    let rgba = image.to_rgba8();
    Some(Picture { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// The platform names every image after the SHA-256 of its bytes (`<hash>.webp`): that hash, so a
/// picture is downloaded once and only again when the platform's file changed.
pub fn content_hash(url: &str) -> Option<&str> {
    let stem = url.rsplit('/').next()?.split('.').next()?;
    (stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit())).then_some(stem)
}

fn file(dir: &Path, hash: &str) -> std::path::PathBuf {
    dir.join(format!("{}.webp", hash.to_ascii_lowercase()))
}

/// The cached bytes for `hash`, checked against it; a damaged file is deleted and `None` returned.
pub fn cached(dir: &Path, hash: &str) -> Option<Vec<u8>> {
    let path = file(dir, hash);
    let bytes = std::fs::read(&path).ok()?;
    if crate::install::hex(&Sha256::digest(&bytes)).eq_ignore_ascii_case(hash) {
        return Some(bytes);
    }
    let _ = std::fs::remove_file(&path);
    None
}

/// Keeps `bytes` for `hash` when they match it; false for bytes that do not (nothing is written).
// ponytail: pictures of changed or deleted addons stay in the folder; prune by age if it grows.
pub fn store(dir: &Path, hash: &str, bytes: &[u8]) -> bool {
    if !crate::install::hex(&Sha256::digest(bytes)).eq_ignore_ascii_case(hash) {
        return false;
    }
    std::fs::create_dir_all(dir).is_ok() && crate::write_atomic(&file(dir, hash), bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(width, height).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn decodes_and_scales_down_to_the_longer_side() {
        let picture = decode(&png(400, 200), 100).unwrap();
        assert_eq!((picture.width, picture.height), (100, 50));
        assert_eq!(picture.rgba.len(), 100 * 50 * 4);
        assert_eq!(decode(&png(40, 20), 100).map(|p| (p.width, p.height)), Some((40, 20)), "small ones stay");
    }

    const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn the_hash_is_the_file_name_the_platform_gives() {
        assert_eq!(content_hash(&format!("https://forge.test/img/{HASH}.webp")), Some(HASH));
        assert_eq!(content_hash("https://forge.test/img/logo.webp"), None);
        assert_eq!(content_hash(&format!("https://forge.test/img/{}.webp", &HASH[1..])), None);
    }

    #[test]
    fn the_cache_keeps_only_files_that_match_their_hash() {
        let dir = tempfile::tempdir().unwrap();
        // HASH is the SHA-256 of "test".
        assert!(!store(dir.path(), HASH, b"other bytes"), "bytes that do not match are not kept");
        assert_eq!(cached(dir.path(), HASH), None);
        assert!(store(dir.path(), HASH, b"test"));
        assert_eq!(cached(dir.path(), HASH).as_deref(), Some(&b"test"[..]));
        std::fs::write(dir.path().join(format!("{HASH}.webp")), b"changed on disk").unwrap();
        assert_eq!(cached(dir.path(), HASH), None, "a damaged file is loaded again");
        assert!(!dir.path().join(format!("{HASH}.webp")).exists(), "and thrown away");
    }

    #[test]
    fn rejects_what_is_no_image() {
        assert!(decode(b"<html>not found</html>", 100).is_none());
        assert!(decode(&[], 100).is_none());
    }
}
