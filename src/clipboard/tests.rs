use super::*;
use image::{ImageFormat, Rgba, RgbaImage};

#[test]
fn picture_files_expose_dimensions_and_decode_with_enabled_codecs() {
    let directory = tempfile::tempdir().unwrap();

    for (name, format) in [
        ("pixels.bmp", ImageFormat::Bmp),
        ("pixels.tiff", ImageFormat::Tiff),
        ("pixels.webp", ImageFormat::WebP),
    ] {
        let path = directory.path().join(name);
        let mut source = RgbaImage::new(2, 3);
        source.put_pixel(0, 0, Rgba([12, 34, 56, 255]));
        source.save_with_format(&path, format).unwrap();

        let picture = picture_from_file(&path).unwrap_or_else(|| panic!("probe {name}"));
        assert_eq!(picture.format, format, "{name}");
        assert_eq!((picture.width, picture.height), (2, 3), "{name}");

        let bitmap = anti_webui::core::images::decode_bitmap(path.clone(), (2, 3))
            .unwrap_or_else(|error| panic!("decode {name}: {error}"));
        assert_eq!((bitmap.width(), bitmap.height()), (2, 3), "{name}");
        let mut first_pixel = None;
        assert!(bitmap.with_levels(|level, size, pixels| {
            if level == 0 {
                assert_eq!(size, (2, 3), "{name}");
                first_pixel = Some(pixels[..4].to_vec());
            }
        }));
        assert_eq!(
            first_pixel.as_deref(),
            Some(&[12, 34, 56, 255][..]),
            "{name}"
        );
    }
}

#[test]
fn arbitrary_binary_and_damaged_files_are_not_pictures() {
    let directory = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("binary", &b"\x00\x01\x02not an image"[..]),
        ("fake-riff", &b"RIFFtextWEBP ordinary text"[..]),
        ("damaged", &b"\x89PNG\r\n\x1a\nbroken"[..]),
    ] {
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        assert!(picture_from_file(&path).is_none(), "{name}");
    }
}

#[test]
fn only_binary_previews_are_probe_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old-cliphist.webp");
    RgbaImage::from_pixel(2, 3, Rgba([12, 34, 56, 255]))
        .save_with_format(&path, ImageFormat::WebP)
        .unwrap();
    let bytes = std::fs::read(path).unwrap();
    let preview = String::from_utf8_lossy(&bytes);
    assert!(Picture::parse(&preview).is_none());
    assert!(
        preview.chars().any(|c| c == '\u{fffd}' || c.is_control()),
        "fixture must resemble cliphist 0.7's raw WebP preview"
    );
    assert!(needs_picture_probe(&preview));

    assert!(!needs_picture_probe("ordinary UTF-8 文本 with spaces"));
    assert!(!needs_picture_probe("https://example.com/image.webp"));
}
