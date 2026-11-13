//! Serialized compositor display-power requests with cancellation and shutdown restoration.

use crate::idle::Message;
use anti_webui::prelude::UiSender;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// A worker commits execution once. Cancellation wins only before that commit point.
#[derive(Clone, Default, Debug)]
pub struct Admission(Arc<AtomicU8>);

impl Admission {
    pub fn start(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn cancel(&self) {
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

struct Request {
    id: Option<u64>,
    on: bool,
    admission: Option<Admission>,
}

enum Command {
    Set(Request),
    Shutdown,
}

struct Worker {
    sender: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Clones share one ordered worker. Dropping the last clone joins it after restoring
/// any display-power-off request not followed by a successful power-on request.
#[derive(Clone)]
pub struct Power(Arc<Worker>);

impl Power {
    pub fn new(sender: UiSender<Message>) -> Self {
        Self::with_backend(sender, set_output_power)
    }

    /// Queues a request; completion or cancellation is delivered through the UI sender.
    pub fn set(
        &self,
        id: Option<u64>,
        on: bool,
        admission: Option<Admission>,
    ) -> Result<(), String> {
        self.0
            .sender
            .send(Command::Set(Request { id, on, admission }))
            .map_err(|_| "Display-power worker is unavailable".to_string())
    }

    fn with_backend(
        sender: UiSender<Message>,
        mut set_power: impl FnMut(bool) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();
        let errors = sender.clone();
        let thread = std::thread::Builder::new()
            .name("shoji-idle-power".into())
            .spawn(move || {
                let mut needs_restore = false;
                for command in rx {
                    let Command::Set(request) = command else {
                        break;
                    };
                    if let Some(admission) = request.admission
                        && !admission.start()
                    {
                        if let Some(id) = request.id {
                            sender.send(Message::Abandoned(id));
                        }
                        continue;
                    }
                    // A failed or lost reply does not prove that the monitors stayed on.
                    if !request.on {
                        needs_restore = true;
                    }
                    let result = set_power(request.on);
                    if request.on && result.is_ok() {
                        needs_restore = false;
                    }
                    sender.send(Message::PowerDone {
                        id: request.id,
                        on: request.on,
                        result,
                    });
                }
                // Restoration must not depend on a live UI receiver during teardown.
                if needs_restore && let Err(error) = set_power(true) {
                    eprintln!("shoji-bar-4 idle: shutdown display restoration failed: {error}");
                }
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                errors.send(Message::Error(format!(
                    "Cannot start display-power worker: {error}"
                )));
                None
            }
        };
        Self(Arc::new(Worker { sender: tx, thread }))
    }
}

fn set_output_power(on: bool) -> Result<(), String> {
    let path = std::env::var_os("NIRI_SOCKET").ok_or("Screen power requires the compositor's NIRI_SOCKET; use a custom idle command on other compositors")?;
    set_output_power_at(std::path::Path::new(&path), on)
}

fn set_output_power_at(path: &std::path::Path, on: bool) -> Result<(), String> {
    use rustix::net::{
        AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with,
    };
    // AF_UNIX connect can otherwise block indefinitely when the compositor's backlog is full.
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| e.to_string())?;
    let address = SocketAddrUnix::new(path).map_err(|e| e.to_string())?;
    connect(&socket, &address).map_err(|e| format!("Display-power IPC: {e}"))?;
    let mut stream = UnixStream::from(socket);
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let action = if on {
        "PowerOnMonitors"
    } else {
        "PowerOffMonitors"
    };
    let request = serde_json::json!({"Action":{action:{}}});
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    let reply: serde_json::Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if reply.get("Ok").is_some() {
        Ok(())
    } else {
        Err(format!("Compositor rejected display power: {reply}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anti_webui::prelude::{create_root, ui_channel};

    #[test]
    fn admission_commits_execution_only_once() {
        let admission = Admission::default();
        let worker = admission.clone();
        admission.cancel();
        assert!(!worker.start());
        let committed = Admission::default();
        assert!(committed.start());
        committed.cancel();
        assert!(
            !committed.start(),
            "an admitted action is never launched twice"
        );
    }

    #[test]
    fn cancelling_a_queued_request_prevents_display_power_changes() {
        let (changes, changed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (sender, owner) = create_root(|| ui_channel(|_: Message| {}));
        let mut first = Some(released);
        let power = Power::with_backend(sender, move |on| {
            changes.send(on).unwrap();
            if let Some(released) = first.take() {
                released.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            Ok(())
        });
        power.set(None, true, None).unwrap();
        assert!(changed.recv_timeout(Duration::from_secs(2)).unwrap());
        let admission = Admission::default();
        power.set(Some(1), false, Some(admission.clone())).unwrap();
        power.set(None, true, None).unwrap();
        admission.cancel();
        release.send(()).unwrap();
        drop(power);
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec![true]);
        owner.dispose();
    }

    #[test]
    fn shutdown_restores_in_flight_off_without_ui_callbacks_even_if_reply_is_uncertain() {
        for failed_off in [false, true] {
            let (changes, changed) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (sender, owner) = create_root(|| {
                ui_channel(|_: Message| panic!("shutdown must not require a UI callback"))
            });
            let power = Power::with_backend(sender, move |on| {
                changes.send(on).unwrap();
                if !on {
                    released.recv_timeout(Duration::from_secs(2)).unwrap();
                    if failed_off {
                        return Err("the compositor reply was lost".into());
                    }
                }
                Ok(())
            });
            let remaining = power.clone();
            drop(power);
            remaining.set(Some(1), false, None).unwrap();
            assert!(!changed.recv_timeout(Duration::from_secs(2)).unwrap());
            owner.dispose();
            let disposing = std::thread::spawn(move || drop(remaining));
            release.send(()).unwrap();
            assert!(changed.recv_timeout(Duration::from_secs(2)).unwrap());
            disposing.join().unwrap();
            assert!(changed.into_iter().collect::<Vec<_>>().is_empty());
        }
    }

    #[test]
    fn failed_power_on_is_retried_at_shutdown_but_successful_power_on_is_not() {
        for failed_on in [false, true] {
            let (changes, changed) = mpsc::channel();
            let (sender, owner) = create_root(|| ui_channel(|_: Message| {}));
            let mut fail_next_on = failed_on;
            let power = Power::with_backend(sender, move |on| {
                changes.send(on).unwrap();
                if on && fail_next_on {
                    fail_next_on = false;
                    return Err("the compositor reply was lost".into());
                }
                Ok(())
            });
            power.set(None, false, None).unwrap();
            power.set(None, true, None).unwrap();
            drop(power);
            assert_eq!(
                changed.into_iter().collect::<Vec<_>>(),
                if failed_on {
                    vec![false, true, true]
                } else {
                    vec![false, true]
                }
            );
            owner.dispose();
        }
    }

    #[test]
    fn saturated_compositor_socket_does_not_block_worker_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compositor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        rustix::net::listen(&listener, 0).unwrap();
        let _queued = UnixStream::connect(&path).unwrap();
        let (attempts, attempted) = mpsc::channel();
        let (sender, owner) = create_root(|| ui_channel(|_: Message| {}));
        let power = Power::with_backend(sender, move |on| {
            let result = set_output_power_at(&path, on);
            attempts.send((on, result.is_err())).unwrap();
            result
        });
        power.set(Some(1), false, None).unwrap();
        owner.dispose();
        let (done, finished) = mpsc::channel();
        let disposing = std::thread::spawn(move || {
            drop(power);
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(Duration::from_secs(2))
            .expect("a full accept backlog must fail, not hang shutdown");
        disposing.join().unwrap();
        assert_eq!(
            attempted.into_iter().collect::<Vec<_>>(),
            vec![(false, true), (true, true)]
        );
    }

    #[test]
    fn worker_preserves_compositor_request_and_acknowledgement_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compositor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let compositor = std::thread::spawn(move || {
            let mut actions = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                actions.push(serde_json::from_str::<serde_json::Value>(&request).unwrap());
                writeln!(stream, "{{\"Ok\":\"Handled\"}}").unwrap();
            }
            actions
        });
        let (sender, owner) = create_root(|| ui_channel(|_: Message| {}));
        let power = Power::with_backend(sender, move |on| set_output_power_at(&path, on));
        power.set(None, false, None).unwrap();
        drop(power);
        assert_eq!(
            compositor.join().unwrap(),
            vec![
                serde_json::json!({"Action":{"PowerOffMonitors":{}}}),
                serde_json::json!({"Action":{"PowerOnMonitors":{}}}),
            ]
        );
        owner.dispose();
    }
}
