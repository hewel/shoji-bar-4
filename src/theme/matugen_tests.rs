use super::*;
use serde_json::json;

pub(super) fn fixture() -> GeneratedTheme {
    let roles = [
        "primary",
        "on_primary",
        "primary_container",
        "on_primary_container",
        "secondary",
        "on_secondary",
        "secondary_container",
        "on_secondary_container",
        "tertiary",
        "on_tertiary",
        "tertiary_container",
        "on_tertiary_container",
        "error",
        "on_error",
        "error_container",
        "on_error_container",
        "surface",
        "surface_dim",
        "surface_bright",
        "surface_variant",
        "on_surface",
        "on_surface_variant",
        "inverse_surface",
        "inverse_on_surface",
        "inverse_primary",
        "scrim",
        "surface_container_lowest",
        "surface_container_low",
        "surface_container",
        "surface_container_high",
        "surface_container_highest",
        "outline",
        "outline_variant",
        "source_color",
        "success",
        "on_success",
        "warning",
        "on_warning",
    ];
    let colors: BTreeMap<_, _> = roles
        .into_iter()
        .map(|role| (role.to_owned(), 0x123456))
        .collect();
    GeneratedTheme {
        dark: true,
        source_color: 0x123456,
        files: ThemeFiles {
            gtk3: expected_template(TEMPLATES[0].1, &colors, true).unwrap(),
            gtk4: expected_template(TEMPLATES[1].1, &colors, true).unwrap(),
            zen: expected_template(TEMPLATES[2].1, &colors, true).unwrap(),
        },
        colors,
    }
}

#[test]
fn palette_parser_rejects_mode_mismatch_and_nonopaque_colors() {
    let mut value = json!({
        "mode": "dark", "is_dark_mode": true,
        "colors": { "source_color": {
            "default": { "color": "#123456" },
            "dark": { "color": "#123456" },
            "light": { "color": "#abcdef" }
        }}
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(parse_palette(&bytes, true).unwrap().0, 0x123456);
    assert!(parse_palette(&bytes, false).is_err());
    value["colors"]["source_color"]["default"]["color"] = json!("#abcdef");
    assert!(parse_palette(&serde_json::to_vec(&value).unwrap(), true).is_err());
    value["colors"]["source_color"]["default"]["color"] = json!("#123456");
    value["colors"]["source_color"]["light"]["color"] = json!("#abcdef80");
    assert!(parse_palette(&serde_json::to_vec(&value).unwrap(), true).is_err());
}

#[test]
fn incomplete_or_injected_theme_cannot_replace_last_good_cache() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("last-good.json");
    assert!(load_cache(&path).unwrap().is_none());
    let original = fixture();
    prepare_cache(&path, &original)
        .unwrap()
        .commit()
        .unwrap()
        .sync()
        .unwrap();
    let good_bytes = fs::read(&path).unwrap();
    let mut incomplete = original.clone();
    incomplete.colors.remove("scrim");
    assert!(incomplete.scheme().is_err());
    assert!(prepare_cache(&path, &incomplete).is_err());
    assert_eq!(fs::read(&path).unwrap(), good_bytes);
    let mut injected = original;
    injected
        .files
        .zen
        .push_str("\n@import url(https://example.invalid/injected.css);\n");
    assert!(prepare_cache(&path, &injected).is_err());
    assert_eq!(fs::read(&path).unwrap(), good_bytes);
    assert_eq!(load_cache(&path).unwrap().unwrap().source_color, 0x123456);
}

#[test]
fn corrupt_or_incompatible_cache_is_an_error_not_a_missing_cache() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("last-good.json");
    fs::write(&path, b"{broken").unwrap();
    assert!(load_cache(&path).is_err());
    let mut value = json!({ "schema_version": CACHE_VERSION + 1, "theme": fixture() });
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(load_cache(&path).is_err());
    value["schema_version"] = json!(CACHE_VERSION);
    value["theme"]["files"]["gtk4"] = json!("truncated stylesheet");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(load_cache(&path).is_err());
}

#[test]
fn cache_rejects_symlinks_and_oversized_files() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.json");
    prepare_cache(&target, &fixture())
        .unwrap()
        .commit()
        .unwrap()
        .sync()
        .unwrap();
    let link = directory.path().join("link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(load_cache(&link).is_err());
    let oversized = directory.path().join("oversized.json");
    File::create(&oversized)
        .unwrap()
        .set_len(CACHE_LIMIT as u64 + 1)
        .unwrap();
    assert!(load_cache(&oversized).is_err());
}

#[test]
fn generation_deadline_includes_descendants_holding_output_pipes() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & exit 0"]);
    let error = command_output(
        &mut command,
        Duration::from_millis(50),
        1024,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("deadline"), "{error}");
}

#[test]
fn noisy_child_is_stopped_instead_of_filling_memory() {
    let mut command = Command::new("sh");
    command.args(["-c", "while :; do printf 'too much output'; done"]);
    let error = command_output(
        &mut command,
        Duration::from_secs(5),
        64,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("size limit"), "{error}");
}

#[test]
fn subprocess_failure_keeps_actionable_bounded_diagnostics() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'unsupported template role' >&2; exit 2"]);
    let error = command_output(
        &mut command,
        Duration::from_secs(5),
        1024,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("unsupported template role"), "{error}");
}

#[test]
fn staged_cache_changes_nothing_until_commit_and_discards_rejected_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("last-good.json");
    let original = fixture();
    prepare_cache(&path, &original)
        .unwrap()
        .commit()
        .unwrap()
        .sync()
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut candidate = original;
    candidate.dark = false;
    candidate.files.zen = expected_template(TEMPLATES[2].1, &candidate.colors, false).unwrap();
    let staged = prepare_cache(&path, &candidate).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(staged);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    prepare_cache(&path, &candidate)
        .unwrap()
        .commit()
        .unwrap()
        .sync()
        .unwrap();
    assert!(!load_cache(&path).unwrap().unwrap().dark);
}

#[test]
fn cancellation_kills_and_reaps_the_owned_child() {
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let (mut ready_reader, ready_writer) = UnixStream::pair().unwrap();
    ready_reader
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut command = Command::new("sleep");
        command.arg("30");
        // SAFETY: this test hook performs only getpid/write syscalls and errno
        // conversion. The socket identifies an actually spawned child without sleeps.
        unsafe {
            command.pre_exec(move || {
                let bytes = rustix::process::getpid().as_raw_pid().to_ne_bytes();
                rustix::io::write(&ready_writer, &bytes)
                    .map(|_| ())
                    .map_err(io::Error::from)
            });
        }
        let result = command_output(
            &mut command,
            Duration::from_secs(5),
            1024,
            &worker_cancelled,
        );
        sender.send(result).unwrap();
    });
    let mut pid = [0u8; 4];
    let ready = ready_reader.read_exact(&mut pid);
    cancelled.store(true, Ordering::Release);
    let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    ready.unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    let pid = rustix::process::Pid::from_raw(i32::from_ne_bytes(pid)).unwrap();
    assert_eq!(
        rustix::process::waitid(
            rustix::process::WaitId::Pid(pid),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG,
        )
        .unwrap_err(),
        rustix::io::Errno::CHILD,
        "cancelled child must already be reaped",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn child_dies_when_its_supervising_thread_exits() {
    use std::sync::mpsc;
    let (sender, receiver) = mpsc::channel();
    let supervisor = std::thread::spawn(move || {
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        supervise(&mut command);
        // Hand ownership to the test so cleanup cannot be mistaken for parent death.
        sender
            .send(Process(command.spawn().unwrap()))
            .unwrap_or_else(|_| panic!("test receiver dropped"));
    });
    let child = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    supervisor.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.status().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "child survived the supervising thread"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(
        status.terminating_signal(),
        Some(rustix::process::Signal::KILL.as_raw())
    );
    // Process::drop signals the still-waitable group and reaps the direct child.
}

/// Exercises the installed generator's config, CLI and template contracts without user hooks.
#[test]
#[ignore = "requires installed Matugen 4.2+; writes only into temporary directories"]
fn installed_matugen_generates_seed_image_and_quoted_config_themes() {
    let cancelled = AtomicBool::new(false);
    for palette in Palette::ALL {
        for dark in [false, true] {
            let theme = generate(
                &Request {
                    source: Source::Color(0x6750a4),
                    palette,
                    dark,
                },
                &cancelled,
            )
            .unwrap();
            assert_eq!(theme.source_color, 0x6750a4);
            theme.validate().unwrap();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let image_path = directory
        .path()
        .join("wallpaper with 'single' and \"double\" quotes.png");
    let image = image::RgbImage::from_pixel(16, 16, image::Rgb([20, 160, 80]));
    image.save(&image_path).unwrap();
    let mut image_source = None;
    for dark in [false, true] {
        let theme = generate(
            &Request {
                source: Source::Image(image_path.clone()),
                palette: Palette::TonalSpot,
                dark,
            },
            &cancelled,
        )
        .unwrap();
        theme.validate().unwrap();
        assert_eq!(
            *image_source.get_or_insert(theme.source_color),
            theme.source_color
        );
    }
    // Exercise actual TOML parsing with names that cannot be shell-quoted by concatenation.
    let quoted = directory.path().join("config 'single' \"double\" \\ slash");
    fs::create_dir(&quoted).unwrap();
    let config = prepare_workspace(&quoted).unwrap();
    let mut command = Command::new("matugen");
    command
        .current_dir(&quoted)
        .env("HOME", &quoted)
        .env("XDG_CONFIG_HOME", quoted.join("config"))
        .env("XDG_CACHE_HOME", quoted.join("cache"))
        .env("XDG_STATE_HOME", quoted.join("state"))
        .env("XDG_DATA_HOME", quoted.join("data"))
        .arg("--config")
        .arg(config)
        .args([
            "--mode",
            "dark",
            "--source-color-index",
            "0",
            "--json",
            "hex",
            "--quiet",
            "color",
            "hex",
            "#6750a4",
        ]);
    let bytes = command_output(&mut command, TIMEOUT, OUTPUT_LIMIT, &cancelled).unwrap();
    let (source_color, colors) = parse_palette(&bytes, true).unwrap();
    GeneratedTheme {
        dark: true,
        source_color,
        colors,
        files: ThemeFiles {
            gtk3: read_text(&quoted.join("gtk3.css"), FILE_LIMIT).unwrap(),
            gtk4: read_text(&quoted.join("gtk4.css"), FILE_LIMIT).unwrap(),
            zen: read_text(&quoted.join("zen.css"), FILE_LIMIT).unwrap(),
        },
    }
    .validate()
    .unwrap();
    println!(
        "Real Matugen: six variants × two modes, two image modes, and quoted config paths passed."
    );
}
