use super::*;

#[test]
fn stalled_ui_releases_evicted_payloads_without_losing_copy_results() {
    let mut pending = PendingUpdates::default();
    let mut retired = Vec::new();
    let mut wakes = 0;
    for id in 1..=200 {
        let bytes: Arc<[u8]> = vec![id as u8; 1024].into();
        retired.push(Arc::downgrade(&bytes));
        wakes += usize::from(pending.push(Update::History(vec![Entry {
            id,
            payload: Payload {
                mime: "image/png".into(),
                bytes,
                sensitive: false,
            },
            pinned: false,
        }])));
        wakes += usize::from(pending.push(Update::Status {
            available: true,
            message: id.to_string(),
        }));
        if id == 50 || id == 150 {
            wakes += usize::from(pending.push(Update::CopyFinished(id, Ok(()))));
        }
    }
    assert!(
        retired[..199]
            .iter()
            .all(|payload| payload.upgrade().is_none())
    );
    assert_eq!(
        wakes, 1,
        "only one UI wake may accumulate while it is stalled"
    );
    let delivered = std::mem::take(&mut pending);
    assert_eq!(delivered.history.as_ref().unwrap()[0].id, 200);
    assert_eq!(
        delivered
            .copies
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [50, 150]
    );
    assert!(pending.push(Update::Status {
        available: false,
        message: "disconnected".into()
    }));
    drop(delivered);
    assert!(retired.iter().all(|payload| payload.upgrade().is_none()));
}

fn text(bytes: &[u8]) -> Payload {
    Payload {
        mime: "text/plain;charset=utf-8".into(),
        bytes: Arc::from(bytes),
        sensitive: false,
    }
}

/// The real UI admission and result adapter, with acknowledgements controlled
/// by the test instead of a Wayland connection. No receipt owns a reactive root.
struct Controlled {
    commands: mpsc::Receiver<Command>,
    updates: Updates,
    owner: Owner,
}

impl Controlled {
    fn new() -> Self {
        assert!(WORKER.with(|worker| worker.borrow().is_none()));
        let state = state();
        state.copy_finished.set(Vec::new());
        let pending = Arc::new(Mutex::new(PendingUpdates::default()));
        let received = pending.clone();
        let (wake, owner) = create_root(|| {
            ui_channel(move |()| {
                apply_updates(state, std::mem::take(&mut *received.lock().unwrap()));
            })
        });
        let (sender, commands) = mpsc::sync_channel(16);
        WORKER.with(|worker| *worker.borrow_mut() = Some(Worker { sender, join: None }));
        Self {
            commands,
            updates: Updates { pending, wake },
            owner,
        }
    }

    fn request(&self, expected_id: u64) -> u64 {
        match self.commands.try_recv().unwrap() {
            Command::Copy { request, id } => {
                assert_eq!(id, expected_id);
                request
            }
            _ => panic!("expected an admitted copy"),
        }
    }

    fn finish(&self, results: impl IntoIterator<Item = (u64, Result<(), String>)>) {
        for (request, result) in results {
            self.updates.send(Update::CopyFinished(request, result));
        }
        anti_webui::core::channel::pump();
    }
}

impl Drop for Controlled {
    fn drop(&mut self) {
        // Keep the bounded receiver alive until the synthetic worker has stopped.
        while self.commands.try_recv().is_ok() {}
        WORKER.with(|worker| worker.borrow_mut().take());
        self.owner.dispose();
        state().copy_finished.set(Vec::new());
        crate::panels::close();
    }
}

fn output(name: &str) -> anti_webui::shell::prelude::Output {
    anti_webui::shell::prelude::Output {
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn clipboard_copy_dismiss_reopen_reuses_owner_without_reusing_request() {
    use crate::clipboard::copy::Copy;
    use crate::panels::{self, Panel};
    for coalesced in [false, true] {
        let session = Controlled::new();
        panels::open("DP-1", Panel::Clipboard);
        let (copy, owner) = create_root(|| Copy::new(&output("DP-1")));
        copy.choose(11);
        let old = session.request(11);
        let reopen = || {
            panels::close();
            panels::open("DP-1", Panel::Clipboard);
        };
        if coalesced {
            batch(reopen);
        } else {
            reopen();
        }
        session.finish([(old, Ok(()))]);
        assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Clipboard)));
        assert_eq!(copy.error(), "");
        copy.choose(12);
        let new = session.request(12);
        session.finish([(new, Err("ownership refused".into()))]);
        assert_eq!(copy.error(), "ownership refused");
        panels::close();
        panels::open("DP-1", Panel::Clipboard);
        assert_eq!(copy.error(), "", "reused views must not retain old errors");
        batch(|| {
            panels::close();
            panels::open("DP-1", Panel::Clipboard);
            copy.choose(13);
        });
        let final_request = session.request(13);
        session.finish([(final_request, Ok(()))]);
        assert_eq!(panels::current(), None);
        owner.dispose();
    }
}

#[test]
fn clipboard_copy_output_switch_survives_old_owner_disposal_and_batched_replies() {
    use crate::clipboard::copy::Copy;
    use crate::panels::{self, Panel};
    for result in [Ok(()), Err("new ownership refused".to_owned())] {
        let session = Controlled::new();
        panels::open("DP-1", Panel::Clipboard);
        let (old_copy, old_owner) = create_root(|| Copy::new(&output("DP-1")));
        old_copy.choose(21);
        let old = session.request(21);
        panels::open("DP-2", Panel::Clipboard);
        let (new_copy, new_owner) = create_root(|| Copy::new(&output("DP-2")));
        new_copy.choose(22);
        let new = session.request(22);
        // Shell-layer disposal is delayed past creation of the new output's view.
        old_owner.dispose();
        // A UI-channel callback batches writes. Even when the stale receipt is
        // last, it must not overwrite the new invocation's success or failure.
        session.finish([(new, result.clone()), (old, Ok(()))]);
        match result {
            Ok(()) => assert_eq!(panels::current(), None),
            Err(error) => {
                assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Clipboard)));
                assert_eq!(new_copy.error(), error);
            }
        }
        new_owner.dispose();
    }
}

#[test]
fn clipboard_copy_disposed_request_owner_is_not_written_by_late_reply() {
    use crate::clipboard::copy::Copy;
    use crate::panels::{self, Panel};
    let session = Controlled::new();
    panels::open("DP-1", Panel::Clipboard);
    let (copy, owner) = create_root(|| Copy::new(&output("DP-1")));
    copy.choose(31);
    let request = session.request(31);
    panels::close();
    owner.dispose();
    panels::open("DP-1", Panel::Clipboard);
    session.finish([(request, Err("late failure".into()))]);
    assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Clipboard)));
    let (copy, owner) = create_root(|| Copy::new(&output("DP-1")));
    assert_eq!(copy.error(), "");
    copy.choose(32);
    let request = session.request(32);
    session.finish([(request, Ok(()))]);
    assert_eq!(panels::current(), None);
    owner.dispose();
}

#[test]
fn clipboard_copy_rejection_cannot_leave_an_older_request_armed() {
    use crate::clipboard::copy::Copy;
    use crate::panels::{self, Panel};
    let session = Controlled::new();
    panels::open("DP-1", Panel::Clipboard);
    let (copy, owner) = create_root(|| Copy::new(&output("DP-1")));
    copy.choose(41);
    let old = session.request(41);
    for id in 0..16 {
        remove(id).unwrap();
    }
    copy.choose(42);
    assert_eq!(copy.error(), "Clipboard is busy; try again");
    session.finish([(old, Ok(()))]);
    assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Clipboard)));
    assert_eq!(copy.error(), "Clipboard is busy; try again");
    owner.dispose();
}

#[test]
fn clipboard_dispatch_captures_pins_copies_and_removes_real_history() {
    let first = text(b"first");
    let second = text(b"second");
    let copied = RefCell::new(Vec::new());
    let observed = RefCell::new(Vec::new());
    let session = Session {
        history: history::History::memory(),
        copy: |request, payload| {
            copied.borrow_mut().push((request, payload));
            Ok(())
        },
        available: false,
        transport_error: None,
        storage_error: None,
    };
    let (sender, receiver) = mpsc::sync_channel(16);
    for command in [
        Command::Transport(transport::Event::Availability(Ok(()))),
        Command::Transport(transport::Event::Selection(first.clone())),
        Command::Transport(transport::Event::Selection(second)),
        Command::Pin(1),
        Command::Copy { request: 51, id: 1 },
        Command::Remove(1),
        Command::Copy { request: 52, id: 1 },
        Command::Transport(transport::Event::Copied(51, Ok(()))),
        Command::Remove(999),
        Command::Pin(999),
        Command::Shutdown,
    ] {
        sender.send(command).unwrap();
    }
    run(receiver, session, |update| {
        observed.borrow_mut().push(update)
    });
    let copied = copied.borrow();
    assert_eq!(copied.len(), 1, "a removed ID must never reach transport");
    assert_eq!(copied[0].0, 51);
    assert_eq!(copied[0].1, first);
    assert!(Arc::ptr_eq(&copied[0].1.bytes, &first.bytes));
    let observed = observed.borrow();
    let snapshots: Vec<_> = observed
        .iter()
        .filter_map(|update| match update {
            Update::History(entries) => Some(entries),
            _ => None,
        })
        .collect();
    assert_eq!(
        snapshots.len(),
        5,
        "only actual history changes publish snapshots"
    );
    assert!(snapshots[0].is_empty());
    assert_eq!(
        snapshots[2]
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(snapshots[3][0].id, 1);
    assert!(snapshots[3][0].pinned);
    assert_eq!(
        snapshots[4]
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [2]
    );
    let receipts: Vec<_> = observed
        .iter()
        .filter_map(|update| match update {
            Update::CopyFinished(request, result) => Some((*request, result.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        receipts,
        [
            (
                52,
                Err("This clipboard entry is no longer available".into())
            ),
            (51, Ok(())),
        ],
        "admission is not success; only the transport acknowledgement finishes a copy"
    );
}

#[test]
fn clipboard_dispatch_reports_transport_rejection_and_recovers_status() {
    let observed = RefCell::new(Vec::new());
    let mut history = history::History::memory();
    assert!(history.capture(text(b"retained")));
    let session = Session {
        history,
        copy: |_, _| Err("copy queue full".into()),
        available: false,
        transport_error: Some("cannot connect".into()),
        storage_error: Some("memory-only history"),
    };
    let (sender, receiver) = mpsc::sync_channel(16);
    for command in [
        Command::Copy { request: 61, id: 1 },
        Command::Transport(transport::Event::Availability(Ok(()))),
        Command::Transport(transport::Event::TransferError("transfer failed".into())),
        Command::Transport(transport::Event::Selection(Payload {
            sensitive: true,
            ..text(b"secret")
        })),
        Command::Transport(transport::Event::Copied(62, Err("ownership lost".into()))),
        Command::Shutdown,
    ] {
        sender.send(command).unwrap();
    }
    run(receiver, session, |update| {
        observed.borrow_mut().push(update)
    });
    let observed = observed.borrow();
    let statuses: Vec<_> = observed
        .iter()
        .filter_map(|update| match update {
            Update::Status { available, message } => Some((*available, message.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(statuses[0], (false, "cannot connect\nmemory-only history"));
    assert!(statuses.contains(&(true, "transfer failed\nmemory-only history")));
    assert_eq!(statuses.last(), Some(&(true, "memory-only history")));
    assert_eq!(
        observed
            .iter()
            .filter(|update| matches!(update, Update::History(_)))
            .count(),
        1
    );
    assert!(observed.iter().any(|update| matches!(
        update, Update::CopyFinished(61, Err(error)) if error == "copy queue full"
    )));
    assert!(observed.iter().any(|update| matches!(
        update, Update::CopyFinished(62, Err(error)) if error == "ownership lost"
    )));
}

#[test]
fn clipboard_shutdown_disconnects_mailbox_before_dropping_transport() {
    struct TransportDrop(mpsc::SyncSender<Command>);
    impl TransportDrop {
        fn copy(&self, _: u64, _: Payload) -> Result<(), String> {
            Ok(())
        }
    }
    impl Drop for TransportDrop {
        fn drop(&mut self) {
            assert!(
                matches!(
                    self.0
                        .try_send(Command::Transport(transport::Event::Availability(Ok(())))),
                    Err(mpsc::TrySendError::Disconnected(_))
                ),
                "transport must be released only after its callback mailbox disconnects"
            );
        }
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.send(Command::Shutdown).unwrap();
    let transport = TransportDrop(sender);
    run(
        receiver,
        Session {
            history: history::History::memory(),
            copy: move |request, payload| transport.copy(request, payload),
            available: false,
            transport_error: None,
            storage_error: None,
        },
        |_| {},
    );
}
