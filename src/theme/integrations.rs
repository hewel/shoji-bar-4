//! Opt-in, journaled desktop configuration ownership. No browser process is restarted.

use super::matugen::GeneratedTheme;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

mod desktop;
mod gtk;
#[cfg(test)]
mod tests;
mod zen;
use desktop::{Desktop, GSettings};

type Result<T, E = String> = std::result::Result<T, E>;
const VERSION: u32 = 2;
const MAX_FILE: u64 = 4 * 1024 * 1024;
// Configuration and recovery records have separate bounds. Reserve most of the record
// budget for the rollback journal, so accepting a committed state never strands its undo.
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
const MAX_COMMITTED: usize = MAX_MANIFEST as usize / 3;

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn from_environment() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let root = |key: &str, fallback: PathBuf| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or(fallback)
        };
        let paths = Self {
            config: root("XDG_CONFIG_HOME", home.join(".config")),
            state: root("XDG_STATE_HOME", home.join(".local/state")),
            data: root("XDG_DATA_HOME", home.join(".local/share")),
            home,
        };
        paths.validate()?;
        Ok(paths)
    }

    fn validate(&self) -> Result<()> {
        for path in [&self.home, &self.config, &self.state, &self.data] {
            if !path.is_absolute()
                || path.parent().is_none()
                || path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            {
                return Err(format!("Unsafe configuration root: {}", path.display()));
            }
            guard_parents(&path.join(".shoji-path-check"))?;
        }
        Ok(())
    }

    fn manifest(&self, kind: &str) -> PathBuf {
        self.state
            .join("shoji-bar-4/theme")
            .join(format!("{kind}-integration.json"))
    }

    fn guard(&self, path: &Path) -> Result<()> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || ![&self.home, &self.config, &self.state]
                .iter()
                .any(|root| path.starts_with(root) && path != *root)
        {
            return Err(format!(
                "Configuration path escapes the allowed roots: {}",
                path.display()
            ));
        }
        guard_parents(path)
    }
}

#[derive(Clone, Debug)]
pub struct IntegrationReport {
    pub gtk: Result<String>,
    pub zen: Result<String>,
}

pub struct IntegrationManager {
    paths: Paths,
    desktop: Box<dyn Desktop>,
}

impl IntegrationManager {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            desktop: Box::new(GSettings),
        }
    }

    pub fn sync(
        &mut self,
        gtk: bool,
        zen: bool,
        theme: Option<&GeneratedTheme>,
    ) -> IntegrationReport {
        if let Err(error) = self.paths.validate() {
            return IntegrationReport {
                gtk: Err(error.clone()),
                zen: Err(error),
            };
        }
        IntegrationReport {
            gtk: self.sync_one("gtk", gtk, theme),
            zen: self.sync_one("zen", zen, theme),
        }
    }

    fn sync_one(
        &mut self,
        kind: &str,
        enabled: bool,
        theme: Option<&GeneratedTheme>,
    ) -> Result<String> {
        let path = self.paths.manifest(kind);
        self.paths.guard(&path)?;
        let mut manifest = match snapshot_limit(&path, MAX_MANIFEST)? {
            Snapshot::Absent => None,
            Snapshot::File { bytes, .. } => {
                let value: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
                    format!(
                        "Cannot read recovery record {}: {e}; preserve it and resolve manually",
                        path.display()
                    )
                })?;
                if value.version != VERSION || value.kind != kind {
                    return Err(format!(
                        "Unsupported recovery record {}; do not delete its backups",
                        path.display()
                    ));
                }
                Some(value)
            }
            _ => {
                return Err(format!(
                    "Recovery record must be a regular file: {}",
                    path.display()
                ));
            }
        };
        if let Some(record) = &mut manifest {
            self.validate_record(record, kind)?;
            if record.pending.is_some() {
                self.rollback(record)?;
                save_json(&path, record)?;
            }
        }
        if !enabled && manifest.is_none() {
            return Ok("Not linked; unmanaged configuration left untouched".into());
        }
        if enabled && theme.is_none() {
            return Err("No last-good palette available; generate a shell palette first".into());
        }
        let mut next = manifest.clone().unwrap_or_else(|| Manifest::new(kind));
        let mut changes = Vec::new();
        // Check every owned field before planning any write, including files unrelated to this update.
        for entry in &next.entries {
            entry.current(&self.paths)?;
        }
        if kind == "zen" {
            zen::check_persistence(&self.paths, &mut next)?;
        }
        for setting in &next.desktop {
            let actual = self.desktop.get(&setting.key)?;
            if actual.as_deref() != Some(setting.expected.as_str()) {
                return Err(format!(
                    "Desktop preference {} changed outside Shoji; restore {} before retrying",
                    setting.key, setting.expected
                ));
            }
        }
        if enabled {
            let theme = theme.ok_or("No palette")?;
            match kind {
                "gtk" => gtk::plan(
                    &self.paths,
                    &mut next,
                    &mut changes,
                    theme,
                    self.desktop.as_mut(),
                )?,
                "zen" => zen::plan(&self.paths, &mut next, &mut changes, theme)?,
                _ => return Err("Unknown integration".into()),
            }
            next.disabled = false;
        } else if kind == "zen" {
            zen::restore_preference(&self.paths, &mut next, &mut changes)?;
            next.disabled = true;
        } else {
            for entry in &next.entries {
                let before = entry.current(&self.paths)?;
                let after = entry.restore(&before)?;
                if before != after {
                    changes.push(Change {
                        path: entry.path.clone(),
                        before,
                        after,
                    });
                }
            }
            next.entries.clear();
            next.disabled = true;
        }
        let mut settings = Vec::new();
        for setting in &next.desktop {
            let before = self.desktop.get(&setting.key)?;
            let after = if enabled {
                Some(setting.expected.clone())
            } else {
                setting.original.clone()
            };
            if before != after {
                settings.push(DesktopChange {
                    key: setting.key.clone(),
                    before,
                    after,
                });
            }
        }
        if !enabled {
            next.desktop.clear();
        }
        let mut recovery = manifest.unwrap_or_else(|| Manifest::new(kind));
        recovery.pending = Some(Journal {
            files: changes,
            desktop: settings,
        });
        self.validate_record(&next, kind)?;
        self.validate_record(&recovery, kind)?;
        validate_file_sizes(&next)?;
        validate_file_sizes(&recovery)?;
        // Both records must be readable before the first mutation. A committed record
        // additionally leaves room for its unchanged-state disable/rollback journal.
        let committed = encode_manifest(&next)?;
        if committed.len() > MAX_COMMITTED {
            return Err("Integration recovery record is too large to safely retain an undo journal; reduce configuration size before enabling".into());
        }
        let pending = encode_manifest(&recovery)?;
        replace(
            &path,
            &Snapshot::File {
                bytes: pending,
                mode: 0o600,
            },
        )?;
        let apply = self.apply(recovery.pending.as_ref().ok_or("Missing journal")?);
        if let Err(error) = apply {
            return match self.rollback(&mut recovery) {
                Ok(()) => {
                    save_json(&path, &recovery)?;
                    Err(format!("{error}; this operation was rolled back"))
                }
                Err(rollback) => Err(format!(
                    "{error}; recovery required: {rollback}. Retain {}",
                    path.display()
                )),
            };
        }
        // A crash before this commit is recovered by rolling back the journal on the next sync.
        if let Err(error) = replace(
            &path,
            &Snapshot::File {
                bytes: committed,
                mode: 0o600,
            },
        ) {
            self.rollback(&mut recovery)?;
            save_json(&path, &recovery)?;
            return Err(error);
        }
        if !enabled && next.entries.is_empty() {
            self.paths.guard(&path)?;
            fs::remove_file(&path).map_err(|e| format!("Cannot retire recovery record: {e}"))?;
        }
        Ok(if enabled {
            if kind == "zen" { "Files updated; Zen application pending restart" }
            else { "Files updated; GTK applications may need restarting" }
        } else if kind == "zen" && !next.entries.is_empty() {
            "Styles restored; prior stylesheet preference will be restored on next Zen start (tracked user.js restoration directive retained; prefs.js is never edited)"
        } else if kind == "zen" {
            "Original configuration restored; restart Zen to apply its prior stylesheet preference"
        } else {
            "Original configuration restored; applications may need restarting"
        }.into())
    }

    fn validate_record(&self, record: &Manifest, kind: &str) -> Result<()> {
        for entry in &record.entries {
            self.paths.guard(&entry.path)?;
            if !allowed_file(&self.paths, kind, &entry.path) {
                return Err(format!(
                    "Recovery record contains an out-of-scope file: {}",
                    entry.path.display()
                ));
            }
        }
        if let Some(journal) = &record.pending {
            for change in &journal.files {
                self.paths.guard(&change.path)?;
                if !allowed_file(&self.paths, kind, &change.path) {
                    return Err("Out-of-scope recovery path".into());
                }
            }
        }
        for key in record.desktop.iter().map(|s| s.key.as_str()).chain(
            record
                .pending
                .iter()
                .flat_map(|j| j.desktop.iter().map(|s| s.key.as_str())),
        ) {
            if kind != "gtk" || !matches!(key, "gtk-theme" | "color-scheme") {
                return Err("Out-of-scope desktop recovery key".into());
            }
        }
        for profile in &record.zen_profiles {
            self.paths.guard(&profile.profile.join("prefs.js"))?;
            if kind != "zen" || !allowed_file(&self.paths, "zen", &profile.profile.join("user.js"))
            {
                return Err("Out-of-scope Zen preference recovery profile".into());
            }
        }
        if record
            .zen_active
            .as_ref()
            .is_some_and(|active| !record.zen_profiles.iter().any(|p| &p.profile == active))
        {
            return Err("Missing Zen active-profile recovery metadata".into());
        }
        Ok(())
    }

    fn apply(&mut self, journal: &Journal) -> Result<()> {
        for change in &journal.files {
            self.paths.guard(&change.path)?;
            if snapshot(&change.path)? != change.before {
                return Err(conflict(&change.path));
            }
            replace(&change.path, &change.after)?;
        }
        for change in &journal.desktop {
            if self.desktop.get(&change.key)? != change.before {
                return Err(format!(
                    "Desktop preference {} changed during update",
                    change.key
                ));
            }
            self.desktop.set(&change.key, change.after.as_deref())?;
        }
        Ok(())
    }

    fn rollback(&mut self, record: &mut Manifest) -> Result<()> {
        let journal = record.pending.as_ref().ok_or("Missing recovery journal")?;
        // Preflight rollback too: never restore one file and then discover a foreign edit elsewhere.
        for change in &journal.files {
            self.paths.guard(&change.path)?;
            let actual = snapshot(&change.path)?;
            if actual != change.before && actual != change.after {
                return Err(conflict(&change.path));
            }
        }
        for change in &journal.desktop {
            let actual = self.desktop.get(&change.key)?;
            if actual != change.before && actual != change.after {
                return Err(format!(
                    "Recovery conflict in desktop preference {}",
                    change.key
                ));
            }
        }
        for change in journal.desktop.iter().rev() {
            if self.desktop.get(&change.key)? == change.after {
                self.desktop.set(&change.key, change.before.as_deref())?;
            }
        }
        for change in journal.files.iter().rev() {
            self.paths.guard(&change.path)?;
            if snapshot(&change.path)? == change.after {
                replace(&change.path, &change.before)?;
            }
        }
        record.pending = None;
        Ok(())
    }
}

fn allowed_file(paths: &Paths, kind: &str, path: &Path) -> bool {
    if kind == "gtk" {
        return ["gtk-3.0", "gtk-4.0"].iter().any(|dir| {
            let root = paths.config.join(dir);
            path.parent() == Some(root.as_path())
                && matches!(
                    path.file_name().and_then(|n| n.to_str()),
                    Some(
                        "settings.ini" | "gtk.css" | "gtk-dark.css" | "assets" | "shoji-colors.css"
                    )
                )
        });
    }
    (path.starts_with(&paths.home) || path.starts_with(&paths.config))
        && match path.file_name().and_then(|n| n.to_str()) {
            Some("user.js") => true,
            Some("userChrome.css" | "shoji-colors.css") => path
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == "chrome")),
            _ => false,
        }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum Snapshot {
    Absent,
    File { bytes: Vec<u8>, mode: u32 },
    Link(PathBuf),
}

fn snapshot(path: &Path) -> Result<Snapshot> {
    snapshot_limit(path, MAX_FILE)
}

fn snapshot_limit(path: &Path, limit: u64) -> Result<Snapshot> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::Absent),
        Err(e) => return Err(format!("Cannot inspect {}: {e}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(Snapshot::Link)
            .map_err(|e| e.to_string());
    }
    if !metadata.is_file() || metadata.len() > limit {
        return Err(format!(
            "Expected a regular configuration/recovery file of at most {limit} bytes: {}",
            path.display()
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{} grew beyond its safe read limit",
            path.display()
        ));
    }
    Ok(Snapshot::File {
        bytes,
        mode: metadata.permissions().mode() & 0o777,
    })
}

fn guard_parents(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    if let Some(parent) = path.parent() {
        for part in parent.components() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(format!(
                        "Refusing symlink/non-directory configuration parent {}",
                        current.display()
                    ));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Cannot inspect parent {}: {e}", current.display())),
            }
        }
    }
    Ok(())
}

fn replace(path: &Path, state: &Snapshot) -> Result<()> {
    guard_parents(path)?;
    let parent = path.parent().ok_or("Missing parent directory")?;
    if *state == Snapshot::Absent {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Cannot remove {}: {e}", path.display())),
        };
    }
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    guard_parents(path)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    match state {
        Snapshot::File { bytes, mode } => {
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(*mode))
                .map_err(|e| e.to_string())?;
            file.write_all(bytes)
                .and_then(|_| file.as_file().sync_all())
                .map_err(|e| e.to_string())?;
            file.persist(path)
                .map_err(|e| format!("Cannot replace {}: {e}", path.display()))?;
        }
        Snapshot::Link(target) => {
            let temp = file.into_temp_path();
            fs::remove_file(&temp).map_err(|e| e.to_string())?;
            symlink(target, &temp).map_err(|e| e.to_string())?;
            fs::rename(&temp, path)
                .map_err(|e| format!("Cannot restore link {}: {e}", path.display()))?;
        }
        Snapshot::Absent => unreachable!(),
    }
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

fn save_json(path: &Path, value: &Manifest) -> Result<()> {
    let bytes = encode_manifest(value)?;
    replace(path, &Snapshot::File { bytes, mode: 0o600 })
}

fn encode_manifest(value: &Manifest) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("Integration recovery record exceeds the 64 MiB safe read limit; no configuration changes were applied".into());
    }
    Ok(bytes)
}

fn validate_file_sizes(value: &Manifest) -> Result<()> {
    let entry_states = value
        .entries
        .iter()
        .flat_map(|e| [&e.original, &e.expected]);
    let pending_states = value
        .pending
        .iter()
        .flat_map(|j| j.files.iter())
        .flat_map(|c| [&c.before, &c.after]);
    for state in entry_states.chain(pending_states) {
        if matches!(state, Snapshot::File { bytes, .. } if bytes.len() as u64 > MAX_FILE) {
            return Err("An integration configuration exceeds its 4 MiB safe read limit; no configuration changes were applied".into());
        }
    }
    Ok(())
}

fn conflict(path: &Path) -> String {
    format!(
        "{} was changed outside Shoji; preserve your edits and restore its managed lines/file before retrying (recovery record retained)",
        path.display()
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    kind: String,
    entries: Vec<Entry>,
    desktop: Vec<DesktopSetting>,
    pending: Option<Journal>,
    disabled: bool,
    zen_active: Option<PathBuf>,
    zen_profiles: Vec<ZenProfile>,
}
impl Manifest {
    fn new(kind: &str) -> Self {
        Self {
            version: VERSION,
            kind: kind.into(),
            entries: Vec::new(),
            desktop: Vec::new(),
            pending: None,
            disabled: false,
            zen_active: None,
            zen_profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ZenProfile {
    profile: PathBuf,
    prior_pref: bool,
    // Last observed prefs.js value. Only its normal transition toward the currently
    // owned directive is accepted; a later reversal is a user-edit conflict.
    persisted: Option<bool>,
    restoring: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    files: Vec<Change>,
    desktop: Vec<DesktopChange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Change {
    path: PathBuf,
    before: Snapshot,
    after: Snapshot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DesktopChange {
    key: String,
    before: Option<String>,
    after: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DesktopSetting {
    key: String,
    original: Option<String>,
    expected: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    original: Snapshot,
    expected: Snapshot,
    // Exact, unique owned replacements allow unrelated later additions to survive restoration.
    fragments: Vec<Fragment>,
    #[serde(default)]
    linked_text: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Fragment {
    original: String,
    expected: String,
}

impl Entry {
    fn current(&self, paths: &Paths) -> Result<Snapshot> {
        paths.guard(&self.path)?;
        let actual = snapshot(&self.path)?;
        if actual == self.expected {
            return Ok(actual);
        }
        if self.fragments.is_empty() {
            return Err(conflict(&self.path));
        }
        let text = text_of(&actual).map_err(|_| conflict(&self.path))?;
        let expected = text_of(&self.expected)?;
        if self.path.file_name().is_some_and(|n| n == "user.js")
            && zen::preference(&text)? != zen::preference(&expected)?
        {
            return Err(conflict(&self.path));
        }
        if self.path.file_name().is_some_and(|n| n == "settings.ini")
            && gtk::owned_settings(&text) != gtk::owned_settings(&expected)
        {
            return Err(conflict(&self.path));
        }
        for part in &self.fragments {
            if part.expected.is_empty() || text.matches(&part.expected).count() != 1 {
                return Err(conflict(&self.path));
            }
        }
        Ok(actual)
    }

    fn restore(&self, actual: &Snapshot) -> Result<Snapshot> {
        if *actual == self.expected {
            return Ok(self.original.clone());
        }
        let mut text = text_of(actual)?;
        for part in &self.fragments {
            if part.expected.is_empty() || text.matches(&part.expected).count() != 1 {
                return Err(conflict(&self.path));
            }
            text = text.replacen(&part.expected, &part.original, 1);
        }
        Ok(file_text(text, actual))
    }
}

fn text_of(state: &Snapshot) -> Result<String> {
    match state {
        Snapshot::Absent => Ok(String::new()),
        Snapshot::File { bytes, .. } => {
            String::from_utf8(bytes.clone()).map_err(|_| "Configuration is not UTF-8".into())
        }
        Snapshot::Link(_) => Err(
            "Text configuration is a symlink; refusing to follow or discard its contents".into(),
        ),
    }
}
fn file_text(text: String, original: &Snapshot) -> Snapshot {
    let mode = match original {
        Snapshot::File { mode, .. } => *mode,
        _ => 0o644,
    };
    Snapshot::File {
        bytes: text.into_bytes(),
        mode,
    }
}

fn own_whole(
    paths: &Paths,
    manifest: &mut Manifest,
    changes: &mut Vec<Change>,
    path: PathBuf,
    after: Snapshot,
    must_be_new: bool,
) -> Result<()> {
    paths.guard(&path)?;
    let index = manifest.entries.iter().position(|e| e.path == path);
    let before = if let Some(index) = index {
        manifest.entries[index].current(paths)?
    } else {
        snapshot(&path)?
    };
    if index.is_none() && must_be_new && before != Snapshot::Absent {
        return Err(format!(
            "Owned output already exists: {}; move it aside before enabling",
            path.display()
        ));
    }
    if let Some(index) = index {
        manifest.entries[index].expected = after.clone();
        manifest.entries[index].fragments.clear();
    } else {
        manifest.entries.push(Entry {
            path: path.clone(),
            original: before.clone(),
            expected: after.clone(),
            fragments: Vec::new(),
            linked_text: None,
        });
    }
    if before != after {
        changes.push(Change {
            path,
            before,
            after,
        });
    }
    Ok(())
}

fn own_text(
    paths: &Paths,
    manifest: &mut Manifest,
    changes: &mut Vec<Change>,
    path: PathBuf,
    transform: impl FnOnce(&str) -> Result<(String, Vec<Fragment>)>,
) -> Result<()> {
    paths.guard(&path)?;
    let index = manifest.entries.iter().position(|e| e.path == path);
    let before = if let Some(i) = index {
        manifest.entries[i].current(paths)?
    } else {
        snapshot(&path)?
    };
    let baseline = if let Some(i) = index {
        manifest.entries[i].restore(&before)?
    } else {
        before.clone()
    };
    let baseline_text = if let Some(i) = index {
        manifest.entries[i]
            .linked_text
            .clone()
            .map(Ok)
            .unwrap_or_else(|| read_text(&path, &baseline))?
    } else {
        read_text(&path, &baseline)?
    };
    let (text, mut fragments) = transform(&baseline_text)?;
    let linked_text = if matches!(baseline, Snapshot::Link(_)) {
        // There is nowhere to put later additions when restoring a symlink without editing
        // its target. Treat the replacement as whole-file ownership in this case.
        fragments.clear();
        Some(baseline_text)
    } else {
        None
    };
    let after = file_text(text, &baseline);
    let entry = Entry {
        path: path.clone(),
        original: baseline,
        expected: after.clone(),
        fragments,
        linked_text,
    };
    if let Some(i) = index {
        manifest.entries[i] = entry;
    } else {
        manifest.entries.push(entry);
    }
    if before != after {
        changes.push(Change {
            path,
            before,
            after,
        });
    }
    Ok(())
}

fn read_text(path: &Path, state: &Snapshot) -> Result<String> {
    if matches!(state, Snapshot::Link(_)) {
        let metadata = fs::metadata(path)
            .map_err(|e| format!("Cannot read linked configuration {}: {e}", path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_FILE {
            return Err(format!(
                "Linked configuration is not a regular file of at most 4 MiB: {}",
                path.display()
            ));
        }
        // Reading an original target is permitted; all writes replace the link itself.
        return fs::read_to_string(path)
            .map_err(|e| format!("Cannot read linked configuration {}: {e}", path.display()));
    }
    text_of(state)
}

fn import_block(
    text: &str,
    marker: &str,
    body: &str,
    beginning: bool,
) -> Result<(String, Vec<Fragment>)> {
    if text.contains(marker) {
        return Err(format!(
            "Untracked Shoji marker {marker}; recover the prior manifest before enabling"
        ));
    }
    let block = format!("/* {marker} begin */\n{body}\n/* {marker} end */\n");
    let (expected, original, result) = if beginning {
        (block.clone(), String::new(), format!("{block}{text}"))
    } else {
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let expected = format!("{separator}{block}");
        (expected.clone(), String::new(), format!("{text}{expected}"))
    };
    Ok((result, vec![Fragment { original, expected }]))
}
