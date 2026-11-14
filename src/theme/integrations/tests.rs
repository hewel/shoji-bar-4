use super::super::matugen::ThemeFiles;
use super::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct DesktopState {
    values: BTreeMap<String, String>,
    fail_once: Option<String>,
    writes: usize,
}
struct FakeDesktop(Arc<Mutex<DesktopState>>);
impl Desktop for FakeDesktop {
    fn get(&mut self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().values.get(key).cloned())
    }
    fn set(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.fail_once.as_deref() == Some(key) {
            state.fail_once = None;
            return Err("Injected desktop write failure".into());
        }
        state.writes += 1;
        if let Some(value) = value {
            state.values.insert(key.into(), value.into());
        } else {
            state.values.remove(key);
        }
        Ok(())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    paths: Paths,
    desktop: Arc<Mutex<DesktopState>>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let paths = Paths {
            config: home.join(".config"),
            state: home.join(".local/state"),
            data: home.join(".local/share"),
            home,
        };
        fs::create_dir_all(&paths.config).unwrap();
        fs::create_dir_all(&paths.state).unwrap();
        fs::create_dir_all(&paths.data).unwrap();
        let values = BTreeMap::from([
            ("gtk-theme".into(), "'Colloid'".into()),
            ("color-scheme".into(), "'default'".into()),
        ]);
        Self {
            _temp: temp,
            paths,
            desktop: Arc::new(Mutex::new(DesktopState {
                values,
                ..DesktopState::default()
            })),
        }
    }
    fn manager(&self) -> IntegrationManager {
        IntegrationManager {
            paths: self.paths.clone(),
            desktop: Box::new(FakeDesktop(self.desktop.clone())),
        }
    }
    fn themes(&self) {
        for name in ["adw-gtk3", "adw-gtk3-dark"] {
            put(
                &self
                    .paths
                    .data
                    .join("themes")
                    .join(name)
                    .join("gtk-3.0/gtk.css"),
                "/* installed test theme */",
            );
        }
    }
    fn profile(&self) -> PathBuf {
        let root = self.paths.config.join("zen");
        let profile = root.join("release");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir_all(root.join("old")).unwrap();
        put(
            &root.join("profiles.ini"),
            "[Profile0]\nName=old\nIsRelative=1\nPath=old\nDefault=1\n\n[Profile1]\nName=release\nIsRelative=1\nPath=release\n\n[InstallABC]\nDefault=release\nLocked=1\n",
        );
        profile
    }
}
fn put(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn text(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}
fn theme(dark: bool) -> GeneratedTheme {
    GeneratedTheme {
        dark,
        source_color: 0xff6750a4,
        colors: BTreeMap::new(),
        files: ThemeFiles {
            gtk3: format!("/* gtk3 {dark} */"),
            gtk4: format!("/* gtk4 {dark} */"),
            zen: format!("/* zen {dark} */"),
        },
    }
}

#[test]
fn disabled_unmanaged_integrations_are_pure_noops_without_palette() {
    let fixture = Fixture::new();
    let original = "[Settings]\ngtk-theme-name=Colloid\n";
    put(&fixture.paths.config.join("gtk-4.0/settings.ini"), original);
    let report = fixture.manager().sync(false, false, None);
    assert!(report.gtk.unwrap().contains("Not linked"));
    assert!(report.zen.unwrap().contains("Not linked"));
    assert_eq!(
        text(&fixture.paths.config.join("gtk-4.0/settings.ini")),
        original
    );
    assert!(!fixture.paths.state.join("shoji-bar-4").exists());
    assert_eq!(fixture.desktop.lock().unwrap().writes, 0);
}

#[test]
fn gtk_symlinks_restore_targets_and_unrelated_user_additions_survive_updates() {
    let fixture = Fixture::new();
    fixture.themes();
    let target = fixture.paths.home.join("Colloid/gtk.css");
    put(&target, "/* original Colloid target */");
    let assets = fixture.paths.home.join("Colloid/assets");
    fs::create_dir_all(&assets).unwrap();
    let gtk4 = fixture.paths.config.join("gtk-4.0");
    fs::create_dir_all(&gtk4).unwrap();
    for name in ["gtk.css", "gtk-dark.css"] {
        symlink(&target, gtk4.join(name)).unwrap();
    }
    symlink(&assets, gtk4.join("assets")).unwrap();
    let gtk3 = fixture.paths.config.join("gtk-3.0");
    let original_settings = "# keep me\n[Settings]\ngtk-theme-name=Colloid # base comment\ngtk-font-name=Sans 11\ngtk-icon-theme-name=Papirus\ngtk-cursor-theme-name=Bibata\n";
    put(&gtk3.join("settings.ini"), original_settings);
    put(&gtk3.join("gtk.css"), "/* original customization */\n");
    let mut manager = fixture.manager();
    manager.sync(true, false, Some(&theme(true))).gtk.unwrap();
    assert!(
        !fs::symlink_metadata(gtk4.join("gtk.css"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!gtk4.join("gtk-dark.css").exists());
    assert!(text(&gtk3.join("settings.ini")).contains("gtk-font-name=Sans 11"));
    assert!(text(&gtk3.join("settings.ini")).contains("# base comment"));
    let additions = "gtk-decoration-layout=menu:close\n";
    put(
        &gtk3.join("settings.ini"),
        &(text(&gtk3.join("settings.ini")) + additions),
    );
    put(
        &gtk3.join("gtk.css"),
        &(text(&gtk3.join("gtk.css")) + "/* user addition */\n"),
    );
    manager.sync(true, false, Some(&theme(false))).gtk.unwrap();
    assert_eq!(text(&gtk4.join("shoji-colors.css")), "/* gtk4 false */");
    manager.sync(false, false, None).gtk.unwrap();
    assert_eq!(fs::read_link(gtk4.join("gtk.css")).unwrap(), target);
    assert_eq!(fs::read_link(gtk4.join("gtk-dark.css")).unwrap(), target);
    assert_eq!(fs::read_link(gtk4.join("assets")).unwrap(), assets);
    assert_eq!(text(&target), "/* original Colloid target */");
    assert_eq!(
        text(&gtk3.join("settings.ini")),
        format!("{original_settings}{additions}")
    );
    assert_eq!(
        text(&gtk3.join("gtk.css")),
        "/* original customization */\n/* user addition */\n"
    );
    assert!(!gtk4.join("shoji-colors.css").exists());
    assert_eq!(
        fixture.desktop.lock().unwrap().values["gtk-theme"],
        "'Colloid'"
    );
    assert_eq!(
        fixture.desktop.lock().unwrap().values["color-scheme"],
        "'default'"
    );
}

#[test]
fn foreign_owned_file_or_preference_changes_block_updates_and_disable() {
    let fixture = Fixture::new();
    fixture.themes();
    let mut manager = fixture.manager();
    manager.sync(true, false, Some(&theme(true))).gtk.unwrap();
    let owned = fixture.paths.config.join("gtk-3.0/shoji-colors.css");
    put(&owned, "/* user edited generated output */");
    let settings = text(&fixture.paths.config.join("gtk-3.0/settings.ini"));
    assert!(
        manager
            .sync(true, false, Some(&theme(false)))
            .gtk
            .unwrap_err()
            .contains("changed outside")
    );
    assert!(manager.sync(false, false, None).gtk.is_err());
    assert_eq!(text(&owned), "/* user edited generated output */");
    assert_eq!(
        text(&fixture.paths.config.join("gtk-3.0/settings.ini")),
        settings
    );
    put(&owned, &theme(true).files.gtk3);
    fixture
        .desktop
        .lock()
        .unwrap()
        .values
        .insert("color-scheme".into(), "'prefer-light'".into());
    assert!(
        manager
            .sync(false, false, None)
            .gtk
            .unwrap_err()
            .contains("Desktop preference")
    );
    assert!(fixture.paths.manifest("gtk").exists());
}

#[test]
fn gtk_output_collision_and_desktop_failure_do_not_partially_enable() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager();
    // The fixture's user theme is deterministic even on machines with a system adw theme:
    // output collision is independently checked before any configuration is committed.
    fixture.themes();
    let original = "[Settings]\ngtk-icon-theme-name=Papirus\n";
    let settings = fixture.paths.config.join("gtk-3.0/settings.ini");
    put(&settings, original);
    let output = fixture.paths.config.join("gtk-4.0/shoji-colors.css");
    put(&output, "foreign collision");
    assert!(
        manager
            .sync(true, false, Some(&theme(true)))
            .gtk
            .unwrap_err()
            .contains("already exists")
    );
    assert_eq!(text(&settings), original);
    assert_eq!(text(&output), "foreign collision");
    assert!(!fixture.paths.manifest("gtk").exists());
    fs::remove_file(&output).unwrap();
    fixture.desktop.lock().unwrap().fail_once = Some("color-scheme".into());
    assert!(
        manager
            .sync(true, false, Some(&theme(true)))
            .gtk
            .unwrap_err()
            .contains("rolled back")
    );
    assert_eq!(text(&settings), original);
    assert!(!fixture.paths.config.join("gtk-3.0/gtk.css").exists());
    assert_eq!(
        fixture.desktop.lock().unwrap().values["gtk-theme"],
        "'Colloid'"
    );
    let record: Manifest = serde_json::from_str(&text(&fixture.paths.manifest("gtk"))).unwrap();
    assert!(record.pending.is_none());
    manager.sync(true, false, Some(&theme(true))).gtk.unwrap();
    manager.sync(false, false, None).gtk.unwrap();
    assert_eq!(text(&settings), original);
}

#[test]
fn zen_uses_install_default_and_retains_safe_effective_preference_restoration() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let chrome = profile.join("chrome/userChrome.css");
    let original_css = "/* user customization */\n#urlbar { opacity: .9; }\n";
    put(&chrome, original_css);
    let original_user =
        "// unrelated user preference\nuser_pref(\"dom.screenwakelock.enabled\", true);\n";
    put(&profile.join("user.js"), original_user);
    let prefs = "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n";
    put(&profile.join("prefs.js"), prefs);
    let mut manager = fixture.manager();
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    assert!(text(&chrome).starts_with("/* shoji-zen-colors begin */\n@import"));
    assert!(!fixture.paths.config.join("zen/old/user.js").exists());
    assert!(!profile.join("chrome/userContent.css").exists());
    assert!(!profile.join("chrome/zen-themes.css").exists());
    put(
        &profile.join("user.js"),
        &(text(&profile.join("user.js")) + "user_pref(\"browser.tabs.warnOnClose\", true);\n"),
    );
    manager.sync(false, true, Some(&theme(false))).zen.unwrap();
    let disabled = manager.sync(false, false, None).zen.unwrap();
    assert!(disabled.contains("next Zen start"));
    assert_eq!(text(&chrome), original_css);
    let restored = text(&profile.join("user.js"));
    assert!(restored.starts_with(original_user));
    assert!(restored.contains("browser.tabs.warnOnClose"));
    assert_eq!(zen::preference(&restored).unwrap(), Some(false));
    assert_eq!(text(&profile.join("prefs.js")), prefs);
    assert!(fixture.paths.manifest("zen").exists());
    manager.sync(false, false, None).zen.unwrap();
    assert_eq!(text(&profile.join("user.js")), restored);
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    manager.sync(false, false, None).zen.unwrap();
    assert_eq!(text(&profile.join("user.js")), restored);
}

#[test]
fn zen_existing_boolean_directives_restore_exactly_and_later_overrides_conflict() {
    for prior in [false, true] {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let user = profile.join("user.js");
        let original = format!(
            "// user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", true);\nuser_pref(\n /* keep comment */ \"toolkit.legacyUserProfileCustomizations.stylesheets\", {prior}\n);\nuser_pref(\"dom.screenwakelock.enabled\", true);\n"
        );
        put(&user, &original);
        let mut manager = fixture.manager();
        manager.sync(false, true, Some(&theme(true))).zen.unwrap();
        let managed = text(&user);
        put(
            &user,
            &(managed.clone()
                + "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n"),
        );
        assert!(manager.sync(false, false, None).zen.is_err());
        assert!(text(&user).ends_with("false);\n"));
        put(&user, &managed);
        manager.sync(false, false, None).zen.unwrap();
        assert_eq!(text(&user), original);
        assert!(!fixture.paths.manifest("zen").exists());
    }
}

#[test]
fn zen_ambiguous_install_defaults_reject_and_legacy_registry_fallback_works() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let registry = fixture.paths.config.join("zen/profiles.ini");
    put(
        &registry,
        &(text(&registry) + "\n[InstallOTHER]\nDefault=old\n"),
    );
    assert!(
        fixture
            .manager()
            .sync(false, true, Some(&theme(true)))
            .zen
            .unwrap_err()
            .contains("Ambiguous")
    );
    assert!(!profile.join("user.js").exists());
    assert!(!fixture.paths.manifest("zen").exists());
    fs::remove_file(registry).unwrap();
    let legacy = fixture.paths.home.join(".zen/default");
    fs::create_dir_all(&legacy).unwrap();
    put(
        &fixture.paths.home.join(".zen/profiles.ini"),
        "[Profile0]\nPath=default\nIsRelative=1\nDefault=1\n",
    );
    assert_eq!(zen::discover(&fixture.paths).unwrap(), legacy);
}

#[test]
fn pending_journal_recovers_crash_but_foreign_changes_preserve_recovery_record() {
    let fixture = Fixture::new();
    let first = fixture.paths.config.join("gtk-3.0/gtk.css");
    let second = fixture.paths.config.join("gtk-4.0/gtk.css");
    let original = file_text("/* original */".into(), &Snapshot::Absent);
    let generated = file_text("/* generated */".into(), &Snapshot::Absent);
    replace(&first, &generated).unwrap();
    replace(&second, &original).unwrap();
    let mut record = Manifest::new("gtk");
    record.pending = Some(Journal {
        files: vec![
            Change {
                path: first.clone(),
                before: original.clone(),
                after: generated.clone(),
            },
            Change {
                path: second.clone(),
                before: original.clone(),
                after: generated,
            },
        ],
        desktop: Vec::new(),
    });
    save_json(&fixture.paths.manifest("gtk"), &record).unwrap();
    put(&second, "/* foreign edit */");
    assert!(fixture.manager().sync(false, false, None).gtk.is_err());
    assert_eq!(text(&first), "/* generated */");
    assert_eq!(text(&second), "/* foreign edit */");
    assert!(fixture.paths.manifest("gtk").exists());
    replace(&second, &original).unwrap();
    fixture.manager().sync(false, false, None).gtk.unwrap();
    assert_eq!(text(&first), "/* original */");
    assert_eq!(text(&second), "/* original */");
    assert!(!fixture.paths.manifest("gtk").exists());
}

#[test]
fn parent_symlinks_and_stale_manifests_are_rejected_without_touching_targets() {
    let fixture = Fixture::new();
    fixture.themes();
    let target = fixture.paths.home.join("unrelated");
    fs::create_dir_all(&target).unwrap();
    symlink(&target, fixture.paths.config.join("gtk-4.0")).unwrap();
    assert!(
        fixture
            .manager()
            .sync(true, false, Some(&theme(true)))
            .gtk
            .unwrap_err()
            .contains("parent")
    );
    assert!(!target.join("settings.ini").exists());
    assert!(!fixture.paths.config.join("gtk-3.0/settings.ini").exists());
    fs::remove_file(fixture.paths.config.join("gtk-4.0")).unwrap();
    let mut stale = Manifest::new("gtk");
    stale.version = VERSION + 1;
    save_json(&fixture.paths.manifest("gtk"), &stale).unwrap();
    let before = text(&fixture.paths.manifest("gtk"));
    assert!(
        fixture
            .manager()
            .sync(false, false, None)
            .gtk
            .unwrap_err()
            .contains("Unsupported")
    );
    assert_eq!(text(&fixture.paths.manifest("gtk")), before);
}

#[test]
fn linked_text_configuration_is_copied_without_writing_targets_and_restored() {
    let fixture = Fixture::new();
    fixture.themes();
    let profile = fixture.profile();
    let settings_target = fixture.paths.home.join("dotfiles/settings.ini");
    let chrome_target = fixture.paths.home.join("dotfiles/userChrome.css");
    let user_target = fixture.paths.home.join("dotfiles/user.js");
    let original_settings = "[Settings]\ngtk-theme-name=Old\ngtk-font-name=Sans 11\n";
    let original_chrome = "/* original dotfile chrome */\n";
    let original_user =
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n";
    put(&settings_target, original_settings);
    put(&chrome_target, original_chrome);
    put(&user_target, original_user);
    let settings = fixture.paths.config.join("gtk-3.0/settings.ini");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::create_dir_all(profile.join("chrome")).unwrap();
    symlink(&settings_target, &settings).unwrap();
    symlink(&chrome_target, profile.join("chrome/userChrome.css")).unwrap();
    symlink(&user_target, profile.join("user.js")).unwrap();
    let mut manager = fixture.manager();
    let report = manager.sync(true, true, Some(&theme(true)));
    report.gtk.unwrap();
    report.zen.unwrap();
    assert!(text(&profile.join("chrome/userChrome.css")).contains(original_chrome));
    let report = manager.sync(true, true, Some(&theme(false)));
    report.gtk.unwrap();
    report.zen.unwrap();
    let report = manager.sync(false, false, None);
    report.gtk.unwrap();
    report.zen.unwrap();
    assert_eq!(fs::read_link(&settings).unwrap(), settings_target);
    assert_eq!(
        fs::read_link(profile.join("chrome/userChrome.css")).unwrap(),
        chrome_target
    );
    assert_eq!(fs::read_link(profile.join("user.js")).unwrap(), user_target);
    assert_eq!(text(&settings_target), original_settings);
    assert_eq!(text(&chrome_target), original_chrome);
    assert_eq!(text(&user_target), original_user);
}

#[test]
fn escaped_boolean_preference_is_restored_without_touching_unrelated_script() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let user = profile.join("user.js");
    let original = r#"// An escaped key is still the same effective browser preference.
user_pref("\u0074oolkit.legacyUserProfileCustomizations.stylesheets", false);
user_pref("browser.example", "quoted \u1234 and \\ paths");
"#;
    put(&user, original);
    let mut manager = fixture.manager();
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    manager.sync(false, false, None).zen.unwrap();
    assert_eq!(text(&user), original);
    assert!(!fixture.paths.manifest("zen").exists());
}

#[test]
fn xdg_install_default_ignores_old_profile_flags_and_legacy_registry() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let registry = fixture.paths.config.join("zen/profiles.ini");
    put(
        &registry,
        "[Profile0]\nPath=../retired\nIsRelative=1\nDefault=1\n[InstallABC]\nDefault=release\n",
    );
    let legacy = fixture.paths.home.join(".zen/old");
    fs::create_dir_all(&legacy).unwrap();
    put(
        &fixture.paths.home.join(".zen/profiles.ini"),
        "[InstallOLD]\nDefault=old\n",
    );
    assert_eq!(zen::discover(&fixture.paths).unwrap(), profile);
    fixture
        .manager()
        .sync(false, true, Some(&theme(true)))
        .zen
        .unwrap();
    assert!(!legacy.join("user.js").exists());
    assert!(!fixture.paths.config.join("retired/user.js").exists());
}

#[test]
fn multi_megabyte_stylesheet_can_be_disabled_and_recovered_after_restart() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let chrome = profile.join("chrome/userChrome.css");
    let original = format!("/* {} */\n", "x".repeat(2 * 1024 * 1024));
    put(&chrome, &original);
    fixture
        .manager()
        .sync(false, true, Some(&theme(true)))
        .zen
        .unwrap();
    assert!(fs::metadata(fixture.paths.manifest("zen")).unwrap().len() > MAX_FILE);
    // Simulate a crash after an update's durable journal and first file replacement.
    let mut record: Manifest = serde_json::from_str(&text(&fixture.paths.manifest("zen"))).unwrap();
    let before = snapshot(&chrome).unwrap();
    let interrupted = file_text("/* interrupted update */".into(), &before);
    record.pending = Some(Journal {
        files: vec![Change {
            path: chrome.clone(),
            before,
            after: interrupted.clone(),
        }],
        desktop: Vec::new(),
    });
    save_json(&fixture.paths.manifest("zen"), &record).unwrap();
    replace(&chrome, &interrupted).unwrap();
    fixture.manager().sync(false, false, None).zen.unwrap();
    assert_eq!(text(&chrome), original);
}

#[test]
fn oversized_recovery_or_committed_record_rejects_before_any_configuration_write() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let chrome = profile.join("chrome/userChrome.css");
    let original = format!("/* {} */\n", "x".repeat(3 * 1024 * 1024));
    put(&chrome, &original);
    let error = fixture
        .manager()
        .sync(false, true, Some(&theme(true)))
        .zen
        .unwrap_err();
    assert!(error.contains("recovery record"), "{error}");
    assert_eq!(text(&chrome), original);
    assert!(!profile.join("user.js").exists());
    assert!(!profile.join("chrome/shoji-colors.css").exists());
    assert!(!fixture.paths.manifest("zen").exists());
}

#[test]
fn unset_desktop_overrides_remain_unset_after_disable_and_failed_enable() {
    let fixture = Fixture::new();
    fixture.themes();
    fixture.desktop.lock().unwrap().values.clear();
    fixture.desktop.lock().unwrap().fail_once = Some("color-scheme".into());
    let mut manager = fixture.manager();
    assert!(manager.sync(true, false, Some(&theme(true))).gtk.is_err());
    assert!(fixture.desktop.lock().unwrap().values.is_empty());
    manager.sync(true, false, Some(&theme(true))).gtk.unwrap();
    assert_eq!(
        fixture.desktop.lock().unwrap().values["color-scheme"],
        "'prefer-dark'"
    );
    manager.sync(false, false, None).gtk.unwrap();
    assert!(fixture.desktop.lock().unwrap().values.is_empty());
}

#[test]
fn changed_persisted_zen_preference_conflicts_without_overwriting_newer_intent() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let prefs = profile.join("prefs.js");
    put(
        &prefs,
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", true);\n",
    );
    let mut manager = fixture.manager();
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    let user = text(&profile.join("user.js"));
    let css = text(&profile.join("chrome/shoji-colors.css"));
    put(
        &prefs,
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n",
    );
    assert!(
        manager
            .sync(false, true, Some(&theme(false)))
            .zen
            .unwrap_err()
            .contains("prefs.js")
    );
    assert!(
        manager
            .sync(false, false, None)
            .zen
            .unwrap_err()
            .contains("prefs.js")
    );
    assert_eq!(text(&profile.join("user.js")), user);
    assert_eq!(text(&profile.join("chrome/shoji-colors.css")), css);
    assert_eq!(zen::preference(&text(&prefs)).unwrap(), Some(false));
}

#[test]
fn zen_allows_initial_persistence_but_detects_later_reversal() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let prefs = profile.join("prefs.js");
    put(
        &prefs,
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n",
    );
    let mut manager = fixture.manager();
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    manager.sync(false, true, Some(&theme(false))).zen.unwrap(); // Browser not restarted yet.
    put(
        &prefs,
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", true);\n",
    );
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    put(
        &prefs,
        "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", false);\n",
    );
    assert!(manager.sync(false, false, None).zen.is_err());
    assert!(profile.join("chrome/shoji-colors.css").exists());
}

#[test]
fn disabled_profile_restoration_does_not_block_a_new_default_and_stays_owned() {
    let fixture = Fixture::new();
    let old = fixture.profile();
    let mut manager = fixture.manager();
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    let new = fixture.paths.config.join("zen/new-default");
    fs::create_dir_all(&new).unwrap();
    put(
        &fixture.paths.config.join("zen/profiles.ini"),
        "[InstallABC]\nDefault=new-default\n",
    );
    assert!(manager.sync(false, true, Some(&theme(false))).zen.is_err());
    manager.sync(false, false, None).zen.unwrap();
    let restored_old = text(&old.join("user.js"));
    assert_eq!(zen::preference(&restored_old).unwrap(), Some(false));
    manager.sync(false, true, Some(&theme(false))).zen.unwrap();
    assert_eq!(text(&old.join("user.js")), restored_old);
    assert!(new.join("chrome/shoji-colors.css").exists());
    manager.sync(false, false, None).zen.unwrap();
    assert!(!new.join("chrome/shoji-colors.css").exists());
    assert_eq!(
        zen::preference(&text(&new.join("user.js"))).unwrap(),
        Some(false)
    );
    manager.sync(false, true, Some(&theme(true))).zen.unwrap();
    put(
        &old.join("user.js"),
        &restored_old.replace("false);", "true);"),
    );
    assert!(manager.sync(false, false, None).zen.is_err());
    assert!(new.join("chrome/shoji-colors.css").exists());
    assert_eq!(
        zen::preference(&text(&old.join("user.js"))).unwrap(),
        Some(true)
    );
}

#[test]
fn profile_outside_configuration_recovery_roots_is_rejected_before_enable() {
    let mut fixture = Fixture::new();
    fixture.paths.state = fixture._temp.path().join("external-state");
    let external = fixture.paths.state.join("browser-profile");
    fs::create_dir_all(&external).unwrap();
    put(
        &fixture.paths.config.join("zen/profiles.ini"),
        &format!("[InstallABC]\nDefault={}\n", external.display()),
    );
    let error = fixture
        .manager()
        .sync(false, true, Some(&theme(true)))
        .zen
        .unwrap_err();
    assert!(error.contains("scope") || error.contains("root"), "{error}");
    assert!(!external.join("user.js").exists());
    assert!(!external.join("chrome").exists());
    assert!(!fixture.paths.manifest("zen").exists());
}

