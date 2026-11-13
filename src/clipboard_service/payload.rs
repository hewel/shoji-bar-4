//! MIME, encoding and codec policy for immutable clipboard bytes.
//! History admission is deliberately stricter than transport completeness: ownership can
//! preserve up to 200 MiB without decoding an image or admitting it to history.

use super::Payload;
use image::{ImageDecoder, ImageFormat};
use std::io::Cursor;

pub(super) const MAX_TEXT: usize = 4 * 1024 * 1024;
pub(super) const MAX_IMAGE: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Utf8,
    LegacyText,
    Latin1,
    Image(ImageFormat),
}

// Order is receive preference, not an encoding classification. Never infer aliases
// from positions: STRING is Latin-1, while TEXT does not promise UTF-8.
const TYPES: &[(&str, Kind)] = &[
    ("text/plain;charset=utf-8", Kind::Utf8),
    ("text/plain; charset=utf-8", Kind::Utf8),
    ("text/plain", Kind::Utf8),
    ("UTF8_STRING", Kind::Utf8),
    ("TEXT", Kind::LegacyText),
    ("STRING", Kind::Latin1),
    ("image/png", Kind::Image(ImageFormat::Png)),
    ("image/jpeg", Kind::Image(ImageFormat::Jpeg)),
    ("image/webp", Kind::Image(ImageFormat::WebP)),
    ("image/gif", Kind::Image(ImageFormat::Gif)),
    ("image/bmp", Kind::Image(ImageFormat::Bmp)),
    ("image/tiff", Kind::Image(ImageFormat::Tiff)),
];

fn kind(mime: &str) -> Option<Kind> {
    TYPES
        .iter()
        .find_map(|(name, kind)| (*name == mime).then_some(*kind))
}

pub(super) fn choose_mime(mimes: &[String]) -> Option<String> {
    TYPES
        .iter()
        .find_map(|(wanted, _)| mimes.iter().find(|mime| mime.as_str() == *wanted).cloned())
}

pub(super) fn history_limit(mime: &str) -> Option<usize> {
    kind(mime).map(|kind| match kind {
        Kind::Image(_) => MAX_IMAGE,
        _ => MAX_TEXT,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageInfo {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

impl Payload {
    pub(super) fn supported(&self) -> bool {
        kind(&self.mime).is_some()
    }

    /// Cheap prefilter only: decoding and nonblank text validation belong to admission.
    pub(super) fn fits_history(&self) -> bool {
        !self.sensitive && history_limit(&self.mime).is_some_and(|limit| self.bytes.len() <= limit)
    }

    pub(super) fn valid_for_history(&self) -> bool {
        if !self.fits_history() || self.bytes.is_empty() {
            return false;
        }
        match kind(&self.mime) {
            Some(Kind::Latin1) => self
                .bytes
                .iter()
                .any(|byte| !char::from(*byte).is_whitespace()),
            Some(Kind::Utf8 | Kind::LegacyText) => {
                std::str::from_utf8(&self.bytes).is_ok_and(|text| !text.trim().is_empty())
            }
            Some(Kind::Image(format)) => {
                let mut reader = image::ImageReader::with_format(Cursor::new(&self.bytes), format);
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(16_384);
                limits.max_image_height = Some(16_384);
                limits.max_alloc = Some(128 * 1024 * 1024);
                reader.limits(limits);
                reader.decode().is_ok()
            }
            None => false,
        }
    }

    pub(super) fn same_content(&self, other: &Self) -> bool {
        if self.bytes != other.bytes {
            return false;
        }
        if self.mime == other.mime {
            return true;
        }
        match (kind(&self.mime), kind(&other.mime)) {
            (Some(Kind::Image(_)) | None, _) | (_, Some(Kind::Image(_)) | None) => false,
            (left, right) => self.bytes.is_ascii() || (left == Some(Kind::Utf8) && right == left),
        }
    }

    /// Only aliases that can serve the original bytes without transcoding.
    pub(super) fn copy_mimes(&self) -> Vec<String> {
        let mut mimes = vec![self.mime.clone()];
        let Some(source) = kind(&self.mime) else {
            return mimes;
        };
        if matches!(source, Kind::Image(_)) {
            return mimes;
        }
        let ascii = self.bytes.is_ascii();
        let utf8 = source == Kind::Utf8 && std::str::from_utf8(&self.bytes).is_ok();
        for &(mime, target) in TYPES {
            if mime != self.mime
                && !matches!(target, Kind::Image(_))
                && (ascii || (utf8 && target == Kind::Utf8))
            {
                mimes.push(mime.to_owned());
            }
        }
        mimes
    }

    pub(crate) fn text_preview(&self, characters: usize) -> String {
        if kind(&self.mime) == Some(Kind::Latin1) {
            self.bytes
                .iter()
                .take(characters)
                .map(|byte| char::from(*byte))
                .collect()
        } else {
            String::from_utf8_lossy(&self.bytes)
                .chars()
                .take(characters)
                .collect()
        }
    }

    /// Construct once per immutable view, rather than decoding/lowercasing on every search.
    pub(crate) fn search_text(&self) -> String {
        if kind(&self.mime) == Some(Kind::Latin1) {
            self.bytes
                .iter()
                .flat_map(|byte| char::from(*byte).to_lowercase())
                .collect()
        } else {
            String::from_utf8_lossy(&self.bytes).to_lowercase()
        }
    }

    /// Read dimensions from the declared codec without decoding the full-size pixels.
    pub(crate) fn image_info(&self) -> Option<ImageInfo> {
        let Kind::Image(format) = kind(&self.mime)? else {
            return None;
        };
        let decoder = image::ImageReader::with_format(Cursor::new(&self.bytes), format)
            .into_decoder()
            .ok()?;
        let (width, height) = decoder.dimensions();
        (width > 0 && height > 0).then_some(ImageInfo {
            format,
            width,
            height,
        })
    }
}

/// A cheap EOF sanity check, not image validation. TIFF has no fixed EOF marker.
pub(super) fn looks_complete(mime: &str, bytes: &[u8]) -> bool {
    match kind(mime) {
        Some(Kind::Image(ImageFormat::Png)) => {
            bytes.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82])
        }
        Some(Kind::Image(ImageFormat::Jpeg)) => bytes.ends_with(&[0xff, 0xd9]),
        Some(Kind::Image(ImageFormat::Gif)) => bytes.ends_with(&[0x3b]),
        Some(Kind::Image(ImageFormat::WebP)) if bytes.len() >= 12 && &bytes[..4] == b"RIFF" => {
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize == bytes.len() - 8
        }
        Some(Kind::Image(ImageFormat::Bmp)) if bytes.len() >= 6 && &bytes[..2] == b"BM" => {
            u32::from_le_bytes(bytes[2..6].try_into().unwrap()) as usize == bytes.len()
        }
        _ => true,
    }
}
