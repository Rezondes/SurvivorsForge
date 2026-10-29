//! Uploaded images: recognised by their bytes, decoded with limits, scaled down to at most 1920 px
//! (icons cropped to 256 px squares) and re-encoded as WebP. Re-encoding drops EXIF data and
//! anything else hidden in the file.

use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use std::io::Cursor;

pub const MAX_UPLOAD: usize = 5 * 1024 * 1024;
pub const MAX_EDGE: u32 = 1920;
pub const ICON_EDGE: u32 = 256;
pub const MAX_SCREENSHOTS: i64 = 10;
const MAX_DECODE_EDGE: u32 = 10_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const QUALITY: f32 = 85.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Screenshot,
    Icon,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "screenshot" => Some(Kind::Screenshot),
            "icon" => Some(Kind::Icon),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Screenshot => "screenshot",
            Kind::Icon => "icon",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ImageError {
    /// Not PNG, JPEG or WebP by its bytes.
    Unsupported,
    /// Larger than the decode limits.
    TooLarge,
    Invalid,
    /// The chosen icon square is empty or leaves the image.
    BadCrop,
}

/// The square of the original image an icon is made from, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

pub struct Processed {
    pub webp: Vec<u8>,
    pub original: (u32, u32),
    pub stored: (u32, u32),
}

impl Processed {
    pub fn resized(&self) -> bool {
        self.original != self.stored
    }
}

/// The format by magic bytes. File names and content types are never trusted.
pub fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::WebP)
    } else {
        None
    }
}

/// `crop` picks the icon's square; without it the icon is the centered one. Screenshots ignore it.
pub fn process(bytes: &[u8], kind: Kind, crop: Option<Crop>) -> Result<Processed, ImageError> {
    let format = sniff(bytes).ok_or(ImageError::Unsupported)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => ImageError::TooLarge,
        _ => ImageError::Invalid,
    })?;
    let original = (image.width(), image.height());
    let image = match kind {
        Kind::Screenshot if original.0.max(original.1) > MAX_EDGE => image.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Lanczos3),
        Kind::Screenshot => image,
        Kind::Icon => {
            let side = original.0.min(original.1);
            let c = crop.unwrap_or(Crop { x: (original.0 - side) / 2, y: (original.1 - side) / 2, size: side });
            if c.size == 0 || c.x.saturating_add(c.size) > original.0 || c.y.saturating_add(c.size) > original.1 {
                return Err(ImageError::BadCrop);
            }
            let square = image.crop_imm(c.x, c.y, c.size, c.size);
            square.resize_exact(ICON_EDGE, ICON_EDGE, image::imageops::FilterType::Lanczos3)
        }
    };
    let rgba = DynamicImage::ImageRgba8(image.to_rgba8());
    let encoder = webp::Encoder::from_rgba(rgba.as_bytes(), rgba.width(), rgba.height());
    Ok(Processed { webp: encoder.encode(QUALITY).to_vec(), original, stored: (rgba.width(), rgba.height()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, RgbImage};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    fn jpeg_with_gps() -> Vec<u8> {
        let mut jpeg = Vec::new();
        let img = RgbImage::from_pixel(64, 64, image::Rgb([200, 10, 10]));
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode_image(&img).unwrap();
        // An APP1 Exif segment with a GPS marker right after the SOI marker.
        let payload = b"Exif\0\0GPSLatitude=48.1;GPSLongitude=11.5";
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn rejects_by_magic_bytes() {
        assert_eq!(process(b"GIF89a....", Kind::Screenshot, None).err(), Some(ImageError::Unsupported));
        assert_eq!(process(b"<svg onload=alert(1)>", Kind::Screenshot, None).err(), Some(ImageError::Unsupported));
        assert_eq!(process(b"<html>not a png</html>", Kind::Screenshot, None).err(), Some(ImageError::Unsupported));
        assert_eq!(process(b"\x89PNG\r\n\x1a\nbroken", Kind::Screenshot, None).err(), Some(ImageError::Invalid));
        assert!(process(&png(10, 10), Kind::Screenshot, None).is_ok());
    }

    #[test]
    fn icon_from_the_chosen_square() {
        // The red channel counts x up, so the left edge of the icon tells where the square starts.
        let red_at_left = |crop| {
            let out = process(&png(300, 200), Kind::Icon, crop).unwrap();
            webp::Decoder::new(&out.webp).decode().unwrap()[0]
        };
        assert!(red_at_left(Some(Crop { x: 200, y: 0, size: 100 })) > 170, "from x = 200");
        assert!(red_at_left(None) < 80, "centered: from x = 50");
        assert_eq!(process(&png(300, 200), Kind::Icon, Some(Crop { x: 250, y: 0, size: 100 })).err(), Some(ImageError::BadCrop));
        assert_eq!(process(&png(300, 200), Kind::Icon, Some(Crop { x: 0, y: 0, size: 0 })).err(), Some(ImageError::BadCrop));
    }

    #[test]
    fn downscales_with_notice() {
        let out = process(&png(3000, 1000), Kind::Screenshot, None).unwrap();
        assert_eq!((out.original, out.stored), ((3000, 1000), (1920, 640)));
        assert!(out.resized());
        assert_eq!(sniff(&out.webp), Some(ImageFormat::WebP));
        let small = process(&png(800, 600), Kind::Screenshot, None).unwrap();
        assert!(!small.resized());
    }

    #[test]
    fn icon_is_a_centered_square() {
        let out = process(&png(400, 300), Kind::Icon, None).unwrap();
        assert_eq!(out.stored, (ICON_EDGE, ICON_EDGE));
    }

    #[test]
    fn decompression_bomb_rejected() {
        assert_eq!(process(&png(12_000, 1), Kind::Screenshot, None).err(), Some(ImageError::TooLarge));
    }

    #[test]
    fn exif_removed() {
        let input = jpeg_with_gps();
        assert!(input.windows(3).any(|w| w == b"GPS"));
        let out = process(&input, Kind::Screenshot, None).unwrap();
        assert!(!out.webp.windows(3).any(|w| w == b"GPS"));
        assert!(!out.webp.windows(4).any(|w| w == b"Exif"));
    }
}
