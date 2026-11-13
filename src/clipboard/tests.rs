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

        let bytes = std::fs::read(&path).unwrap();
        let picture = Payload {
            mime: format.to_mime_type().into(),
            bytes: bytes.as_slice().into(),
            sensitive: false,
        }
        .image_info()
        .unwrap_or_else(|| panic!("probe {name}"));
        assert_eq!(picture.format, format, "{name}");
        assert_eq!((picture.width, picture.height), (2, 3), "{name}");

        let bitmap = anti_webui::core::images::decode_bitmap(bytes, (2, 3))
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
    for (mime, bytes) in [
        ("image/png", &b"\x00\x01\x02not an image"[..]),
        ("image/webp", &b"RIFFtextWEBP ordinary text"[..]),
        ("image/png", &b"\x89PNG\r\n\x1a\nbroken"[..]),
    ] {
        assert!(
            Payload {
                mime: mime.into(),
                bytes: bytes.into(),
                sensitive: false,
            }
            .image_info()
            .is_none(),
            "{mime}"
        );
    }
}

#[test]
fn search_matches_text_beyond_the_display_preview() {
    let text = format!("{} searchable suffix 文本", "a".repeat(700));
    let entry = Entry::new(&clipboard_service::Entry {
        id: 1,
        payload: Payload {
            mime: "text/plain;charset=utf-8".into(),
            bytes: text.into_bytes().into(),
            sensitive: false,
        },
        pinned: false,
    });
    assert!(!entry.view.preview.contains("suffix"));
    assert!(entry.view.search.get().is_none());
    assert!(entry.matches(""));
    assert!(
        entry.view.search.get().is_none(),
        "empty search must not index payloads"
    );
    assert!(entry.matches(&" SEARCHABLE suffix ".trim().to_lowercase()));
    assert!(entry.matches("文本"));
    assert!(!entry.matches("not present"));
}

#[test]
fn legacy_string_previews_and_search_use_latin1_without_changing_copy_bytes() {
    let payload = Payload {
        mime: "STRING".into(),
        bytes: (&b"caf\xe9"[..]).into(),
        sensitive: false,
    };
    let entry = Entry::new(&clipboard_service::Entry {
        id: 1,
        payload: payload.clone(),
        pinned: false,
    });
    assert_eq!(entry.view.preview, "café");
    assert!(entry.matches(&"CAFÉ".to_lowercase()));
    assert_eq!(entry.payload, payload);
    let ambiguous = Payload {
        bytes: (&b"\xc3\xa9"[..]).into(),
        ..payload
    };
    assert_eq!(ambiguous.text_preview(600), "Ã©");
    assert_eq!(
        Payload {
            mime: "UTF8_STRING".into(),
            ..ambiguous
        }
        .text_preview(600),
        "é"
    );
}

#[test]
fn pin_reorder_and_delete_reuse_only_current_immutable_interpretations() {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let mut history = vec![
        clipboard_service::Entry {
            id: 1,
            payload: Payload {
                mime: "image/png".into(),
                bytes: encoded.into_inner().into(),
                sensitive: false,
            },
            pinned: false,
        },
        clipboard_service::Entry {
            id: 2,
            payload: Payload {
                mime: "STRING".into(),
                bytes: [vec![b'a'; 700], b" caf\xe9".to_vec()].concat().into(),
                sensitive: false,
            },
            pinned: false,
        },
    ];
    let mut previous = Rc::new(Vec::new());
    let first = Entry::refresh(&mut previous, &history);
    let picture = Rc::downgrade(&first[0].view);
    assert_eq!(first[0].view.picture.as_ref().unwrap().width, 2);
    assert_eq!(first[1].view.preview.chars().count(), 600);
    assert!(!first[1].view.preview.contains("café"));
    assert!(first[1].view.search.get().is_none());
    assert!(first[1].matches("café"));
    let search = first[1].view.search.get().unwrap().as_ptr();

    history[1].pinned = true;
    history.swap(0, 1);
    let second = Entry::refresh(&mut previous, &history);
    assert!(second[0].pinned);
    assert!(Rc::ptr_eq(&first[1].view, &second[0].view));
    assert!(Rc::ptr_eq(&first[0].view, &second[1].view));
    assert!(second[0].matches("café"));
    assert_eq!(second[0].view.search.get().unwrap().as_ptr(), search);
    assert!(std::sync::Arc::ptr_eq(
        &second[0].payload.bytes,
        &history[0].payload.bytes
    ));
    drop(first);
    drop(second);

    history.pop();
    let remaining = Entry::refresh(&mut previous, &history);
    assert_eq!(previous.len(), 1);
    assert_eq!(remaining[0].id, 2);
    assert!(
        picture.upgrade().is_none(),
        "removed interpretations must not be cached"
    );
}
