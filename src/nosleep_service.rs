//! Session-shared nosleep monitor and manual toggle runner. Observes systemd user units via
//! D-Bus and `$XDG_RUNTIME_DIR/nosleep-agents` via read-only `inotify` without idle polling.

use anti_webui::prelude::*;
use async_channel::{Receiver, Sender};
use futures_lite::{StreamExt, future};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::{Errno, read, write};
use rustix::pipe::{PipeFlags, pipe_with};
use serde_json::Value;
use std::cell::RefCell;
use std::fs;
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};
use zbus::{
    Connection, MatchRule, MessageStream, Proxy, fdo, proxy::CacheProperties,
    zvariant::OwnedObjectPath,
};

const SYSTEMD_NAME: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_UNIT: &str = "org.freedesktop.systemd1.Unit";
const MANUAL_UNIT: &str = "nosleep.service";
const AGENTS_UNIT: &str = "nosleep-agents.service";

const STALE_AFTER: Duration = Duration::from_secs(10);
const STALE_LEEWAY: Duration = Duration::from_millis(200);
const HERDR_TIMEOUT: Duration = Duration::from_secs(3);
const HERDR_OUTPUT_LIMIT: usize = 1024 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(12);
const CONTROL_OUTPUT_LIMIT: usize = 16 * 1024;
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(3);
const SYSTEMCTL_OUTPUT_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub manual_active: bool,
    pub manual_busy: bool,
    pub saved_profile: Option<String>,
    pub auto_active: bool,
    pub watcher_active: bool,
    pub watcher_loaded: bool,
    pub lock_deferred: bool,
    pub suspend_deferred: bool,
    pub agents: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateFiles {
    pub working_fresh: bool,
    pub next_expiry: Option<Duration>,
    pub lock_deferred: bool,
    pub suspend_deferred: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlRequest {
    Set(bool),
    Toggle,
}

#[derive(Default)]
struct SharedState {
    manual_active: bool,
    manual_busy: bool,
    saved_profile: Option<String>,
    watcher_active: bool,
    watcher_loaded: bool,
    state_files: StateFiles,
    agents: Vec<String>,
    bus_error: Option<String>,
    herdr_error: Option<String>,
    control_error: Option<String>,
    last_published: Option<Snapshot>,
}

impl SharedState {
    fn snapshot(&self) -> Snapshot {
        let auto_active = self.state_files.working_fresh;
        Snapshot {
            manual_active: self.manual_active,
            manual_busy: self.manual_busy,
            saved_profile: self.saved_profile.clone(),
            auto_active,
            watcher_active: self.watcher_active,
            watcher_loaded: self.watcher_loaded,
            lock_deferred: auto_active && self.state_files.lock_deferred,
            suspend_deferred: auto_active && self.state_files.suspend_deferred,
            agents: if auto_active {
                self.agents.clone()
            } else {
                Vec::new()
            },
            error: self
                .control_error
                .clone()
                .or_else(|| self.herdr_error.clone())
                .or_else(|| self.bus_error.clone()),
        }
    }

    fn publish(&mut self, emit: &dyn Fn(Snapshot)) {
        let next = self.snapshot();
        if self.last_published.as_ref() != Some(&next) {
            self.last_published = Some(next.clone());
            emit(next);
        }
    }
}

struct HerdrGate {
    inner: Mutex<(bool, bool)>, // (pending, stopped)
    cv: Condvar,
}

impl HerdrGate {
    fn new() -> Self {
        Self {
            inner: Mutex::new((false, false)),
            cv: Condvar::new(),
        }
    }

    fn request(&self) {
        let mut guard = self.inner.lock().unwrap();
        if !guard.1 {
            guard.0 = true;
            self.cv.notify_one();
        }
    }

    fn stop(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.1 = true;
        self.cv.notify_all();
    }
}

struct ControlGate {
    inner: Mutex<(Option<ControlRequest>, bool)>,
    cv: Condvar,
}

impl ControlGate {
    fn new() -> Self {
        Self {
            inner: Mutex::new((None, false)),
            cv: Condvar::new(),
        }
    }

    fn submit(&self, req: ControlRequest) {
        let mut guard = self.inner.lock().unwrap();
        if !guard.1 {
            guard.0 = Some(req);
            self.cv.notify_one();
        }
    }

    fn stop(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.1 = true;
        self.cv.notify_all();
    }
}

struct ServiceHandle {
    stop: Arc<AtomicBool>,
    fs_refresh: Arc<AtomicBool>,
    fs_wake_tx: OwnedFd,
    bus_wake_tx: Sender<()>,
    bus_stop_tx: Sender<()>,
    herdr: Arc<HerdrGate>,
    control: Arc<ControlGate>,
}

impl ServiceHandle {
    fn wake_fs(&self, refresh_agents: bool) {
        if refresh_agents {
            self.fs_refresh.store(true, Ordering::SeqCst);
        }
        let _ = write(&self.fs_wake_tx, &[1u8]);
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = write(&self.fs_wake_tx, &[1u8]);
        let _ = self.bus_stop_tx.try_send(());
        self.herdr.stop();
        self.control.stop();
    }
}

thread_local! {
    static STATE: Signal<Snapshot> = create_root(|| signal(Snapshot::default())).0;
    static SERVICE: RefCell<Option<ServiceHandle>> = const { RefCell::new(None) };
}

/// Allocating observable state alone performs no I/O (including native previews).
pub(crate) fn state() -> Signal<Snapshot> {
    STATE.with(|s| *s)
}

/// Start session-shared monitoring once; safe across repeated calls.
pub(crate) fn install() {
    if SERVICE.with(|s| s.borrow().is_some()) {
        return;
    }
    let state = state();
    let shared = Arc::new(Mutex::new(SharedState::default()));
    let to_ui = ui_channel(move |snapshot: Snapshot| {
        state.set(snapshot);
    });

    let stop = Arc::new(AtomicBool::new(false));
    let fs_refresh = Arc::new(AtomicBool::new(false));
    let (fs_wake_rx, fs_wake_tx) = match pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK) {
        Ok(fds) => fds,
        Err(error) => {
            let mut s = shared.lock().unwrap();
            s.bus_error = Some(format!("Cannot initialize nosleep pipe: {error}"));
            s.publish(&|snap| to_ui.send(snap));
            return;
        }
    };
    let (bus_wake_tx, bus_wake_rx) = async_channel::bounded(4);
    let (bus_stop_tx, bus_stop_rx) = async_channel::bounded(1);
    let herdr = Arc::new(HerdrGate::new());
    let control = Arc::new(ControlGate::new());

    let runtime = runtime_dir();

    // 1) Systemd user D-Bus lane.
    {
        let shared = shared.clone();
        let to_ui = to_ui.clone();
        let _ = std::thread::Builder::new()
            .name("shoji nosleep bus".into())
            .spawn(move || {
                future::block_on(future::race(bus_lane(shared, to_ui, bus_wake_rx), async {
                    let _ = bus_stop_rx.recv().await;
                }));
            });
    }

    // 2) Herdr query worker.
    {
        let shared = shared.clone();
        let to_ui = to_ui.clone();
        let gate = herdr.clone();
        let _ = std::thread::Builder::new()
            .name("shoji nosleep herdr".into())
            .spawn(move || herdr_worker(gate, shared, to_ui));
    }

    // 3) Read-only inotify watcher on $XDG_RUNTIME_DIR and .../nosleep-agents.
    {
        let shared = shared.clone();
        let to_ui = to_ui.clone();
        let stop = stop.clone();
        let fs_refresh = fs_refresh.clone();
        let herdr = herdr.clone();
        let _ = std::thread::Builder::new()
            .name("shoji nosleep fs".into())
            .spawn(move || {
                fs_watch_loop(
                    &runtime,
                    &stop,
                    &fs_refresh,
                    fs_wake_rx,
                    || herdr.request(),
                    &shared,
                    &|snap| to_ui.send(snap),
                );
            });
    }

    // 4) Manual toggle command worker.
    {
        let shared = shared.clone();
        let to_ui = to_ui.clone();
        let gate = control.clone();
        let bus_wake = bus_wake_tx.clone();
        let _ = std::thread::Builder::new()
            .name("shoji nosleep ctl".into())
            .spawn(move || control_worker(gate, shared, to_ui, bus_wake));
    }

    SERVICE.with(|cell| {
        *cell.borrow_mut() = Some(ServiceHandle {
            stop,
            fs_refresh,
            fs_wake_tx,
            bus_wake_tx,
            bus_stop_tx,
            herdr,
            control,
        });
    });

    effect(move || {
        let active = state.with(|s| s.auto_active);
        crate::idle::set_agents_working(active);
    });

    on_cleanup(move || {
        SERVICE.with(|cell| cell.borrow_mut().take());
        state.set(Snapshot::default());
        crate::idle::set_agents_working(false);
    });
}

pub(crate) fn toggle_manual() {
    SERVICE.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            handle.control.submit(ControlRequest::Toggle);
        }
    });
}

pub(crate) fn set_manual(enabled: bool) {
    SERVICE.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            handle.control.submit(ControlRequest::Set(enabled));
        }
    });
}

pub(crate) fn refresh() {
    SERVICE.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            let _ = handle.bus_wake_tx.try_send(());
            handle.wake_fs(true);
        }
    });
}

pub(crate) fn parse_working_agents(bytes: &[u8]) -> Result<Vec<String>, String> {
    let parsed: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid herdr agent list JSON: {e}"))?;
    let agents = parsed
        .get("result")
        .and_then(|r| r.get("agents"))
        .and_then(Value::as_array)
        .ok_or_else(|| "Herdr returned an invalid agent list".to_owned())?;

    let mut working = Vec::new();
    for item in agents {
        let Some(record) = item.as_object() else {
            continue;
        };
        if record.get("agent_status").and_then(Value::as_str) != Some("working") {
            continue;
        }
        let agent_name = record
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let pane_id = record
            .get("pane_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let title = record
            .get("terminal_title_stripped")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(pane_id);
        let label = match (agent_name.is_empty(), title.is_empty()) {
            (false, false) => format!("{agent_name} · {title}"),
            (false, true) => agent_name.to_owned(),
            (true, false) => title.to_owned(),
            (true, true) => "Working agent".to_owned(),
        };
        working.push(label);
    }
    Ok(working)
}

pub(crate) fn inspect_state_dir(state_dir: &Path, now: SystemTime) -> StateFiles {
    let mut files = StateFiles::default();
    if let Ok(meta) = fs::metadata(state_dir.join("working"))
        && let Ok(mtime) = meta.modified()
    {
        let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
        if age <= STALE_AFTER {
            files.working_fresh = true;
            files.next_expiry = Some(STALE_AFTER.saturating_sub(age) + STALE_LEEWAY);
        }
    }
    files.lock_deferred = state_dir.join("lock-deferred").exists();
    files.suspend_deferred = state_dir.join("suspend-deferred").exists();
    files
}

pub(crate) fn parse_saved_profile(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') {
        let parsed: Value = serde_json::from_str(trimmed).ok()?;
        let previous = parsed.get("previous")?.as_str()?.trim();
        return (!previous.is_empty()).then(|| previous.to_owned());
    }
    Some(trimmed.to_owned())
}

fn read_saved_profile(runtime_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(runtime_dir.join("nosleep-prev-power-profile")).ok()?;
    parse_saved_profile(&raw)
}

fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
}

fn resolve_bun() -> PathBuf {
    if let Some(home) = home_dir() {
        let candidate = home.join(".bun/bin/bun");
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    PathBuf::from("bun")
}

fn resolve_control_script() -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(".local/libexec/nosleep-control.ts");
    }
    PathBuf::from(".local/libexec/nosleep-control.ts")
}

fn resolve_herdr() -> PathBuf {
    if let Some(home) = home_dir() {
        let candidate = home.join(".local/bin/herdr");
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    PathBuf::from("herdr")
}

struct ChildGuard(Child);

impl ChildGuard {
    fn status(&self) -> Result<Option<rustix::process::WaitIdStatus>, String> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = Pid::from_raw(self.0.id() as i32)
            .ok_or_else(|| "Invalid child process id".to_owned())?;
        loop {
            match waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Err(Errno::INTR) => continue,
                Ok(status) => return Ok(status),
                Err(e) => return Err(format!("Process wait failed: {e}")),
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct CommandOutput {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn set_nonblocking(fd: &impl AsFd) -> Result<(), String> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(|e| format!("fcntl_getfl failed: {e}"))?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|e| format!("fcntl_setfl failed: {e}"))
}

fn drain_pipe(
    pipe: &mut impl Read,
    eof: &mut bool,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<usize, String> {
    if *eof {
        return Ok(0);
    }
    let mut total = 0usize;
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => {
                *eof = true;
                return Ok(total);
            }
            Ok(n) => {
                total += n;
                let room = limit.saturating_sub(out.len());
                if room > 0 {
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
                if out.len() >= limit && n > room {
                    return Err("Command output exceeded limit".to_owned());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(total),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("Pipe read failed: {e}")),
        }
    }
}

fn run_bounded(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<CommandOutput, String> {
    let started = Instant::now();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Could not start command: {e}"))?;
    let mut child = ChildGuard(child);
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| "Missing stdout pipe".to_owned())?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| "Missing stderr pipe".to_owned())?;
    set_nonblocking(&stdout)?;
    set_nonblocking(&stderr)?;

    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let mut out_eof = false;
    let mut err_eof = false;
    let mut status = None;

    loop {
        if started.elapsed() >= timeout {
            return Err("Command timed out".to_owned());
        }
        let read_out = drain_pipe(&mut stdout, &mut out_eof, &mut out_buf, limit)?;
        let read_err = drain_pipe(&mut stderr, &mut err_eof, &mut err_buf, limit)?;

        if status.is_none() {
            status = child.status()?;
        }
        if let Some(status) = status
            && out_eof
            && err_eof
        {
            return Ok(CommandOutput {
                code: status.exit_status().map(|c| c as i32),
                stdout: out_buf,
                stderr: err_buf,
            });
        }
        if read_out == 0 && read_err == 0 {
            let remaining = timeout.saturating_sub(started.elapsed());
            let mut fds = Vec::with_capacity(2);
            if !out_eof {
                fds.push(PollFd::new(&stdout, PollFlags::IN));
            }
            if !err_eof {
                fds.push(PollFd::new(&stderr, PollFlags::IN));
            }
            if fds.is_empty() {
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            } else {
                let wait = Duration::from_millis(25).min(remaining);
                let ts = Timespec {
                    tv_sec: wait.as_secs() as _,
                    tv_nsec: wait.subsec_nanos() as _,
                };
                match poll(&mut fds, Some(&ts)) {
                    Err(Errno::INTR) | Ok(_) => {}
                    Err(e) => return Err(format!("poll failed: {e}")),
                }
            }
        }
    }
}

fn query_herdr() -> Result<Vec<String>, String> {
    let mut cmd = Command::new(resolve_herdr());
    cmd.args(["agent", "list"]);
    let output = run_bounded(&mut cmd, HERDR_TIMEOUT, HERDR_OUTPUT_LIMIT)?;
    if output.code != Some(0) {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("herdr agent list exited {}", output.code.unwrap_or(-1))
        } else {
            stderr
        });
    }
    parse_working_agents(&output.stdout)
}

fn herdr_worker(gate: Arc<HerdrGate>, shared: Arc<Mutex<SharedState>>, to_ui: UiSender<Snapshot>) {
    loop {
        {
            let mut guard = gate.inner.lock().unwrap();
            while !guard.0 && !guard.1 {
                guard = gate.cv.wait(guard).unwrap();
            }
            if guard.1 {
                return;
            }
            guard.0 = false;
        }
        let result = query_herdr();
        let mut s = shared.lock().unwrap();
        if s.state_files.working_fresh {
            match result {
                Ok(agents) => {
                    s.agents = agents;
                    s.herdr_error = None;
                }
                Err(error) => {
                    s.herdr_error = Some(error);
                }
            }
        } else {
            s.agents.clear();
            s.herdr_error = None;
        }
        s.publish(&|snap| to_ui.send(snap));
    }
}

fn control_worker(
    gate: Arc<ControlGate>,
    shared: Arc<Mutex<SharedState>>,
    to_ui: UiSender<Snapshot>,
    bus_wake: Sender<()>,
) {
    loop {
        let request = {
            let mut guard = gate.inner.lock().unwrap();
            while guard.0.is_none() && !guard.1 {
                guard = gate.cv.wait(guard).unwrap();
            }
            if guard.1 {
                return;
            }
            guard.0.take().unwrap()
        };

        let action = {
            let mut s = shared.lock().unwrap();
            let action = match request {
                ControlRequest::Set(true) => "start",
                ControlRequest::Set(false) => "stop",
                ControlRequest::Toggle => {
                    if s.manual_active {
                        "stop"
                    } else {
                        "start"
                    }
                }
            };
            s.manual_busy = true;
            s.control_error = None;
            s.publish(&|snap| to_ui.send(snap));
            action
        };

        let script = resolve_control_script();
        let result = if !script.is_file() {
            Err(format!("Missing {}", script.display()))
        } else {
            let mut cmd = Command::new(resolve_bun());
            cmd.arg(&script).arg(action);
            run_bounded(&mut cmd, CONTROL_TIMEOUT, CONTROL_OUTPUT_LIMIT).and_then(|out| {
                if out.code == Some(0) {
                    Ok(())
                } else {
                    let msg = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    let cleaned = msg.strip_prefix("nosleep：").unwrap_or(&msg).trim();
                    Err(if cleaned.is_empty() {
                        format!(
                            "nosleep-control.ts {action} failed ({})",
                            out.code.unwrap_or(-1)
                        )
                    } else {
                        cleaned.to_owned()
                    })
                }
            })
        };

        let refreshed = query_units_cli();
        let profile = read_saved_profile(&runtime_dir());

        {
            let mut s = shared.lock().unwrap();
            s.manual_busy = false;
            s.saved_profile = profile;
            s.control_error = result.err();
            if let Ok(units) = refreshed {
                s.manual_active = units.manual_active;
                s.watcher_loaded = units.watcher_loaded;
                s.watcher_active = units.watcher_active;
            }
            s.publish(&|snap| to_ui.send(snap));
        }
        let _ = bus_wake.try_send(());
    }
}

fn drain_pipe_nonblock(fd: &OwnedFd) {
    let mut buf = [0u8; 64];
    loop {
        match read(fd, &mut buf) {
            Ok(0) | Err(Errno::WOULDBLOCK) => break,
            Ok(_) => continue,
            Err(Errno::INTR) => continue,
            Err(_) => break,
        }
    }
}

fn sync_state_watch(inotify_fd: &OwnedFd, state_dir: &Path, state_wd: &mut Option<i32>) {
    if state_wd.is_some() || !state_dir.is_dir() {
        return;
    }
    // Deliberately omit ATTRIB: nosleep-agents.ts touches mtime every 2s via utimesSync.
    let flags = WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_TO
        | WatchFlags::MOVED_FROM
        | WatchFlags::CLOSE_WRITE
        | WatchFlags::DELETE_SELF
        | WatchFlags::MOVE_SELF;
    if let Ok(wd) = inotify::add_watch(inotify_fd, state_dir, flags) {
        *state_wd = Some(wd);
    }
}

fn fs_watch_loop(
    runtime_dir: &Path,
    stop: &AtomicBool,
    fs_refresh: &AtomicBool,
    wake_rx: OwnedFd,
    on_agents_refresh: impl Fn(),
    shared: &Arc<Mutex<SharedState>>,
    emit: &dyn Fn(Snapshot),
) {
    let inotify_fd = match inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK) {
        Ok(fd) => fd,
        Err(error) => {
            let mut s = shared.lock().unwrap();
            s.bus_error = Some(format!("Cannot initialize nosleep inotify: {error}"));
            s.publish(emit);
            return;
        }
    };
    let runtime_flags =
        WatchFlags::CREATE | WatchFlags::DELETE | WatchFlags::MOVED_TO | WatchFlags::MOVED_FROM;
    if let Err(error) = inotify::add_watch(&inotify_fd, runtime_dir, runtime_flags) {
        let mut s = shared.lock().unwrap();
        s.bus_error = Some(format!("Cannot watch {}: {error}", runtime_dir.display()));
        s.publish(emit);
        return;
    }

    let state_dir = runtime_dir.join("nosleep-agents");
    let mut state_wd: Option<i32> = None;
    let mut was_working = false;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }

        // Register the state-dir watch BEFORE stat'ing its files so no creation is missed.
        sync_state_watch(&inotify_fd, &state_dir, &mut state_wd);
        let state_files = inspect_state_dir(&state_dir, SystemTime::now());
        let saved_profile = read_saved_profile(runtime_dir);
        let manual_refresh = fs_refresh.swap(false, Ordering::SeqCst);
        let became_working = state_files.working_fresh && !was_working;
        was_working = state_files.working_fresh;

        {
            let mut s = shared.lock().unwrap();
            s.state_files = state_files;
            s.saved_profile = saved_profile;
            if !state_files.working_fresh {
                s.agents.clear();
                s.herdr_error = None;
            }
            s.publish(emit);
        }

        if state_files.working_fresh && (became_working || manual_refresh) {
            on_agents_refresh();
        }

        let timeout = state_files.next_expiry.map(|d| Timespec {
            tv_sec: d.as_secs() as _,
            tv_nsec: d.subsec_nanos() as _,
        });
        let mut fds = [
            PollFd::new(&inotify_fd, PollFlags::IN),
            PollFd::new(&wake_rx, PollFlags::IN),
        ];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => {
                let mut s = shared.lock().unwrap();
                s.bus_error = Some(format!("nosleep fs poll failed: {error}"));
                s.publish(emit);
                return;
            }
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if fds[1].revents().intersects(PollFlags::IN) {
            drain_pipe_nonblock(&wake_rx);
        }
        if fds[0].revents().intersects(PollFlags::IN) {
            let mut buf = [MaybeUninit::<u8>::uninit(); 1024];
            let mut reader = inotify::Reader::new(&inotify_fd, &mut buf);
            loop {
                match reader.next() {
                    Ok(ev) => {
                        if Some(ev.wd()) == state_wd
                            && ev.events().intersects(
                                ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF | ReadFlags::IGNORED,
                            )
                        {
                            state_wd = None;
                        }
                    }
                    Err(Errno::WOULDBLOCK) => break,
                    Err(Errno::INTR) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UnitStatus {
    manual_active: bool,
    watcher_loaded: bool,
    watcher_active: bool,
}

fn is_unit_absent(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(err, _, _)
            if err.as_str() == "org.freedesktop.systemd1.NoSuchUnit"
                || err.as_str() == "org.freedesktop.DBus.Error.UnknownObject"
    )
}

async fn query_unit_dbus(
    connection: &Connection,
    manager: &Proxy<'_>,
    name: &str,
    load_if_missing: bool,
) -> Result<Option<(OwnedObjectPath, String, String)>, zbus::Error> {
    let reply: Result<OwnedObjectPath, zbus::Error> = if load_if_missing {
        manager.call("LoadUnit", &(name,)).await
    } else {
        manager.call("GetUnit", &(name,)).await
    };
    let path = match reply {
        Ok(path) => path,
        Err(ref err) if !load_if_missing && is_unit_absent(err) => return Ok(None),
        Err(err) => return Err(err),
    };
    let unit: Proxy<'_> = zbus::proxy::Builder::<Proxy<'_>>::new(connection)
        .destination(SYSTEMD_NAME)?
        .path(path.as_ref())?
        .interface(SYSTEMD_UNIT)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let load_state: String = match unit.get_property("LoadState").await {
        Ok(val) => val,
        Err(ref err) if !load_if_missing && is_unit_absent(err) => return Ok(None),
        Err(err) => return Err(err),
    };
    let active_state: String = match unit.get_property("ActiveState").await {
        Ok(val) => val,
        Err(ref err) if !load_if_missing && is_unit_absent(err) => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(Some((path, load_state, active_state)))
}

async fn query_units_dbus(
    connection: &Connection,
    manager: &Proxy<'_>,
) -> Result<(UnitStatus, Option<OwnedObjectPath>, Option<OwnedObjectPath>), zbus::Error> {
    let manual = query_unit_dbus(connection, manager, MANUAL_UNIT, false).await?;
    let agents = query_unit_dbus(connection, manager, AGENTS_UNIT, true).await?;
    let status = UnitStatus {
        manual_active: manual
            .as_ref()
            .is_some_and(|(_, _, active)| matches!(active.as_str(), "active" | "activating")),
        watcher_loaded: agents.as_ref().is_some_and(|(_, load, _)| load == "loaded"),
        watcher_active: agents
            .as_ref()
            .is_some_and(|(_, _, active)| active == "active"),
    };
    Ok((status, manual.map(|(p, _, _)| p), agents.map(|(p, _, _)| p)))
}

fn query_units_cli() -> Result<UnitStatus, String> {
    let mut cmd = Command::new("systemctl");
    cmd.args([
        "--user",
        "show",
        MANUAL_UNIT,
        AGENTS_UNIT,
        "--property=Id,LoadState,ActiveState",
    ]);
    let output = run_bounded(&mut cmd, SYSTEMCTL_TIMEOUT, SYSTEMCTL_OUTPUT_LIMIT)?;
    if output.code != Some(0) {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("systemctl --user show exited {}", output.code.unwrap_or(-1))
        } else {
            stderr
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut status = UnitStatus::default();
    for block in text.split("\n\n") {
        let mut id = "";
        let mut load = "";
        let mut active = "";
        for line in block.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "Id" => id = v,
                    "LoadState" => load = v,
                    "ActiveState" => active = v,
                    _ => {}
                }
            }
        }
        if id == MANUAL_UNIT {
            status.manual_active = load != "not-found" && matches!(active, "active" | "activating");
        } else if id == AGENTS_UNIT {
            status.watcher_loaded = load == "loaded";
            status.watcher_active = active == "active";
        }
    }
    Ok(status)
}

fn apply_unit_status(
    shared: &Arc<Mutex<SharedState>>,
    to_ui: &UiSender<Snapshot>,
    result: Result<UnitStatus, String>,
) {
    let mut s = shared.lock().unwrap();
    match result {
        Ok(units) => {
            s.manual_active = units.manual_active;
            s.watcher_loaded = units.watcher_loaded;
            s.watcher_active = units.watcher_active;
            s.bus_error = None;
        }
        Err(error) => {
            s.bus_error = Some(error);
        }
    }
    s.publish(&|snap| to_ui.send(snap));
}

async fn bus_lane(
    shared: Arc<Mutex<SharedState>>,
    to_ui: UiSender<Snapshot>,
    wake_rx: Receiver<()>,
) {
    let setup = async {
        let connection = zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(3))
            .build()
            .await?;
        let manager: Proxy<'_> = zbus::proxy::Builder::<Proxy<'_>>::new(&connection)
            .destination(SYSTEMD_NAME)?
            .path(SYSTEMD_PATH)?
            .interface(SYSTEMD_MANAGER)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let _: () = manager.call("Subscribe", &()).await?;

        let owner = fdo::DBusProxy::new(&connection)
            .await?
            .get_name_owner(SYSTEMD_NAME.try_into()?)
            .await?;

        let manager_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner.as_str())?
            .path(SYSTEMD_PATH)?
            .interface(SYSTEMD_MANAGER)?
            .build();
        let manager_stream = MessageStream::for_match_rule(manager_rule, &connection, None).await?;

        let props_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner.as_str())?
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .arg(0, SYSTEMD_UNIT)?
            .build();
        let props_stream = MessageStream::for_match_rule(props_rule, &connection, None).await?;

        let owner_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(SYSTEMD_NAME)?
            .build();
        let owner_stream = MessageStream::for_match_rule(owner_rule, &connection, None).await?;

        Ok::<_, zbus::Error>((
            connection,
            manager,
            manager_stream,
            props_stream,
            owner_stream,
        ))
    }
    .await;

    let (connection, manager, mut manager_stream, mut props_stream, mut owner_stream) = match setup
    {
        Ok(parts) => parts,
        Err(error) => {
            let cli = query_units_cli().map_err(|cli_err| format!("{error}; {cli_err}"));
            apply_unit_status(&shared, &to_ui, cli);
            while wake_rx.recv().await.is_ok() {
                while wake_rx.try_recv().is_ok() {}
                apply_unit_status(&shared, &to_ui, query_units_cli());
            }
            return;
        }
    };

    let (mut manual_path, mut agents_path) = match query_units_dbus(&connection, &manager).await {
        Ok((units, m_path, a_path)) => {
            apply_unit_status(&shared, &to_ui, Ok(units));
            (m_path, a_path)
        }
        Err(error) => {
            apply_unit_status(
                &shared,
                &to_ui,
                Err(format!("systemd query failed: {error}")),
            );
            (None, None)
        }
    };

    enum BusInput {
        Manager(Option<zbus::Result<zbus::Message>>),
        Props(Option<zbus::Result<zbus::Message>>),
        Owner(Option<zbus::Result<zbus::Message>>),
        Wake(bool),
    }

    loop {
        let input = future::race(
            future::race(
                async { BusInput::Manager(manager_stream.next().await) },
                async { BusInput::Props(props_stream.next().await) },
            ),
            future::race(
                async { BusInput::Owner(owner_stream.next().await) },
                async { BusInput::Wake(wake_rx.recv().await.is_ok()) },
            ),
        )
        .await;

        let should_query = match input {
            BusInput::Wake(false) => return,
            BusInput::Wake(true) => {
                while wake_rx.try_recv().is_ok() {}
                true
            }
            BusInput::Owner(Some(Ok(msg))) => msg
                .header()
                .sender()
                .is_some_and(|s| s.as_str() == "org.freedesktop.DBus"),
            BusInput::Owner(Some(Err(_))) | BusInput::Owner(None) => break,
            BusInput::Props(Some(Ok(msg))) => msg.header().path().is_some_and(|path| {
                manual_path.as_ref().is_some_and(|m| m.as_ref() == *path)
                    || agents_path.as_ref().is_some_and(|a| a.as_ref() == *path)
                    || path.as_str().ends_with("/nosleep_2eservice")
                    || path.as_str().ends_with("/nosleep_2dagents_2eservice")
            }),
            BusInput::Props(Some(Err(_))) | BusInput::Props(None) => break,
            BusInput::Manager(Some(Ok(msg))) => {
                let member = msg.header().member().map(|m| m.as_str().to_owned());
                match member.as_deref() {
                    Some("UnitNew") | Some("UnitRemoved") => msg
                        .body()
                        .deserialize::<(String, OwnedObjectPath)>()
                        .is_ok_and(|(id, _)| id == MANUAL_UNIT || id == AGENTS_UNIT),
                    Some("JobRemoved") => msg
                        .body()
                        .deserialize::<(u32, OwnedObjectPath, String, String)>()
                        .is_ok_and(|(_, _, unit, _)| unit == MANUAL_UNIT || unit == AGENTS_UNIT),
                    _ => false,
                }
            }
            BusInput::Manager(Some(Err(_))) | BusInput::Manager(None) => break,
        };

        if should_query {
            match query_units_dbus(&connection, &manager).await {
                Ok((units, m_path, a_path)) => {
                    manual_path = m_path;
                    agents_path = a_path;
                    apply_unit_status(&shared, &to_ui, Ok(units));
                }
                Err(error) => {
                    apply_unit_status(
                        &shared,
                        &to_ui,
                        Err(format!("systemd query failed: {error}")),
                    );
                }
            }
        }
    }

    // Stream ended; keep serving explicit refreshes via CLI fallback without busy-looping.
    apply_unit_status(&shared, &to_ui, query_units_cli());
    while wake_rx.recv().await.is_ok() {
        while wake_rx.try_recv().is_ok() {}
        apply_unit_status(&shared, &to_ui, query_units_cli());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn parses_working_agents_and_ignores_idle_or_done() {
        let json = br#"{
            "result": {
                "agents": [
                    {"agent": "claude", "pane_id": "%1", "agent_status": "working", "terminal_title_stripped": "Editing bar.rs"},
                    {"agent": "codex", "pane_id": "%2", "agent_status": "idle", "terminal_title_stripped": "Idle shell"},
                    {"agent": "opencode", "pane_id": "%3", "agent_status": "working", "terminal_title_stripped": ""},
                    {"agent": "claude", "pane_id": "%4", "agent_status": "done", "terminal_title_stripped": "Done"}
                ]
            }
        }"#;
        assert_eq!(
            parse_working_agents(json).unwrap(),
            vec!["claude · Editing bar.rs", "opencode · %3"]
        );
        assert!(parse_working_agents(br#"{"result":{}}"#).is_err());
    }

    #[test]
    fn inspects_fresh_and_stale_working_and_deferred_markers() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("nosleep-agents");
        assert_eq!(
            inspect_state_dir(&state_dir, SystemTime::now()),
            StateFiles::default()
        );

        fs::create_dir(&state_dir).unwrap();
        let working = state_dir.join("working");
        fs::File::create(&working).unwrap();
        fs::File::create(state_dir.join("lock-deferred")).unwrap();

        let now = SystemTime::now();
        let fresh = inspect_state_dir(&state_dir, now);
        assert!(fresh.working_fresh);
        assert!(fresh.next_expiry.is_some());
        assert!(fresh.lock_deferred);
        assert!(!fresh.suspend_deferred);

        let stale = inspect_state_dir(&state_dir, now + Duration::from_secs(11));
        assert!(!stale.working_fresh);
        assert!(stale.next_expiry.is_none());
        assert!(stale.lock_deferred);
    }

    #[test]
    fn parses_json_and_legacy_saved_power_profiles() {
        assert_eq!(
            parse_saved_profile(r#"{"previous":"balanced","applied":"power-saver"}"#),
            Some("balanced".into())
        );
        assert_eq!(
            parse_saved_profile("performance\n"),
            Some("performance".into())
        );
        assert_eq!(parse_saved_profile("  "), None);
    }

    #[test]
    fn inotify_tracks_working_and_deferred_lifecycle_without_polling() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().to_path_buf();
        let shared = Arc::new(Mutex::new(SharedState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let fs_refresh = Arc::new(AtomicBool::new(false));
        let (wake_rx, wake_tx) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap();
        let (tx, rx) = mpsc::channel::<Snapshot>();
        let (refresh_tx, refresh_rx) = mpsc::channel::<()>();

        let runtime_clone = runtime.clone();
        let stop_clone = stop.clone();
        let shared_clone = shared.clone();
        let worker = std::thread::spawn(move || {
            fs_watch_loop(
                &runtime_clone,
                &stop_clone,
                &fs_refresh,
                wake_rx,
                move || {
                    let _ = refresh_tx.send(());
                },
                &shared_clone,
                &|snap| {
                    let _ = tx.send(snap);
                },
            );
        });

        // Initial snapshot publishes idle state.
        let init = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!init.auto_active);

        // Create nosleep-agents/working -> inotify triggers auto_active and refresh callback.
        let state_dir = runtime.join("nosleep-agents");
        fs::create_dir(&state_dir).unwrap();
        fs::File::create(state_dir.join("working")).unwrap();
        refresh_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let active = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(active.auto_active);

        // Remove working -> inotify clears auto_active.
        fs::remove_file(state_dir.join("working")).unwrap();
        let inactive = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!inactive.auto_active);

        stop.store(true, Ordering::SeqCst);
        let _ = write(&wake_tx, &[1u8]);
        worker.join().unwrap();
    }
}
