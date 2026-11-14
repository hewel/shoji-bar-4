use super::*;

pub(super) fn plan(
    paths: &Paths,
    manifest: &mut Manifest,
    changes: &mut Vec<Change>,
    theme: &GeneratedTheme,
    desktop: &mut dyn Desktop,
) -> Result<()> {
    let theme_name = if theme.dark {
        "adw-gtk3-dark"
    } else {
        "adw-gtk3"
    };
    let roots = [
        paths.data.join("themes"),
        paths.home.join(".themes"),
        PathBuf::from("/usr/share/themes"),
        PathBuf::from("/usr/local/share/themes"),
    ];
    if !roots
        .iter()
        .any(|root| root.join(theme_name).join("gtk-3.0/gtk.css").is_file())
    {
        return Err(format!(
            "GTK integration needs {theme_name}. Install adw-gtk-theme (official package), then retry; no GTK changes were made"
        ));
    }
    for (directory, base, css) in [
        ("gtk-3.0", theme_name, &theme.files.gtk3),
        ("gtk-4.0", "Adwaita", &theme.files.gtk4),
    ] {
        let root = paths.config.join(directory);
        own_text(
            paths,
            manifest,
            changes,
            root.join("settings.ini"),
            |text| settings(text, base, theme.dark),
        )?;
        own_whole(
            paths,
            manifest,
            changes,
            root.join("shoji-colors.css"),
            file_text(css.clone(), &Snapshot::Absent),
            true,
        )?;
        let entry = root.join("gtk.css");
        let tracked = manifest.entries.iter().any(|e| e.path == entry);
        let original = snapshot(&entry)?;
        if !tracked && colloid(&original) {
            let (text, _) = import_block(
                "",
                "shoji-gtk-colors",
                "@import url(\"shoji-colors.css\");",
                true,
            )?;
            own_whole(
                paths,
                manifest,
                changes,
                entry,
                file_text(text, &Snapshot::Absent),
                false,
            )?;
        } else if tracked
            && manifest
                .entries
                .iter()
                .any(|e| e.path == entry && colloid(&e.original))
        {
            let (text, _) = import_block(
                "",
                "shoji-gtk-colors",
                "@import url(\"shoji-colors.css\");",
                true,
            )?;
            own_whole(
                paths,
                manifest,
                changes,
                entry,
                file_text(text, &original),
                false,
            )?;
        } else {
            own_text(paths, manifest, changes, entry, |text| {
                import_block(
                    text,
                    "shoji-gtk-colors",
                    "@import url(\"shoji-colors.css\");",
                    true,
                )
            })?;
        }
        // Colloid installs these as stylesheet-injecting links. Replace the links themselves,
        // never their targets, and leave real directories/unrelated themes alone.
        for name in ["gtk-dark.css", "assets"] {
            let path = root.join(name);
            let owned = manifest.entries.iter().any(|e| e.path == path);
            if owned
                || fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
                    && colloid(&snapshot(&path)?)
            {
                own_whole(paths, manifest, changes, path, Snapshot::Absent, false)?;
            }
        }
    }
    for (key, value) in [
        ("gtk-theme", format!("'{theme_name}'")),
        (
            "color-scheme",
            if theme.dark {
                "'prefer-dark'"
            } else {
                "'prefer-light'"
            }
            .to_owned(),
        ),
    ] {
        if let Some(setting) = manifest.desktop.iter_mut().find(|s| s.key == key) {
            setting.expected = value;
        } else {
            let original = desktop.get(key)?;
            manifest.desktop.push(DesktopSetting {
                key: key.into(),
                original,
                expected: value,
            });
        }
    }
    Ok(())
}

fn colloid(state: &Snapshot) -> bool {
    matches!(state, Snapshot::Link(target) if target.to_string_lossy().to_ascii_lowercase().contains("colloid"))
}

fn settings(text: &str, base: &str, dark: bool) -> Result<(String, Vec<Fragment>)> {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    let mut section = None;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "[Settings]" {
            if section.is_some() {
                return Err(
                    "Multiple GTK [Settings] sections; resolve ambiguity before enabling".into(),
                );
            }
            section = Some(index);
            end = lines.len();
        } else if trimmed.starts_with('[') && section.is_some() && end == lines.len() {
            end = index;
        }
    }
    if section.is_none() {
        if !text.is_empty() && !text.ends_with('\n') {
            lines.push("\n".into());
        }
        lines.push("[Settings]\n".into());
        section = Some(lines.len() - 1);
        end = lines.len();
    }
    let start = section.ok_or("Missing GTK section")? + 1;
    let mut replacements = BTreeMap::new();
    let mut additions = String::new();
    let mut fragments = Vec::new();
    for (key, value) in [
        ("gtk-theme-name", base),
        (
            "gtk-application-prefer-dark-theme",
            if dark { "1" } else { "0" },
        ),
    ] {
        let matching: Vec<usize> = (start..end)
            .filter(|index| {
                lines[*index]
                    .trim_start()
                    .split_once('=')
                    .is_some_and(|(name, _)| name.trim() == key)
            })
            .collect();
        if matching.len() > 1 {
            return Err(format!(
                "Duplicate GTK key {key}; resolve ambiguity before enabling"
            ));
        }
        if let Some(index) = matching.first().copied() {
            let old = lines[index].clone();
            let value_start = old.find('=').ok_or("Malformed GTK setting")? + 1;
            let tail = &old[value_start..];
            let comment = tail
                .char_indices()
                .find_map(|(i, c)| {
                    ((c == '#' || c == ';') && (i == 0 || tail[..i].ends_with(char::is_whitespace)))
                        .then_some(i)
                })
                .map(|i| format!(" {}", tail[i..].trim_end_matches(['\r', '\n'])))
                .unwrap_or_default();
            let newline = if old.ends_with("\r\n") { "\r\n" } else { "\n" };
            let new = format!("{}{value}{comment}{newline}", &old[..value_start]);
            fragments.push(Fragment {
                original: old,
                expected: new.clone(),
            });
            replacements.insert(index, new);
        } else {
            let new = format!("{key}={value}\n");
            fragments.push(Fragment {
                original: String::new(),
                expected: new.clone(),
            });
            additions.push_str(&new);
        }
    }
    // Keep a newly introduced section header when unrelated settings were later added to it.
    let mut output = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index == end {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&additions);
        }
        output.push_str(replacements.get(&index).unwrap_or(line));
    }
    if end == lines.len() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&additions);
    }
    Ok((output, fragments))
}

pub(super) fn owned_settings(text: &str) -> Vec<(String, String)> {
    let mut in_settings = false;
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_settings = line == "[Settings]";
        } else if in_settings {
            if let Some((key, value)) = line.split_once('=') {
                if matches!(
                    key.trim(),
                    "gtk-theme-name" | "gtk-application-prefer-dark-theme"
                ) {
                    values.push((key.trim().to_owned(), value.trim().to_owned()));
                }
            }
        }
    }
    values.sort();
    values
}
