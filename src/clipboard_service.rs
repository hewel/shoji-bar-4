//! Session-owned native clipboard capture, history, and reactive snapshots.

mod history;
pub(crate) mod payload;
mod transport;

use anti_webui::prelude::*;
pub(crate) use history::Entry;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Payload {
    pub mime: String,
    pub bytes: Arc<[u8]>,
    pub sensitive: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub entries: Signal<Vec<Entry>>,
    pub ready: Signal<bool>,
    pub available: Signal<bool>,
    pub status: Signal<String>,
    copy_finished: Signal<Vec<(u64, Result<(), String>)>>,
}

impl State {
    pub(crate) fn with_copy_results<R>(
        self,
        f: impl FnOnce(&[(u64, Result<(), String>)]) -> R,
    ) -> R {
        self.copy_finished.with(|results| f(results))
    }
}

thread_local! {
    static STATE: State = create_root(|| State {
        entries: signal(Vec::new()),
        ready: signal(false),
        available: signal(false),
        status: signal(String::new()),
        copy_finished: signal(Vec::new()),
    }).0;
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(1) };
}

pub(crate) fn state() -> State {
    STATE.with(|state| *state)
}

enum Command {
    Transport(transport::Event),
    Copy { request: u64, id: u64 },
    Remove(u64),
    Pin(u64),
    Shutdown,
}

enum Update {
    History(Vec<Entry>),
    Status { available: bool, message: String },
    CopyFinished(u64, Result<(), String>),
}

#[derive(Default)]
struct PendingUpdates {
    history: Option<Vec<Entry>>,
    status: Option<(bool, String)>,
    copies: Vec<(u64, Result<(), String>)>,
    queued: bool,
}

impl PendingUpdates {
    fn push(&mut self, update: Update) -> bool {
        match update {
            Update::History(entries) => self.history = Some(entries),
            Update::Status { available, message } => self.status = Some((available, message)),
            Update::CopyFinished(request, result) => self.copies.push((request, result)),
        }
        !std::mem::replace(&mut self.queued, true)
    }
}

struct Updates {
    pending: Arc<Mutex<PendingUpdates>>,
    wake: UiSender<()>,
}

impl Updates {
    fn send(&self, update: Update) {
        // A stalled UI retains only the latest history, not every evicted payload
        // in an unbounded chain of snapshots. Copy acknowledgements are not lost.
        let notify = self.pending.lock().unwrap().push(update);
        if notify {
            self.wake.send(());
        }
    }
}

fn apply_updates(state: State, updates: PendingUpdates) {
    if let Some(entries) = updates.history {
        state.entries.set(entries);
        state.ready.set(true);
    }
    if let Some((available, message)) = updates.status {
        state.available.set(available);
        state.status.set(message);
    }
    // ui_channel batches signal writes: publish all receipts together so an
    // unrelated acknowledgement cannot overwrite the active request's result.
    state.copy_finished.set(updates.copies);
}

struct Worker {
    sender: mpsc::SyncSender<Command>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Install once for the session, not for each opening of the panel.
/// Previews use an isolated in-memory history and never touch the user's archive.
pub(crate) fn install(persistent: bool) {
    if WORKER.with(|worker| worker.borrow().is_some()) {
        return;
    }
    let state = state();
    state.ready.set(false);
    state.copy_finished.set(Vec::new());
    let pending = Arc::new(Mutex::new(PendingUpdates::default()));
    let received = pending.clone();
    let wake = ui_channel(move |()| {
        let updates = std::mem::take(&mut *received.lock().unwrap());
        apply_updates(state, updates);
    });
    let to_ui = Updates { pending, wake };
    let (sender, receiver) = mpsc::sync_channel(16);
    let events = sender.clone();
    match std::thread::Builder::new()
        .name("shoji clipboard".into())
        .spawn(move || {
            // Environment acquisition belongs to this installation adapter. The
            // dispatch below also runs with controlled history, copy I/O and output.
            let transport = transport::Transport::start(move |event| {
                let _ = events.send(Command::Transport(event));
            });
            let transport_error = transport.as_ref().err().map(ToString::to_string);
            let directory = directory();
            let storage_error = (persistent && directory.is_none())
                .then_some("History is memory-only: no absolute state directory is configured");
            let history = match directory.filter(|_| persistent) {
                Some(directory) => history::History::open(directory),
                None => history::History::memory(),
            };
            let session = Session {
                history,
                copy: move |request, payload| {
                    transport
                        .as_ref()
                        .map_err(ToString::to_string)?
                        .copy(request, payload)
                },
                available: false,
                transport_error,
                storage_error,
            };
            run(receiver, session, |update| to_ui.send(update));
        }) {
        Ok(join) => WORKER.with(|worker| {
            *worker.borrow_mut() = Some(Worker {
                sender,
                join: Some(join),
            })
        }),
        Err(error) => {
            state.ready.set(true);
            state.status.set(format!("Cannot start clipboard: {error}"));
            return;
        }
    }
    on_cleanup(move || {
        WORKER.with(|worker| worker.borrow_mut().take());
        state.available.set(false);
        state.copy_finished.set(Vec::new());
        state.entries.set(Vec::new());
    });
}

fn directory() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
        .map(|state| state.join("shoji-bar-4/clipboard"))
}

struct Session<C> {
    history: history::History,
    copy: C,
    available: bool,
    transport_error: Option<String>,
    storage_error: Option<&'static str>,
}

impl<C: Fn(u64, Payload) -> Result<(), String>> Session<C> {
    fn publish_status(&self, output: &impl Fn(Update)) {
        let message = match (
            self.transport_error.as_deref(),
            self.history.error().or(self.storage_error),
        ) {
            (Some(transport), Some(storage)) => format!("{transport}\n{storage}"),
            (Some(error), None) | (None, Some(error)) => error.to_owned(),
            (None, None) => String::new(),
        };
        output(Update::Status {
            available: self.available,
            message,
        });
    }

    fn dispatch(&mut self, command: Command, output: &impl Fn(Update)) -> bool {
        let changed = match command {
            Command::Shutdown => return false,
            Command::Transport(transport::Event::Availability(result)) => {
                self.available = result.is_ok();
                self.transport_error = result.err();
                false
            }
            Command::Transport(transport::Event::TransferError(error)) => {
                self.transport_error = Some(error);
                false
            }
            Command::Transport(transport::Event::Selection(payload)) => {
                self.transport_error = None;
                self.history.capture(payload)
            }
            Command::Transport(transport::Event::Copied(request, result)) => {
                if result.is_ok() {
                    self.transport_error = None;
                }
                output(Update::CopyFinished(request, result));
                false
            }
            Command::Copy { request, id } => {
                let result = self
                    .history
                    .entries()
                    .iter()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| "This clipboard entry is no longer available".to_owned())
                    .and_then(|entry| (self.copy)(request, entry.payload.clone()));
                if let Err(error) = result {
                    output(Update::CopyFinished(request, Err(error)));
                }
                false
            }
            Command::Remove(id) => self.history.remove(id),
            Command::Pin(id) => self.history.toggle_pin(id),
        };
        if changed {
            output(Update::History(self.history.entries().to_vec()));
        }
        self.publish_status(output);
        true
    }
}

fn run(
    receiver: mpsc::Receiver<Command>,
    mut session: Session<impl Fn(u64, Payload) -> Result<(), String>>,
    output: impl Fn(Update),
) {
    output(Update::History(session.history.entries().to_vec()));
    session.publish_status(&output);
    while let Ok(command) = receiver.recv() {
        if !session.dispatch(command, &output) {
            break;
        }
    }
    // A transport callback may be blocked on this bounded mailbox. Disconnect it
    // before joining the transport, otherwise shutdown could deadlock.
    drop(receiver);
    drop(session);
}

fn send(command: Command) -> Result<(), String> {
    WORKER.with(|worker| {
        worker
            .borrow()
            .as_ref()
            .ok_or_else(|| "Clipboard is not running".to_owned())
            .and_then(|worker| {
                worker
                    .sender
                    .try_send(command)
                    .map_err(|error| match error {
                        mpsc::TrySendError::Full(_) => "Clipboard is busy; try again".to_owned(),
                        mpsc::TrySendError::Disconnected(_) => "Clipboard has stopped".to_owned(),
                    })
            })
    })
}

/// Admission returns a request identity; its eventual result is observable in State.
pub(crate) fn copy(id: u64) -> Result<u64, String> {
    let request = NEXT_REQUEST.with(|next| {
        let request = next.get();
        next.set(
            request
                .checked_add(1)
                .expect("clipboard request IDs exhausted"),
        );
        request
    });
    send(Command::Copy { request, id })?;
    Ok(request)
}

pub(crate) fn remove(id: u64) -> Result<(), String> {
    send(Command::Remove(id))
}
pub(crate) fn toggle_pin(id: u64) -> Result<(), String> {
    send(Command::Pin(id))
}

#[cfg(test)]
mod tests;
