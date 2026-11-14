use super::Result;
use libloading::Library;
use std::ffi::{CStr, c_char, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub(super) trait Desktop: Send {
    /// The user's override, not the effective value inherited from system defaults.
    fn get(&mut self, key: &str) -> Result<Option<String>>;
    fn set(&mut self, key: &str, value: Option<&str>) -> Result<()>;
}

// Gio backends/global caches may retain callbacks after the last local GSettings object.
// Never unload the DSO while the process is alive, including after a GTK sync error.
static GIO_LIBRARY: LazyLock<Result<Library>> = LazyLock::new(|| {
    // SAFETY: Gio is loaded only for an opted-in GTK operation, never unloaded afterward.
    unsafe { Library::new("libgio-2.0.so.0") }
        .map_err(|e| format!("GTK integration requires GLib/Gio (libgio-2.0.so.0): {e}"))
});
static GIO: LazyLock<Result<Gio>> = LazyLock::new(Gio::load);

type Source = unsafe extern "C" fn() -> *mut c_void;
type Lookup = unsafe extern "C" fn(*mut c_void, *const c_char, i32) -> *mut c_void;
type HasKey = unsafe extern "C" fn(*mut c_void, *const c_char) -> i32;
type SchemaPath = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type New = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void;
type Get = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type Print = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_char;
type Release = unsafe extern "C" fn(*mut c_void);

struct Gio {
    _library: &'static Library,
    source: Source,
    lookup: Lookup,
    has_key: HasKey,
    path: SchemaPath,
    new: New,
    get: Get,
    print: Print,
    schema_unref: Release,
    object_unref: Release,
    variant_unref: Release,
    free: Release,
}

impl Gio {
    fn load() -> Result<Self> {
        // SAFETY: These types exactly match the public GLib/Gio C declarations.
        // The owning Library is retained alongside the copied function pointers.
        unsafe {
            let library = GIO_LIBRARY.as_ref().map_err(Clone::clone)?;
            Ok(Self {
                source: *library
                    .get::<Source>(b"g_settings_schema_source_get_default\0")
                    .map_err(|e| e.to_string())?,
                lookup: *library
                    .get::<Lookup>(b"g_settings_schema_source_lookup\0")
                    .map_err(|e| e.to_string())?,
                has_key: *library
                    .get::<HasKey>(b"g_settings_schema_has_key\0")
                    .map_err(|e| e.to_string())?,
                path: *library
                    .get::<SchemaPath>(b"g_settings_schema_get_path\0")
                    .map_err(|e| e.to_string())?,
                new: *library
                    .get::<New>(b"g_settings_new_full\0")
                    .map_err(|e| e.to_string())?,
                get: *library
                    .get::<Get>(b"g_settings_get_user_value\0")
                    .map_err(|e| e.to_string())?,
                print: *library
                    .get::<Print>(b"g_variant_print\0")
                    .map_err(|e| e.to_string())?,
                schema_unref: *library
                    .get::<Release>(b"g_settings_schema_unref\0")
                    .map_err(|e| e.to_string())?,
                object_unref: *library
                    .get::<Release>(b"g_object_unref\0")
                    .map_err(|e| e.to_string())?,
                variant_unref: *library
                    .get::<Release>(b"g_variant_unref\0")
                    .map_err(|e| e.to_string())?,
                free: *library
                    .get::<Release>(b"g_free\0")
                    .map_err(|e| e.to_string())?,
                _library: library,
            })
        }
    }
}

pub(super) struct GSettings;
impl GSettings {
    fn user_value(key: &str) -> Result<Option<String>> {
        let key = match key {
            "gtk-theme" => c"gtk-theme",
            "color-scheme" => c"color-scheme",
            _ => return Err("Out-of-scope GTK preference key".into()),
        };
        // Gio's documented get_user_value API distinguishes an explicit default value from
        // no override, unlike `gsettings get` or a merged dconf read:
        // https://docs.gtk.org/gio/method.Settings.get_user_value.html
        // SAFETY: Every symbol below has the exact public GLib/Gio C ABI. The static
        // library remains loaded for process lifetime, each nullable pointer is checked,
        // and every owned schema/object/variant/string is released before scope exit.
        unsafe {
            let gio = GIO.as_ref().map_err(Clone::clone)?;
            let source = (gio.source)();
            if source.is_null() {
                return Err("GTK integration needs the org.gnome.desktop.interface GSettings schema; install gsettings-desktop-schemas".into());
            }
            let schema = (gio.lookup)(source, c"org.gnome.desktop.interface".as_ptr(), 1);
            if schema.is_null() {
                return Err("GTK integration needs org.gnome.desktop.interface; install gsettings-desktop-schemas".into());
            }
            // g_settings_new() aborts for a missing schema. Lookup, key and fixed-path
            // checks prevent GLib programmer-error aborts with incomplete installations.
            if (gio.path)(schema).is_null() || (gio.has_key)(schema, key.as_ptr()) == 0 {
                (gio.schema_unref)(schema);
                return Err(
                    "GTK desktop schema is incomplete; install/update gsettings-desktop-schemas"
                        .into(),
                );
            }
            let settings = (gio.new)(schema, std::ptr::null_mut(), std::ptr::null());
            (gio.schema_unref)(schema);
            if settings.is_null() {
                return Err("Cannot create GTK desktop settings".into());
            }
            let value = (gio.get)(settings, key.as_ptr());
            (gio.object_unref)(settings);
            if value.is_null() {
                return Ok(None);
            }
            let printed = (gio.print)(value, 0);
            (gio.variant_unref)(value);
            if printed.is_null() {
                return Err("Cannot serialize the GTK desktop override".into());
            }
            let result = CStr::from_ptr(printed)
                .to_str()
                .map(str::to_owned)
                .map_err(|e| e.to_string());
            (gio.free)(printed.cast());
            result.map(Some)
        }
    }

    fn run(args: &[&str]) -> Result<String> {
        let stdout_file = tempfile::tempfile().map_err(|e| e.to_string())?;
        let stderr_file = tempfile::tempfile().map_err(|e| e.to_string())?;
        let mut command = Command::new("gsettings");
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout_file.try_clone().map_err(|e| e.to_string())?)
            .stderr(stderr_file.try_clone().map_err(|e| e.to_string())?);
        let parent = rustix::process::getpid();
        // SAFETY: This pre-exec closure only uses async-signal-safe prctl/getppid/kill
        // operations and constructs an allocation-free OS error on the failure path.
        unsafe {
            command.pre_exec(move || {
                use rustix::process::{
                    Signal, getpid, getppid, kill_process, set_parent_process_death_signal,
                };
                set_parent_process_death_signal(Some(Signal::KILL))
                    .map_err(std::io::Error::from)?;
                if getppid() != Some(parent) {
                    kill_process(getpid(), Signal::KILL).map_err(std::io::Error::from)?;
                    return Err(std::io::Error::from_raw_os_error(
                        rustix::io::Errno::CHILD.raw_os_error(),
                    ));
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| {
            format!("Cannot run gsettings (install glib2 and use a desktop session): {e}")
        })?;
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("gsettings failed or exceeded its 2-second deadline".into());
                }
            }
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut stdout_file = stdout_file;
        let mut stderr_file = stderr_file;
        stdout_file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        stderr_file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        stdout_file
            .take(8192)
            .read_to_string(&mut stdout)
            .map_err(|e| e.to_string())?;
        stderr_file
            .take(8192)
            .read_to_string(&mut stderr)
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("gsettings failed: {}", stderr.trim()));
        }
        Ok(stdout.trim().to_owned())
    }
}

impl Desktop for GSettings {
    fn get(&mut self, key: &str) -> Result<Option<String>> {
        Self::user_value(key)
    }
    fn set(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        if let Some(value) = value {
            Self::run(&["set", "org.gnome.desktop.interface", key, value])?;
        } else {
            Self::run(&["reset", "org.gnome.desktop.interface", key])?;
        }
        if self.get(key)?.as_deref() != value {
            return Err(format!(
                "gsettings did not retain {key}; check the session D-Bus/dconf service"
            ));
        }
        Ok(())
    }
}
