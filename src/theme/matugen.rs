//! Isolated Matugen generation and the validated, last-good palette cache.

use crate::settings::Palette;
use anti_webui::prelude::rgb;
use anti_webui_md3e::ColorScheme;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const FILE_LIMIT: usize = 64 * 1024;
const CACHE_LIMIT: usize = 256 * 1024;
const CACHE_VERSION: u32 = 1;
const TEMPLATES: [(&str, &str); 3] = [
    ("gtk3", include_str!("templates/gtk3.css")),
    ("gtk4", include_str!("templates/gtk4.css")),
    ("zen", include_str!("templates/zen.css")),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Color(u32),
    Image(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub source: Source,
    pub palette: Palette,
    pub dark: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFiles {
    pub gtk3: String,
    pub gtk4: String,
    pub zen: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedTheme {
    pub dark: bool,
    pub source_color: u32,
    pub colors: BTreeMap<String, u32>,
    pub files: ThemeFiles,
}

impl GeneratedTheme {
    /// Every shell role must be supplied by Matugen; missing roles are errors.
    pub fn scheme(&self) -> Result<ColorScheme, String> {
        macro_rules! roles {
            ($($role:ident),+ $(,)?) => {
                Ok(ColorScheme { $($role: rgb(self.color(stringify!($role))?),)+ })
            };
        }
        roles!(
            primary,
            on_primary,
            primary_container,
            on_primary_container,
            secondary,
            on_secondary,
            secondary_container,
            on_secondary_container,
            tertiary,
            on_tertiary,
            tertiary_container,
            on_tertiary_container,
            error,
            on_error,
            error_container,
            on_error_container,
            surface,
            surface_dim,
            surface_bright,
            surface_variant,
            on_surface,
            on_surface_variant,
            inverse_surface,
            inverse_on_surface,
            inverse_primary,
            scrim,
            surface_container_lowest,
            surface_container_low,
            surface_container,
            surface_container_high,
            surface_container_highest,
            outline,
            outline_variant,
        )
    }

    fn color(&self, role: &str) -> Result<u32, String> {
        match self.colors.get(role).copied() {
            Some(value) if value <= 0xffffff => Ok(value),
            Some(_) => Err(format!("Matugen color '{role}' is not an opaque RGB color")),
            None => Err(format!("Matugen palette is missing '{role}'")),
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.scheme()?;
        if self.source_color > 0xffffff || self.color("source_color")? != self.source_color {
            return Err("Matugen palette has an inconsistent source color".into());
        }
        for role in self.colors.keys() {
            self.color(role)?;
        }
        // Exact expansion also rejects unresolved tokens, truncated CSS, injected rules,
        // stale templates and cached CSS that disagrees with the shell palette.
        for ((name, template), rendered) in
            TEMPLATES
                .iter()
                .zip([&self.files.gtk3, &self.files.gtk4, &self.files.zen])
        {
            if rendered.len() > FILE_LIMIT
                || !expected_template(template, &self.colors, self.dark)?
                    .eq_ignore_ascii_case(rendered)
            {
                return Err(format!(
                    "Matugen {name} stylesheet does not match the generated palette"
                ));
            }
        }
        Ok(())
    }
}

pub fn generate(request: &Request, cancelled: &AtomicBool) -> Result<GeneratedTheme, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Matugen generation cancelled".into());
    }
    let workspace = tempfile::Builder::new()
        .prefix("shoji-matugen-")
        .tempdir()
        .map_err(|e| format!("Cannot create Matugen workspace: {e}"))?;
    let config = prepare_workspace(workspace.path())?;
    let mut command = Command::new("matugen");
    command
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("XDG_CONFIG_HOME", workspace.path().join("config"))
        .env("XDG_CACHE_HOME", workspace.path().join("cache"))
        .env("XDG_STATE_HOME", workspace.path().join("state"))
        .env("XDG_DATA_HOME", workspace.path().join("data"))
        .arg("--config")
        .arg(config)
        .args(["--mode", if request.dark { "dark" } else { "light" }])
        .args(["--type", &format!("scheme-{}", request.palette.name())])
        .args(["--source-color-index", "0", "--json", "hex", "--quiet"])
        .args(["--include-image-in-json", "false"]);
    match &request.source {
        Source::Color(seed) => {
            if *seed > 0xffffff {
                return Err("The theme seed must be a 24-bit RGB color".into());
            }
            command.args(["color", "hex", &format!("#{seed:06x}")]);
        }
        Source::Image(path) => {
            let path = fs::canonicalize(path)
                .map_err(|e| format!("Cannot read theme image '{}': {e}", path.display()))?;
            command.arg("image").arg("--").arg(path);
        }
    }
    let output = command_output(&mut command, TIMEOUT, OUTPUT_LIMIT, cancelled)?;
    let (source_color, colors) = parse_palette(&output, request.dark)?;
    if let Source::Color(seed) = &request.source {
        if source_color != *seed {
            return Err("Matugen returned a different source color than the requested seed".into());
        }
    }
    let theme = GeneratedTheme {
        dark: request.dark,
        source_color,
        colors,
        files: ThemeFiles {
            gtk3: read_text(&workspace.path().join("gtk3.css"), FILE_LIMIT)?,
            gtk4: read_text(&workspace.path().join("gtk4.css"), FILE_LIMIT)?,
            zen: read_text(&workspace.path().join("zen.css"), FILE_LIMIT)?,
        },
    };
    theme.validate()?;
    Ok(theme)
}

fn prepare_workspace(workspace: &Path) -> Result<PathBuf, String> {
    let mut config = String::from(
        "[config]\nversion_check = false\ncaching = false\n\
         [config.custom_colors]\n\
         success = { color = \"#2e7d32\", blend = false }\n\
         warning = { color = \"#a56a00\", blend = false }\n",
    );
    for (name, template) in TEMPLATES {
        let input = workspace.join(format!("{name}.template"));
        let output = workspace.join(format!("{name}.css"));
        fs::write(&input, template).map_err(|e| format!("Cannot write Matugen template: {e}"))?;
        writeln!(
            config,
            "[templates.{name}]\ninput_path = {}\noutput_path = {}",
            toml_path(&input)?,
            toml_path(&output)?,
        )
        .expect("writing to a String cannot fail");
    }
    let path = workspace.join("config.toml");
    fs::write(&path, config).map_err(|e| format!("Cannot write owned Matugen config: {e}"))?;
    Ok(path)
}

fn toml_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("Matugen workspace path is not valid UTF-8")?;
    let mut encoded = String::with_capacity(text.len() + 2);
    encoded.push('"');
    for ch in text.chars() {
        match ch {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            ch if ch.is_control() => {
                write!(encoded, "\\u{:04X}", u32::from(ch))
                    .expect("writing to a String cannot fail");
            }
            ch => encoded.push(ch),
        }
    }
    encoded.push('"');
    Ok(encoded)
}

#[derive(Deserialize)]
struct MatugenJson {
    mode: String,
    is_dark_mode: bool,
    colors: BTreeMap<String, MatugenRole>,
}

#[derive(Deserialize)]
struct MatugenRole {
    default: JsonColor,
    dark: JsonColor,
    light: JsonColor,
}

#[derive(Deserialize)]
struct JsonColor {
    color: String,
}

fn parse_palette(bytes: &[u8], dark: bool) -> Result<(u32, BTreeMap<String, u32>), String> {
    let json: MatugenJson =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid Matugen JSON palette: {e}"))?;
    if json.mode != if dark { "dark" } else { "light" } || json.is_dark_mode != dark {
        return Err("Matugen returned the wrong light/dark mode".into());
    }
    let mut colors = BTreeMap::new();
    for (name, role) in json.colors {
        let default = parse_hex(&role.default.color)?;
        let light = parse_hex(&role.light.color)?;
        let dark_color = parse_hex(&role.dark.color)?;
        if default != if dark { dark_color } else { light } {
            return Err(format!(
                "Matugen color '{name}' disagrees with the selected mode"
            ));
        }
        colors.insert(name, default);
    }
    let source = colors
        .get("source_color")
        .copied()
        .ok_or("Matugen omitted the selected source color")?;
    Ok((source, colors))
}

fn parse_hex(text: &str) -> Result<u32, String> {
    let hex = text
        .strip_prefix('#')
        .ok_or("Matugen returned a non-hex color")?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Matugen returned a non-opaque or malformed RGB color".into());
    }
    u32::from_str_radix(hex, 16).map_err(|_| "Matugen returned an invalid RGB color".into())
}

fn expected_template(
    template: &str,
    colors: &BTreeMap<String, u32>,
    dark: bool,
) -> Result<String, String> {
    let mut rendered = String::with_capacity(template.len());
    let template = template.replace("{{mode}}", if dark { "dark" } else { "light" });
    let mut rest = template.as_str();
    while let Some((before, expression)) = rest.split_once("{{colors.") {
        rendered.push_str(before);
        let (role, after) = expression
            .split_once(".default.hex}}")
            .ok_or("Invalid built-in Matugen template expression")?;
        let color = colors
            .get(role)
            .filter(|color| **color <= 0xffffff)
            .ok_or_else(|| format!("Matugen palette is missing valid '{role}'"))?;
        write!(rendered, "#{color:06x}").expect("writing to a String cannot fail");
        rest = after;
    }
    rendered.push_str(rest);
    Ok(rendered)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    schema_version: u32,
    theme: GeneratedTheme,
}

pub fn load_cache(path: &Path) -> Result<Option<GeneratedTheme>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "Cannot inspect theme cache '{}': {e}",
                path.display()
            ));
        }
        Ok(_) => {}
    }
    let text = read_text(path, CACHE_LIMIT)?;
    let cache: Cache = serde_json::from_str(&text)
        .map_err(|e| format!("Corrupt theme cache '{}': {e}", path.display()))?;
    if cache.schema_version != CACHE_VERSION {
        return Err(format!(
            "Unsupported theme cache schema {} in '{}'",
            cache.schema_version,
            path.display()
        ));
    }
    cache
        .theme
        .validate()
        .map_err(|e| format!("Invalid theme cache '{}': {e}", path.display()))?;
    Ok(Some(cache.theme))
}

/// A validated and flushed cache candidate. Dropping it discards the staged file.
pub struct PreparedCache {
    file: tempfile::NamedTempFile,
    path: PathBuf,
    directory: File,
}

#[must_use = "sync after releasing the request acceptance guard"]
pub struct PublishedCache {
    directory: File,
}

impl PublishedCache {
    /// Durability I/O must run after releasing the UI-shared acceptance guard.
    pub fn sync(self) -> Result<(), String> {
        self.directory
            .sync_all()
            .map_err(|e| format!("Cannot sync theme cache directory: {e}"))
    }
}

impl PreparedCache {
    /// Atomically publish while the caller holds its newest-request acceptance guard.
    pub fn commit(self) -> Result<PublishedCache, String> {
        self.file
            .persist(&self.path)
            .map_err(|e| format!("Cannot publish theme cache '{}': {e}", self.path.display()))?;
        Ok(PublishedCache {
            directory: self.directory,
        })
    }
}

pub fn prepare_cache(path: &Path, theme: &GeneratedTheme) -> Result<PreparedCache, String> {
    theme.validate()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create theme cache directory: {e}"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("Cannot create atomic theme cache: {e}"))?;
    #[derive(Serialize)]
    struct CacheRef<'a> {
        schema_version: u32,
        theme: &'a GeneratedTheme,
    }
    serde_json::to_writer(
        &mut file,
        &CacheRef {
            schema_version: CACHE_VERSION,
            theme,
        },
    )
    .map_err(|e| format!("Cannot encode theme cache: {e}"))?;
    file.flush()
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| format!("Cannot flush theme cache: {e}"))?;
    if file.as_file().metadata().map_err(|e| e.to_string())?.len() > CACHE_LIMIT as u64 {
        return Err("Generated theme cache exceeds the size limit".into());
    }
    let directory =
        File::open(parent).map_err(|e| format!("Cannot open theme cache directory: {e}"))?;
    Ok(PreparedCache {
        file,
        path: path.to_owned(),
        directory,
    })
}

fn read_text(path: &Path, limit: usize) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot read theme file '{}': {e}", path.display()))?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(format!(
            "Theme file '{}' is not a regular file within the size limit",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Cannot read theme file '{}': {e}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!(
            "Theme file '{}' exceeds the size limit",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map_err(|e| format!("Theme file '{}' is not UTF-8: {e}", path.display()))
}

struct Process(Child);

impl Process {
    fn status(&self) -> Result<Option<rustix::process::WaitIdStatus>, String> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = Pid::from_raw(self.0.id() as i32).ok_or("Invalid Matugen process ID")?;
        loop {
            match waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Err(rustix::io::Errno::INTR) => continue,
                result => {
                    return result.map_err(|e| format!("Cannot observe Matugen process: {e}"));
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Do not reap before signaling: descendants may hold pipes open and reaping
        // first would allow the process-group ID to be reused by an unrelated job.
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn supervise(command: &mut Command) {
    command.process_group(0);
    #[cfg(target_os = "linux")]
    {
        let parent = rustix::process::getpid();
        // SAFETY: after fork this closure invokes only prctl/getppid/getpid/kill
        // syscalls and errno conversion, without allocation or acquiring locks.
        unsafe {
            command.pre_exec(move || {
                use rustix::process::{
                    Signal, getpid, getppid, kill_process, set_parent_process_death_signal,
                };
                set_parent_process_death_signal(Some(Signal::KILL)).map_err(io::Error::from)?;
                // Close the race where the spawning process exits before prctl.
                if getppid() != Some(parent) {
                    kill_process(getpid(), Signal::KILL).map_err(io::Error::from)?;
                    return Err(io::Error::from_raw_os_error(
                        rustix::io::Errno::CHILD.raw_os_error(),
                    ));
                }
                Ok(())
            });
        }
    }
}

fn command_output(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Matugen generation cancelled".into());
    }
    let started = Instant::now();
    supervise(command);
    let child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                "Matugen is not installed or not on PATH; install matugen to generate the shell palette".into()
            } else {
                format!("Cannot start Matugen: {e}")
            }
        })?;
    let mut child = Process(child);
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or("Cannot capture Matugen stdout")?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or("Cannot capture Matugen stderr")?;
    for fd in [
        &stdout as &dyn std::os::fd::AsFd,
        &stderr as &dyn std::os::fd::AsFd,
    ] {
        let flags = rustix::fs::fcntl_getfl(fd)
            .map_err(|e| format!("Cannot inspect Matugen output pipe: {e}"))?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|e| format!("Cannot bound Matugen output reads: {e}"))?;
    }
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut status = None;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("Matugen generation cancelled".into());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "Matugen exceeded its {} second generation deadline",
                timeout.as_secs()
            ));
        }
        let progress = read_pipe(&mut stdout, &mut output, &mut stdout_eof, limit)?
            | read_pipe(&mut stderr, &mut errors, &mut stderr_eof, STDERR_LIMIT)?;
        if status.is_none() {
            status = child.status()?;
        }
        if let Some(status) = status {
            if stdout_eof && stderr_eof {
                if status.exit_status() == Some(0) {
                    return Ok(output);
                }
                let detail = String::from_utf8_lossy(&errors);
                let detail: String = detail
                    .chars()
                    .filter(|ch| !ch.is_control() || *ch == '\n')
                    .take(2000)
                    .collect();
                return Err(format!("Matugen generation failed: {}", detail.trim()));
            }
        }
        if !progress {
            std::thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
            );
        }
    }
}

fn read_pipe(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    eof: &mut bool,
    limit: usize,
) -> Result<bool, String> {
    if *eof {
        return Ok(false);
    }
    let mut buffer = [0u8; 8192];
    match reader.read(&mut buffer) {
        Ok(0) => {
            *eof = true;
            Ok(false)
        }
        Ok(count) => {
            if count > limit.saturating_sub(output.len()) {
                return Err("Matugen output exceeded its size limit".into());
            }
            output.extend_from_slice(&buffer[..count]);
            Ok(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(format!("Cannot read Matugen output: {e}")),
    }
}

#[cfg(test)]
#[path = "matugen_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) fn test_theme() -> GeneratedTheme {
    tests::fixture()
}
