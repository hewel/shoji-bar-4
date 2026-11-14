use super::*;

const PREF: &str = "toolkit.legacyUserProfileCustomizations.stylesheets";
const MARKER: &str = "shoji-zen-stylesheet-preference";

pub(super) fn plan(
    paths: &Paths,
    manifest: &mut Manifest,
    changes: &mut Vec<Change>,
    theme: &GeneratedTheme,
) -> Result<()> {
    let profile = discover(paths)?;
    if let Some(active) = &manifest.zen_active {
        if active != &profile {
            return Err(format!(
                "Zen's default profile changed from {} to {}; disable the previous integration before enabling the new profile",
                active.display(),
                profile.display()
            ));
        }
    }
    let user = profile.join("user.js");
    if !manifest.zen_profiles.iter().any(|p| p.profile == profile) {
        paths.guard(&user)?;
        let prefs = profile.join("prefs.js");
        paths.guard(&prefs)?;
        let user_pref = preference(&read_text(&user, &snapshot(&user)?)?)?;
        let persisted = preference(&read_text(&prefs, &snapshot(&prefs)?)?)?;
        manifest.zen_profiles.push(ZenProfile {
            profile: profile.clone(),
            prior_pref: user_pref.or(persisted).unwrap_or(false),
            persisted,
            restoring: false,
        });
    }
    own_whole(
        paths,
        manifest,
        changes,
        profile.join("chrome/shoji-colors.css"),
        file_text(theme.files.zen.clone(), &Snapshot::Absent),
        true,
    )?;
    own_text(
        paths,
        manifest,
        changes,
        profile.join("chrome/userChrome.css"),
        |text| {
            import_block(
                text,
                "shoji-zen-colors",
                "@import url(\"shoji-colors.css\");",
                true,
            )
        },
    )?;
    own_text(paths, manifest, changes, user, |text| {
        preference(text)?;
        import_block(
            text,
            MARKER,
            &format!("user_pref(\"{PREF}\", true);"),
            false,
        )
    })?;
    if let Some(state) = manifest
        .zen_profiles
        .iter_mut()
        .find(|p| p.profile == profile)
    {
        state.restoring = false;
    }
    manifest.zen_active = Some(profile);
    Ok(())
}

pub(super) fn restore_preference(
    paths: &Paths,
    manifest: &mut Manifest,
    changes: &mut Vec<Change>,
) -> Result<()> {
    let Some(profile) = manifest.zen_active.clone() else {
        return Ok(());
    };
    let user_path = profile.join("user.js");
    let active_paths = [
        user_path.clone(),
        profile.join("chrome/userChrome.css"),
        profile.join("chrome/shoji-colors.css"),
    ];
    for entry in manifest
        .entries
        .iter()
        .filter(|e| active_paths.contains(&e.path))
    {
        let before = entry.current(paths)?;
        let after = entry.restore(&before)?;
        if before != after {
            changes.push(Change {
                path: entry.path.clone(),
                before,
                after,
            });
        }
    }
    let user = manifest
        .entries
        .iter()
        .find(|e| e.path == user_path)
        .cloned()
        .ok_or("Missing active Zen user.js recovery entry")?;
    let before = user.current(paths)?;
    let baseline = user.restore(&before)?;
    let original = user
        .linked_text
        .clone()
        .map(Ok)
        .unwrap_or_else(|| text_of(&baseline))?;
    let state = manifest
        .zen_profiles
        .iter_mut()
        .find(|p| p.profile == profile)
        .ok_or("Missing active Zen preference recovery metadata")?;
    // Removing user.js does not undo a value already persisted into prefs.js. Retain
    // this restoration directive, and its ownership, independently of the active profile.
    // A later default-profile switch must not abandon or overwrite this recovery state.
    let mut retained = None;
    if preference(&original)?.is_none() {
        let (text, mut fragments) = import_block(
            &original,
            MARKER,
            &format!("user_pref(\"{PREF}\", {});", state.prior_pref),
            false,
        )?;
        let after = file_text(text, &baseline);
        if matches!(baseline, Snapshot::Link(_)) {
            fragments.clear();
        }
        retained = Some(Entry {
            path: user_path.clone(),
            original: baseline,
            expected: after.clone(),
            fragments,
            linked_text: user.linked_text,
        });
        if let Some(change) = changes.iter_mut().find(|c| c.path == user_path) {
            change.after = after;
        } else if before != after {
            changes.push(Change {
                path: user_path,
                before,
                after,
            });
        }
        state.restoring = true;
    }
    manifest
        .entries
        .retain(|entry| !active_paths.contains(&entry.path));
    if let Some(entry) = retained {
        manifest.entries.push(entry);
    } else {
        manifest.zen_profiles.retain(|p| p.profile != profile);
    }
    manifest.zen_active = None;
    Ok(())
}

pub(super) fn check_persistence(paths: &Paths, manifest: &mut Manifest) -> Result<()> {
    for profile in &mut manifest.zen_profiles {
        let prefs = profile.profile.join("prefs.js");
        paths.guard(&prefs)?;
        let actual = preference(&read_text(&prefs, &snapshot(&prefs)?)?)?;
        let target = if profile.restoring {
            profile.prior_pref
        } else {
            true
        };
        if actual != profile.persisted && actual != Some(target) {
            return Err(format!(
                "{} stylesheet preference changed outside Shoji; preserve your choice and resolve the managed user.js directive/recovery record before retrying",
                prefs.display()
            ));
        }
        profile.persisted = actual;
    }
    Ok(())
}

pub(super) fn discover(paths: &Paths) -> Result<PathBuf> {
    for root in [paths.config.join("zen"), paths.home.join(".zen")] {
        let registry = root.join("profiles.ini");
        paths.guard(&registry)?;
        let state = snapshot(&registry)?;
        if state == Snapshot::Absent {
            continue;
        }
        let sections = ini(&text_of(&state)?)?;
        let mut selected = Vec::new();
        for (name, values) in &sections {
            if name.starts_with("Install") {
                if let Some(default) = values.get("Default") {
                    selected.push(profile_path(paths, &root, default, None)?);
                }
            }
        }
        // Install defaults take authority over Profile.Default, which can still name an
        // old installation's profile. Do not even resolve those unselected paths.
        if selected.is_empty() {
            for (name, values) in &sections {
                if name.starts_with("Profile") && values.get("Default").is_some_and(|v| v == "1") {
                    let path = values.get("Path").ok_or_else(|| {
                        format!("Default profile in {} has no Path", registry.display())
                    })?;
                    selected.push(profile_path(
                        paths,
                        &root,
                        path,
                        values.get("IsRelative").map(String::as_str),
                    )?);
                }
            }
        }
        selected.sort();
        selected.dedup();
        if selected.len() > 1 {
            return Err(format!(
                "Ambiguous Zen install-default profiles in {}; select one default before enabling",
                registry.display()
            ));
        }
        if let Some(profile) = selected.pop() {
            paths.guard(&profile.join("user.js"))?;
            if !profile.is_dir() {
                return Err(format!(
                    "Zen default profile does not exist: {}",
                    profile.display()
                ));
            }
            return Ok(profile);
        }
    }
    Err("No Zen default profile found in XDG_CONFIG_HOME/zen/profiles.ini or ~/.zen/profiles.ini; start Zen once and select a default profile".into())
}

fn profile_path(
    paths: &Paths,
    root: &Path,
    value: &str,
    relative: Option<&str>,
) -> Result<PathBuf> {
    let source = Path::new(value);
    if value.is_empty()
        || source
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("Unsafe Zen profile path in profiles.ini".into());
    }
    let path = match relative {
        Some("0") if source.is_absolute() => source.to_path_buf(),
        Some("0") => return Err("Zen absolute profile path is not absolute".into()),
        Some("1") if source.is_absolute() => {
            return Err("Zen relative profile path is absolute".into());
        }
        Some("1") | None => {
            if source.is_absolute() {
                source.to_path_buf()
            } else {
                root.join(source)
            }
        }
        Some(_) => return Err("Invalid IsRelative value in Zen profiles.ini".into()),
    };
    if path == paths.home || path == paths.config {
        return Err("Zen profile cannot be a configuration root".into());
    }
    paths.guard(&path.join("user.js"))?;
    if !allowed_file(paths, "zen", &path.join("user.js")) {
        return Err("Zen profile is outside the HOME/XDG_CONFIG_HOME recovery scope".into());
    }
    Ok(path)
}

fn ini(text: &str) -> Result<Vec<(String, BTreeMap<String, String>)>> {
    let mut result: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].to_owned();
            if result.iter().any(|(existing, _)| *existing == name) {
                return Err(format!("Duplicate Zen registry section {name}"));
            }
            result.push((name, BTreeMap::new()));
        } else if let Some((key, value)) = line.split_once('=') {
            let (_, section) = result
                .last_mut()
                .ok_or("Zen registry key outside a section")?;
            if section
                .insert(key.trim().into(), value.trim().into())
                .is_some()
            {
                return Err("Duplicate Zen registry key".into());
            }
        } else {
            return Err("Malformed Zen profiles.ini".into());
        }
    }
    Ok(result)
}

#[derive(Debug, PartialEq)]
enum Token {
    Name(String),
    String(String),
    Symbol(u8),
}

// Parse only the boolean preference we own. Tokenization skips JS comments and quoted
// strings, so commented-out prefs or wake-lock strings cannot masquerade as directives.
pub(super) fn preference(text: &str) -> Result<Option<bool>> {
    let tokens = tokens(text)?;
    let mut result = None;
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(token, Token::String(value) if value == PREF) {
            continue;
        }
        if index < 2
            || !matches!(tokens.get(index - 2), Some(Token::Name(value)) if value == "user_pref")
            || !matches!(tokens.get(index - 1), Some(Token::Symbol(b'(')))
            || !matches!(tokens.get(index + 1), Some(Token::Symbol(b',')))
            || !matches!(tokens.get(index + 3), Some(Token::Symbol(b')')))
        {
            return Err("Zen stylesheet preference uses unsupported JavaScript; simplify it to a boolean user_pref before enabling".into());
        }
        result = match tokens.get(index + 2) {
            Some(Token::Name(value)) if value == "true" => Some(true),
            Some(Token::Name(value)) if value == "false" => Some(false),
            _ => return Err("Zen stylesheet preference is not a boolean user_pref".into()),
        };
    }
    Ok(result)
}

fn tokens(text: &str) -> Result<Vec<Token>> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut output = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b if b.is_ascii_whitespace() => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                if index + 1 >= bytes.len() {
                    return Err("Unterminated JavaScript comment in Zen preferences".into());
                }
                index += 2;
            }
            quote @ (b'\'' | b'"') => {
                index += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if byte == quote {
                        closed = true;
                        break;
                    }
                    if byte == b'\\' {
                        let escaped = *bytes.get(index).ok_or("Unterminated JS string escape")?;
                        index += 1;
                        if escaped == b'u' || escaped == b'x' {
                            let count = if escaped == b'u' { 4 } else { 2 };
                            let digits = bytes
                                .get(index..index + count)
                                .ok_or("Incomplete JS character escape")?;
                            let digits = std::str::from_utf8(digits)
                                .map_err(|_| "Invalid JS character escape")?;
                            let code = u32::from_str_radix(digits, 16)
                                .map_err(|_| "Invalid JS character escape")?;
                            let mut buffer = [0; 4];
                            value.extend_from_slice(
                                char::from_u32(code)
                                    .unwrap_or('\u{fffd}')
                                    .encode_utf8(&mut buffer)
                                    .as_bytes(),
                            );
                            index += count;
                        } else {
                            match escaped {
                                b'\n' => {}
                                b'\r' => {
                                    if bytes.get(index) == Some(&b'\n') {
                                        index += 1;
                                    }
                                }
                                b'n' => value.push(b'\n'),
                                b'r' => value.push(b'\r'),
                                b't' => value.push(b'\t'),
                                _ => value.push(escaped),
                            }
                        }
                    } else {
                        value.push(byte);
                    }
                }
                if !closed {
                    return Err("Unterminated JavaScript string in Zen preferences".into());
                }
                output.push(Token::String(String::from_utf8_lossy(&value).into_owned()));
            }
            b if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$') => {
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric()
                        || matches!(bytes[index], b'_' | b'$' | b'.'))
                {
                    index += 1;
                }
                output.push(Token::Name(
                    String::from_utf8_lossy(&bytes[start..index]).into_owned(),
                ));
            }
            symbol => {
                output.push(Token::Symbol(symbol));
                index += 1;
            }
        }
    }
    Ok(output)
}
