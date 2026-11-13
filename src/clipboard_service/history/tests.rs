use super::*;
use crate::clipboard_service::payload::MAX_TEXT;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

fn encode(entries: &[Entry], next_id: u64) -> Zeroizing<Vec<u8>> {
    let stored = entries.iter().map(|entry| (entry.id, [0; 24])).collect();
    encode_manifest(entries, next_id, &stored)
}

fn text(value: &str) -> Payload {
    Payload {
        mime: "text/plain;charset=utf-8".to_owned(),
        bytes: Arc::from(value.as_bytes()),
        sensitive: false,
    }
}

fn open(directory: &Path) -> History {
    let history = History::open_with_key(directory.to_owned(), |_| Ok(Zeroizing::new([71; 32])));
    assert_eq!(history.error(), None);
    history
}

fn png() -> Payload {
    let mut bytes = io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    Payload {
        mime: "image/png".to_owned(),
        bytes: Arc::from(bytes.into_inner()),
        sensitive: false,
    }
}

#[test]
fn round_trip_preserves_ids_original_image_mime_bytes_and_pin_order() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("history");
    let mut history = open(&directory);
    let picture = png();
    assert!(history.capture(text("first")));
    let first_id = history.entries()[0].id;
    assert!(history.capture(picture.clone()));
    let image_id = history.entries()[0].id;
    assert!(history.toggle_pin(first_id));
    assert!(history.capture(text("third")));
    let expected = history.entries().to_vec();
    drop(history);

    let history = open(&directory);
    assert_eq!(history.entries(), expected);
    assert_eq!(history.entries()[0].id, first_id);
    assert!(history.entries()[0].pinned);
    assert_eq!(
        history
            .entries()
            .iter()
            .find(|entry| entry.id == image_id)
            .unwrap()
            .payload,
        picture
    );
}

#[test]
fn successful_deletion_never_resurrects_and_ids_are_not_reused_even_after_emptying() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("deleted oldest"));
    let oldest = history.entries()[0].id;
    history.capture(text("deleted newest"));
    let newest = history.entries()[0].id;
    history.toggle_pin(oldest);
    assert!(history.remove(oldest));
    assert!(history.remove(newest));
    assert!(!history.remove(newest));
    assert_eq!(history.error(), None);
    assert!(!temporary.path().join(payload_file(oldest)).exists());
    assert!(!temporary.path().join(payload_file(newest)).exists());
    drop(history);

    let mut history = open(temporary.path());
    assert!(history.entries().is_empty());
    assert!(history.capture(text("replacement")));
    assert!(history.entries()[0].id > newest);
    drop(history);
    assert_eq!(open(temporary.path()).entries().len(), 1);
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
}

#[test]
fn repeats_promote_without_new_ids_and_never_duplicate_or_move_pins() {
    let mut history = History::memory();
    history.capture(text("alpha"));
    let alpha = history.entries()[0].id;
    history.capture(text("beta"));
    let beta = history.entries()[0].id;
    let mut alias = text("alpha");
    alias.mime = "UTF8_STRING".to_owned();
    assert!(history.capture(alias.clone()));
    assert_eq!(history.entries()[0].id, alpha);
    assert_eq!(
        history.entries()[0].payload.mime,
        "text/plain;charset=utf-8"
    );
    assert!(!history.capture(alias.clone()));
    history.toggle_pin(beta);
    history.toggle_pin(alpha);
    let expected = history.entries().to_vec();
    assert!(!history.capture(text("beta")));
    assert_eq!(history.entries(), expected);
    assert!(!history.capture(alias));
    assert!(history.toggle_pin(alpha));
    assert_eq!(history.entries()[0].id, beta);
    assert!(history.entries()[0].pinned);
    assert_eq!(history.entries()[1].id, alpha);
    assert!(!history.entries()[1].pinned);
    assert!(!history.toggle_pin(u64::MAX));
}

#[test]
fn default_entry_limit_evicts_oldest_unpinned_and_refuses_when_all_pinned() {
    let mut history = History::memory();
    history.capture(text("protected"));
    let protected = history.entries()[0].id;
    history.toggle_pin(protected);
    for index in 0..105 {
        history.capture(text(&format!("entry {index}")));
    }
    assert_eq!(history.entries().len(), MAX_ENTRIES);
    assert_eq!(history.entries()[0].id, protected);
    assert_eq!(history.entries().last().unwrap().payload, text("entry 6"));
    let ids: Vec<_> = history
        .entries()
        .iter()
        .filter(|entry| !entry.pinned)
        .map(|entry| entry.id)
        .collect();
    for id in ids {
        history.toggle_pin(id);
    }
    let expected = history.entries().to_vec();
    assert!(!history.capture(text("cannot fit")));
    assert_eq!(history.entries(), expected);
}

#[test]
fn byte_budget_evicts_unpinned_but_refuses_without_harming_pinned_data() {
    // Exercise the production eviction algorithm with a small injected byte budget.
    let mut history = History::memory();
    history.byte_limit = 12;
    history.capture(text("aaaa"));
    let pin = history.entries()[0].id;
    history.toggle_pin(pin);
    history.capture(text("bbbb"));
    history.capture(text("cccc"));
    assert!(history.capture(text("dddddd")));
    assert_eq!(history.entries().len(), 2);
    assert_eq!(history.entries()[0].id, pin);
    assert_eq!(history.entries()[1].payload, text("dddddd"));
    history.toggle_pin(history.entries()[1].id);
    let expected = history.entries().to_vec();
    assert!(!history.capture(text("eee")));
    assert_eq!(history.entries(), expected);
}

#[test]
fn rejects_sensitive_blank_unsupported_malformed_and_oversized_payloads() {
    let mut history = History::memory();
    let mut sensitive = text("do not retain my password");
    sensitive.sensitive = true;
    assert!(!history.capture(sensitive));
    for value in ["", " \n\t", "\u{2003}"] {
        assert!(!history.capture(text(value)));
    }
    let picture = png();
    for (mime, bytes) in [
        ("application/octet-stream", b"unknown".as_slice()),
        ("x-kde-passwordManagerHint", b"secret".as_slice()),
        ("text/plain", b"\xff\xfe".as_slice()),
        ("image/png", b"not a PNG".as_slice()),
        ("image/jpeg", picture.bytes.as_ref()),
    ] {
        assert!(!history.capture(Payload {
            mime: mime.to_owned(),
            bytes: Arc::from(bytes),
            sensitive: false
        }));
    }
    assert!(!history.capture(text(&"a".repeat(MAX_TEXT + 1))));
    assert!(!history.capture(Payload {
        mime: "image/png".to_owned(),
        bytes: Arc::from(vec![0; MAX_IMAGE + 1]),
        sensitive: false
    }));
    assert!(history.entries().is_empty());
    assert!(history.capture(text("valid text")));
    assert!(history.capture(png()));
}

#[test]
fn manifest_payload_and_temporary_files_never_contain_plaintext_and_have_private_permissions() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("private").join("history");
    let mut history = open(&directory);
    let marker = "a uniquely identifiable clipboard secret, never a filename";
    history.capture(text(marker));
    history.toggle_pin(history.entries()[0].id);
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        assert!(name == SNAPSHOT || payload_id(name).is_some());
        assert_eq!(
            entry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = fs::read(entry.path()).unwrap();
        assert!(
            !bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
        );
        assert!(!bytes.windows(10).any(|window| window == b"text/plain"));
    }
    let first = fs::read(directory.join(SNAPSHOT)).unwrap();
    history.toggle_pin(history.entries()[0].id);
    let second = fs::read(directory.join(SNAPSHOT)).unwrap();
    assert_ne!(
        &first[8..32],
        &second[8..32],
        "every snapshot needs a fresh nonce"
    );
    assert!(history.error().is_none());
}

#[test]
fn missing_key_is_memory_only_without_fallback_or_overwriting_existing_history() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("saved before key became unavailable"));
    let expected = history.entries().to_vec();
    drop(history);
    let encrypted = fs::read(temporary.path().join(SNAPSHOT)).unwrap();
    let mut unavailable = History::open_with_key(temporary.path().to_owned(), |existing| {
        assert!(
            existing,
            "existing encrypted data must forbid creation of another key"
        );
        Err("encryption key missing".to_owned())
    });
    assert!(unavailable.entries().is_empty());
    assert!(unavailable.error().unwrap().contains("Memory-only"));
    assert!(unavailable.capture(text("memory only")));
    unavailable.toggle_pin(unavailable.entries()[0].id);
    assert_eq!(
        fs::read(temporary.path().join(SNAPSHOT)).unwrap(),
        encrypted
    );
    drop(unavailable);
    assert_eq!(open(temporary.path()).entries(), expected);

    let empty = TempDir::new().unwrap();
    let mut unavailable = History::open_with_key(empty.path().to_owned(), |existing| {
        assert!(!existing);
        Err("keyring locked".to_owned())
    });
    unavailable.capture(text("nothing on disk"));
    assert_eq!(fs::read_dir(empty.path()).unwrap().count(), 0);
    assert!(History::memory().error().is_none());
}

#[test]
fn authenticated_tamper_wrong_key_and_truncation_leave_existing_ciphertext_untouched() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("original"));
    drop(history);
    let path = temporary.path().join(SNAPSHOT);
    let original = fs::read(&path).unwrap();
    for index in [0, 8, 32, original.len() - 1] {
        let mut corrupted = original.clone();
        corrupted[index] ^= 0x80;
        fs::write(&path, &corrupted).unwrap();
        let mut history = History::open_with_key(temporary.path().to_owned(), |_| {
            Ok(Zeroizing::new([71; 32]))
        });
        assert!(history.error().is_some());
        assert!(history.entries().is_empty());
        history.capture(text("do not overwrite damaged history"));
        assert_eq!(fs::read(&path).unwrap(), corrupted);
    }
    fs::write(&path, &original).unwrap();
    let mut wrong_key = History::open_with_key(temporary.path().to_owned(), |_| {
        Ok(Zeroizing::new([72; 32]))
    });
    assert!(wrong_key.error().unwrap().contains("authentication"));
    wrong_key.capture(text("not saved"));
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(wrong_key);
    fs::write(&path, &original[..20]).unwrap();
    let truncated = History::open_with_key(temporary.path().to_owned(), |_| {
        Ok(Zeroizing::new([71; 32]))
    });
    assert!(truncated.error().is_some());
    drop(truncated);
    fs::write(&path, original).unwrap();
    assert_eq!(
        open(temporary.path()).entries()[0].payload,
        text("original")
    );
}

#[test]
fn failed_save_is_reported_and_later_success_persists_current_state_not_stale_deleted_entries() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("history");
    let moved = temporary.path().join("moved");
    let mut history = open(&directory);
    history.capture(text("delete this"));
    let deleted = history.entries()[0].id;
    fs::rename(&directory, &moved).unwrap();
    fs::write(&directory, b"not a directory").unwrap();
    assert!(history.remove(deleted));
    assert!(history.error().unwrap().contains("persistence"));
    assert!(history.entries().is_empty());
    fs::remove_file(&directory).unwrap();
    fs::rename(&moved, &directory).unwrap();
    assert!(history.capture(text("new current state")));
    assert_eq!(history.error(), None);
    drop(history);
    let history = open(&directory);
    assert_eq!(history.entries().len(), 1);
    assert_eq!(history.entries()[0].payload, text("new current state"));
    assert!(history.entries()[0].id > deleted);
}

#[test]
fn symlinked_directory_and_snapshot_are_rejected_without_touching_targets() {
    let temporary = TempDir::new().unwrap();
    let target = temporary.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let alias = temporary.path().join("alias");
    symlink(&target, &alias).unwrap();
    let history =
        History::open_with_key(alias, |_| panic!("unsafe directory must not reach keyring"));
    assert!(history.error().is_some());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let directory = temporary.path().join("history");
    fs::create_dir(&directory).unwrap();
    let external = target.join("external");
    fs::write(&external, b"must not be modified").unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&external, directory.join(SNAPSHOT)).unwrap();
    let mut history = History::open_with_key(directory, |existing| {
        assert!(existing);
        Ok(Zeroizing::new([71; 32]))
    });
    assert!(history.error().is_some());
    history.capture(text("not saved"));
    assert_eq!(fs::read(&external).unwrap(), b"must not be modified");
    assert_eq!(
        fs::metadata(&external).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn second_writer_is_memory_only_and_cannot_replace_the_first_writers_snapshot() {
    let temporary = TempDir::new().unwrap();
    let mut first = open(temporary.path());
    first.capture(text("first writer"));
    let original = fs::read(temporary.path().join(SNAPSHOT)).unwrap();
    let mut second = History::open_with_key(temporary.path().to_owned(), |_| {
        panic!("must lock before keyring")
    });
    assert!(second.error().unwrap().contains("another process"));
    second.capture(text("second writer"));
    assert_eq!(fs::read(temporary.path().join(SNAPSHOT)).unwrap(), original);
}

#[test]
fn authenticated_but_invalid_records_are_rejected_without_partial_restore() {
    let valid = Entry {
        id: 1,
        payload: text("valid"),
        pinned: false,
    };
    let bad_order = [
        valid.clone(),
        Entry {
            id: 2,
            payload: text("pin"),
            pinned: true,
        },
    ];
    assert!(decode(&encode(&bad_order, 3)).is_err());
    assert!(decode(&encode(&[valid.clone(), valid.clone()], 3)).is_err());
    assert!(decode(&encode(&[valid.clone()], 1)).is_err());
    let mut sensitive_mime = valid.clone();
    sensitive_mime.payload.mime = "x-kde-passwordManagerHint".to_owned();
    assert!(decode(&encode(&[sensitive_mime], 2)).is_err());
    let mut trailing = encode(&[valid], 2);
    trailing.push(0);
    assert!(decode(&trailing).is_err());
    assert!(decode(&u64::MAX.to_le_bytes()).is_err());
}

#[test]
fn metadata_changes_and_new_captures_never_rewrite_unchanged_payload_files() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text(&"a".repeat(MAX_TEXT)));
    let large = history.entries()[0].id;
    history.capture(text("small"));
    let small = history.entries()[0].id;
    let large_path = temporary.path().join(payload_file(large));
    let small_path = temporary.path().join(payload_file(small));
    let large_encrypted = fs::read(&large_path).unwrap();
    let small_encrypted = fs::read(&small_path).unwrap();
    let large_inode = fs::metadata(&large_path).unwrap().ino();
    let small_inode = fs::metadata(&small_path).unwrap().ino();
    drop(history);

    // Reopening must restore which immutable payload files already exist.
    let mut history = open(temporary.path());
    history.toggle_pin(large);
    history.capture(text("next capture"));
    assert_eq!(fs::read(&large_path).unwrap(), large_encrypted);
    assert_eq!(fs::read(&small_path).unwrap(), small_encrypted);
    assert_eq!(fs::metadata(&large_path).unwrap().ino(), large_inode);
    assert_eq!(fs::metadata(&small_path).unwrap().ino(), small_inode);
    history.remove(small);
    assert!(!small_path.exists());
    assert_eq!(fs::read(&large_path).unwrap(), large_encrypted);
    assert_eq!(fs::metadata(&large_path).unwrap().ino(), large_inode);
    assert!(fs::metadata(temporary.path().join(SNAPSHOT)).unwrap().len() < 1024);
    assert!(history.error().is_none());
}

#[test]
fn failed_manifest_commit_preserves_old_payloads_and_orphans_never_restore_or_reuse_stale_bytes() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("committed"));
    let committed = history.entries()[0].id;
    let manifest = temporary.path().join(SNAPSHOT);
    let backup = temporary.path().join("saved-manifest");
    let committed_manifest = fs::read(&manifest).unwrap();
    fs::rename(&manifest, &backup).unwrap();
    fs::create_dir(&manifest).unwrap(); // Make only the manifest replacement fail.
    history.remove(committed);
    assert!(history.error().is_some());
    assert!(temporary.path().join(payload_file(committed)).exists());
    history.capture(text("uncommitted orphan"));
    let orphan_id = history.entries()[0].id;
    assert!(history.error().is_some());
    assert!(temporary.path().join(payload_file(orphan_id)).exists());
    assert!(temporary.path().join(payload_file(committed)).exists());
    let orphan_path = temporary.path().join(payload_file(orphan_id));
    let stale_generation = fs::read(&orphan_path).unwrap();
    assert_eq!(fs::read(&backup).unwrap(), committed_manifest);
    fs::remove_dir(&manifest).unwrap();
    fs::rename(&backup, &manifest).unwrap();
    drop(history); // Simulate exit after payload creation but before manifest commit.

    let mut history = open(temporary.path());
    assert_eq!(history.entries().len(), 1);
    assert_eq!(history.entries()[0].id, committed);
    assert_eq!(history.entries()[0].payload, text("committed"));
    history.capture(text("fresh bytes using the uncommitted id"));
    assert_eq!(history.entries()[0].id, orphan_id);
    assert_eq!(history.error(), None);
    history.remove(committed);
    drop(history);
    let history = open(temporary.path());
    assert_eq!(history.entries().len(), 1);
    assert_eq!(
        history.entries()[0].payload,
        text("fresh bytes using the uncommitted id")
    );
    assert!(!temporary.path().join(payload_file(committed)).exists());
    drop(history);
    let committed_generation = fs::read(&orphan_path).unwrap();
    fs::write(&orphan_path, stale_generation).unwrap();
    let replayed = History::open_with_key(temporary.path().to_owned(), |_| {
        Ok(Zeroizing::new([71; 32]))
    });
    assert!(replayed.error().unwrap().contains("binding"));
    assert!(replayed.entries().is_empty());
    drop(replayed);
    fs::write(orphan_path, committed_generation).unwrap();
}

#[test]
fn orphan_only_storage_requires_existing_key_but_never_restores_orphans() {
    let temporary = TempDir::new().unwrap();
    let history = open(temporary.path());
    let storage = history.storage.as_ref().unwrap();
    storage
        .write_encrypted(
            &payload_file(1),
            PAYLOAD_HEADER,
            &payload_associated(1),
            Zeroizing::new(b"crashed before first manifest".to_vec()),
        )
        .unwrap();
    drop(history);
    let unavailable = History::open_with_key(temporary.path().to_owned(), |existing| {
        assert!(
            existing,
            "orphaned ciphertext must also forbid replacing a missing key"
        );
        Err("key unavailable".to_owned())
    });
    assert!(unavailable.error().is_some());
    drop(unavailable);
    let mut history = open(temporary.path());
    assert!(history.entries().is_empty());
    history.capture(text("new committed payload"));
    history.remove(history.entries()[0].id);
    assert_eq!(history.error(), None);
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
    drop(history);
    assert!(open(temporary.path()).entries().is_empty());
}

#[test]
fn payload_tamper_swapping_and_missing_files_are_detected_without_overwriting_manifest() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("first"));
    let first = history.entries()[0].id;
    history.capture(text("other"));
    let other = history.entries()[0].id;
    drop(history);
    let manifest = fs::read(temporary.path().join(SNAPSHOT)).unwrap();
    let first_path = temporary.path().join(payload_file(first));
    let other_path = temporary.path().join(payload_file(other));
    let first_bytes = fs::read(&first_path).unwrap();
    let other_bytes = fs::read(&other_path).unwrap();
    for changed in [
        {
            let mut bytes = first_bytes.clone();
            *bytes.last_mut().unwrap() ^= 1;
            bytes
        },
        other_bytes,
    ] {
        fs::write(&first_path, &changed).unwrap();
        let mut damaged = History::open_with_key(temporary.path().to_owned(), |_| {
            Ok(Zeroizing::new([71; 32]))
        });
        assert!(damaged.entries().is_empty());
        assert!(damaged.error().unwrap().contains("authentication"));
        damaged.capture(text("memory only"));
        assert_eq!(fs::read(temporary.path().join(SNAPSHOT)).unwrap(), manifest);
        assert_eq!(fs::read(&first_path).unwrap(), changed);
    }
    fs::remove_file(&first_path).unwrap();
    let damaged = History::open_with_key(temporary.path().to_owned(), |_| {
        Ok(Zeroizing::new([71; 32]))
    });
    assert!(damaged.error().unwrap().contains("missing"));
    assert_eq!(fs::read(temporary.path().join(SNAPSHOT)).unwrap(), manifest);
}

#[test]
fn installed_manifest_remains_readable_after_sync_failure_and_failed_retry() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("history");
    let mut history = open(&directory);
    history.capture(text("first committed"));
    history.storage.as_mut().unwrap().fail_directory_sync_after = Some(1);
    history.capture(text("installed before directory sync failed"));
    let second = history.entries()[0].id;
    assert!(history.error().unwrap().contains("synchronization"));
    let payload_path = directory.join(payload_file(second));
    let committed_payload = fs::read(&payload_path).unwrap();
    let manifest = directory.join(SNAPSHOT);
    let saved_manifest = temporary.path().join("installed-manifest");
    fs::rename(&manifest, &saved_manifest).unwrap();
    fs::create_dir(&manifest).unwrap();
    history.toggle_pin(second);
    assert!(history.error().is_some());
    assert_eq!(fs::read(&payload_path).unwrap(), committed_payload);
    fs::remove_dir(&manifest).unwrap();
    fs::rename(saved_manifest, manifest).unwrap();
    drop(history);

    let history = open(&directory);
    assert_eq!(history.entries().len(), 2);
    assert_eq!(history.entries()[0].id, second);
    assert!(
        !history.entries()[0].pinned,
        "failed retry must not replace the manifest"
    );
    assert_eq!(
        history.entries()[0].payload,
        text("installed before directory sync failed")
    );
}

#[test]
fn latin1_string_preserves_bytes_without_deduplicating_different_utf8_text() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    let latin1 = Payload {
        mime: "STRING".to_owned(),
        bytes: Arc::from(&b"\xe9"[..]),
        sensitive: false,
    };
    assert!(history.capture(latin1.clone()));
    assert!(!history.capture(latin1.clone()));
    let looks_utf8 = Payload {
        mime: "STRING".to_owned(),
        bytes: Arc::from(&b"\xc3\xa9"[..]),
        sensitive: false,
    };
    assert!(history.capture(looks_utf8.clone()));
    assert!(history.capture(text("é")));
    assert_eq!(history.entries().len(), 3);
    assert!(!history.capture(Payload {
        mime: "STRING".to_owned(),
        bytes: Arc::from(&b" \xa0\t"[..]),
        sensitive: false,
    }));
    let expected = history.entries().to_vec();
    drop(history);
    assert_eq!(open(temporary.path()).entries(), expected);

    let mut history = History::memory();
    assert!(history.capture(Payload {
        mime: "STRING".to_owned(),
        bytes: Arc::from(&b"ascii"[..]),
        sensitive: false,
    }));
    assert!(
        !history.capture(text("ascii")),
        "ASCII has equivalent legacy and UTF-8 encoding"
    );
    assert_eq!(history.entries().len(), 1);
    assert!(history.capture(Payload {
        mime: "TEXT".to_owned(),
        bytes: Arc::from("é".as_bytes()),
        sensitive: false,
    }));
    assert!(
        history.capture(text("é")),
        "TEXT does not guarantee UTF-8 encoding"
    );
}

#[test]
fn archive_releases_writer_lock_even_with_duplicate_descriptor() {
    let temporary = TempDir::new().unwrap();
    let mut history = open(temporary.path());
    history.capture(text("committed before closing"));
    // A concurrent fork inherits this same open-file description until exec.
    let inherited = history
        .storage
        .as_ref()
        .unwrap()
        ._directory
        .0
        .try_clone()
        .unwrap();
    drop(history);
    let reopened = open(temporary.path());
    assert_eq!(
        reopened.entries()[0].payload,
        text("committed before closing")
    );
    let contender = History::open_with_key(temporary.path().to_owned(), |_| {
        Ok(Zeroizing::new([71; 32]))
    });
    assert!(contender.error().unwrap().contains("already open"));
    drop(inherited);
}

#[test]
fn key_initialization_is_exclusive_across_storage_directories_without_user_keyring() {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    struct PrivateBus(Child);
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temporary = TempDir::new().unwrap();
    let mut bus = PrivateBus(
        Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                temporary.path().join("bus").display()
            ))
            .stdout(Stdio::piped())
            .spawn()
            .expect("ownership regression requires dbus-daemon"),
    );
    let mut address = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    futures_lite::future::block_on(async {
        let owner = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let contender = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        lock_key_acquisition(&owner).await.unwrap();
        let attempted_key_write = std::cell::Cell::new(false);
        let denied = History::open_with_key(temporary.path().join("other-state-directory"), |_| {
            futures_lite::future::block_on(async {
                lock_key_acquisition(&contender)
                    .await
                    .map_err(|error| error.to_string())?;
                attempted_key_write.set(true);
                Ok(Zeroizing::new([99; 32]))
            })
        });
        assert!(
            !attempted_key_write.get(),
            "another state directory must not fill an empty key item"
        );
        assert!(denied.error().is_some());
        drop(denied);
        // DoNotQueue means the failed contender is not silently promoted after release.
        assert!(owner.release_name(KEY_ACQUISITION_NAME).await.unwrap());
        let next = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        lock_key_acquisition(&next).await.unwrap();
        assert!(lock_key_acquisition(&contender).await.is_err());
        next.release_name(KEY_ACQUISITION_NAME).await.unwrap();
        lock_key_acquisition(&contender).await.unwrap();
        contender.release_name(KEY_ACQUISITION_NAME).await.unwrap();
        owner.close().await.unwrap();
        contender.close().await.unwrap();
        next.close().await.unwrap();
    });
}
