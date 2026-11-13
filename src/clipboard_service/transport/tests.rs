use super::*;
use crate::clipboard_service::payload::{MAX_IMAGE, MAX_TEXT};
use std::sync::mpsc::{self, Receiver};
use wayland_protocols::ext::data_control::v1::server::{
    ext_data_control_device_v1 as se_device, ext_data_control_manager_v1 as se_manager,
    ext_data_control_offer_v1 as se_offer, ext_data_control_source_v1 as se_source,
};
use wayland_protocols_wlr::data_control::v1::server::{
    zwlr_data_control_device_v1 as sw_device, zwlr_data_control_manager_v1 as sw_manager,
    zwlr_data_control_offer_v1 as sw_offer, zwlr_data_control_source_v1 as sw_source,
};
use wayland_server::protocol::wl_seat as server_seat;
use wayland_server::{
    Client, DataInit, Dispatch as ServerDispatch, Display, DisplayHandle, GlobalDispatch, New,
    Resource,
};

fn text(bytes: &[u8]) -> Payload {
    Payload {
        mime: "text/plain;charset=utf-8".into(),
        bytes: bytes.into(),
        sensitive: false,
    }
}
fn pipe_pair() -> (OwnedFd, OwnedFd) {
    let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    nonblocking(&read).unwrap();
    (read, write)
}
fn read_all(fd: OwnedFd) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        match rustix::io::read(&fd, &mut buffer) {
            Ok(0) => return bytes,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::AGAIN) => {
                assert!(
                    Instant::now() < deadline,
                    "source never closed its transfer"
                );
                let timeout = timespec(deadline.saturating_duration_since(Instant::now()));
                poll(&mut [PollFd::new(&fd, PollFlags::IN)], Some(&timeout)).unwrap();
            }
            Err(error) => panic!("pipe read: {error}"),
        }
    }
}

#[derive(Clone)]
enum PeerDevice {
    Ext(se_device::ExtDataControlDeviceV1),
    Wlr(sw_device::ZwlrDataControlDeviceV1),
}
#[derive(Clone)]
enum PeerSource {
    Ext(se_source::ExtDataControlSourceV1),
    Wlr(sw_source::ZwlrDataControlSourceV1),
}
impl PeerSource {
    fn id(&self) -> wayland_server::backend::ObjectId {
        match self {
            Self::Ext(source) => source.id(),
            Self::Wlr(source) => source.id(),
        }
    }
    fn send(&self, mime: &str, fd: &OwnedFd) {
        match self {
            Self::Ext(source) => source.send(mime.into(), fd.as_fd()),
            Self::Wlr(source) => source.send(mime.into(), fd.as_fd()),
        }
    }
    fn cancelled(&self) {
        match self {
            Self::Ext(source) => source.cancelled(),
            Self::Wlr(source) => source.cancelled(),
        }
    }
}
#[derive(Default)]
struct PeerState {
    device: Option<PeerDevice>,
    selections: Vec<PeerSource>,
    receives: Vec<(usize, String, OwnedFd)>,
    destroyed_offers: Vec<usize>,
    bound: Vec<&'static str>,
    mimes: HashMap<wayland_server::backend::ObjectId, Vec<String>>,
}
struct Peer {
    display: Display<PeerState>,
    state: PeerState,
    client: Client,
    transport: Transport,
    events: Vec<Event>,
    receiver: Receiver<Event>,
    event_wake: UnixStream,
}
#[derive(Clone, Copy)]
enum Protocol {
    Ext,
    Wlr,
    Both,
}
impl Peer {
    fn new(protocol: Protocol) -> Self {
        let display = Display::<PeerState>::new().unwrap();
        let mut handle = display.handle();
        handle.create_global::<PeerState, server_seat::WlSeat, _>(2, ());
        // Register deprecated wlr first to test preference independently of order.
        if matches!(protocol, Protocol::Wlr | Protocol::Both) {
            handle.create_global::<PeerState, sw_manager::ZwlrDataControlManagerV1, _>(2, ());
        }
        if matches!(protocol, Protocol::Ext | Protocol::Both) {
            handle.create_global::<PeerState, se_manager::ExtDataControlManagerV1, _>(1, ());
        }
        let (client_socket, server_socket) = UnixStream::pair().unwrap();
        let client = handle.insert_client(server_socket, Arc::new(())).unwrap();
        let (event_wake, writer) = UnixStream::pair().unwrap();
        event_wake.set_nonblocking(true).unwrap();
        writer.set_nonblocking(true).unwrap();
        let (sender, receiver) = mpsc::channel();
        let transport = Transport::spawn(
            move |event| {
                sender.send(event).unwrap();
                wake(&writer);
            },
            move || Connection::from_socket(client_socket),
        )
        .unwrap();
        let mut peer = Self {
            display,
            client,
            transport,
            receiver,
            event_wake,
            state: PeerState::default(),
            events: Vec::new(),
        };
        peer.until(|peer| {
            peer.events
                .iter()
                .any(|event| matches!(event, Event::Availability(Ok(()))))
        });
        peer
    }

    fn until(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.display.dispatch_clients(&mut self.state).unwrap();
            self.display.flush_clients().unwrap();
            let mut reader = &self.event_wake;
            let mut bytes = [0; 128];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("event wake: {error}"),
                }
            }
            self.events.extend(self.receiver.try_iter());
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "protocol peer stalled; events: {:?}",
                self.events
            );
            let timeout = timespec(deadline.saturating_duration_since(Instant::now()));
            poll(
                &mut [
                    PollFd::new(&self.display, PollFlags::IN),
                    PollFd::new(&self.event_wake, PollFlags::IN),
                ],
                Some(&timeout),
            )
            .unwrap();
        }
    }

    fn offer(&mut self, tag: usize, mimes: &[&str], primary: bool) {
        let handle = self.display.handle();
        match self.state.device.as_ref().unwrap() {
            PeerDevice::Ext(device) => {
                let offer = self
                    .client
                    .create_resource::<se_offer::ExtDataControlOfferV1, _, PeerState>(
                        &handle, 1, tag,
                    )
                    .unwrap();
                device.data_offer(&offer);
                for mime in mimes {
                    offer.offer((*mime).into());
                }
                if primary {
                    device.primary_selection(Some(&offer));
                } else {
                    device.selection(Some(&offer));
                }
            }
            PeerDevice::Wlr(device) => {
                let offer = self
                    .client
                    .create_resource::<sw_offer::ZwlrDataControlOfferV1, _, PeerState>(
                        &handle, 1, tag,
                    )
                    .unwrap();
                device.data_offer(&offer);
                for mime in mimes {
                    offer.offer((*mime).into());
                }
                if primary {
                    device.primary_selection(Some(&offer));
                } else {
                    device.selection(Some(&offer));
                }
            }
        }
    }
    fn empty(&mut self) {
        match self.state.device.as_ref().unwrap() {
            PeerDevice::Ext(device) => device.selection(None),
            PeerDevice::Wlr(device) => device.selection(None),
        }
    }
    fn receive(&mut self, tag: usize) -> OwnedFd {
        self.until(|peer| {
            peer.state
                .receives
                .iter()
                .any(|(found, _, _)| *found == tag)
        });
        let index = self
            .state
            .receives
            .iter()
            .position(|(found, _, _)| *found == tag)
            .unwrap();
        self.state.receives.remove(index).2
    }
    fn capture(&mut self, tag: usize, bytes: &[u8]) {
        self.offer(tag, &["text/plain;charset=utf-8"], false);
        let fd = self.receive(tag);
        assert_eq!(rustix::io::write(&fd, bytes).unwrap(), bytes.len());
        drop(fd);
        self.until(|peer| {
            peer.events
                .iter()
                .any(|event| matches!(event, Event::Selection(payload) if &*payload.bytes == bytes))
        });
    }
    fn paste(&mut self, source: &PeerSource, mime: &str) -> Vec<u8> {
        let (reader, writer) = pipe_pair();
        source.send(mime, &writer);
        drop(writer);
        self.display.flush_clients().unwrap();
        read_all(reader)
    }
}

macro_rules! global {
    ($module:ident, $interface:ident, $name:literal) => {
        impl GlobalDispatch<$module::$interface, ()> for PeerState {
            fn bind(
                state: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                resource: New<$module::$interface>,
                _: &(),
                init: &mut DataInit<'_, Self>,
            ) {
                init.init(resource, ());
                state.bound.push($name);
            }
        }
    };
}
global!(server_seat, WlSeat, "seat");
global!(se_manager, ExtDataControlManagerV1, "ext");
global!(sw_manager, ZwlrDataControlManagerV1, "wlr");
impl ServerDispatch<server_seat::WlSeat, ()> for PeerState {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &server_seat::WlSeat,
        _: server_seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
macro_rules! peer_dispatch {
    ($manager:ident, $manager_type:ident, $device:ident, $device_type:ident, $offer:ident, $offer_type:ident, $source:ident, $source_type:ident, $variant:ident) => {
        impl ServerDispatch<$manager::$manager_type, ()> for PeerState {
            fn request(
                state: &mut Self,
                _: &Client,
                _: &$manager::$manager_type,
                request: $manager::Request,
                _: &(),
                _: &DisplayHandle,
                init: &mut DataInit<'_, Self>,
            ) {
                match request {
                    $manager::Request::GetDataDevice { id, .. } => {
                        let device = init.init(id, ());
                        device.selection(None);
                        state.device = Some(PeerDevice::$variant(device));
                    }
                    $manager::Request::CreateDataSource { id } => {
                        init.init(id, ());
                    }
                    _ => {}
                }
            }
        }
        impl ServerDispatch<$device::$device_type, ()> for PeerState {
            fn request(
                state: &mut Self,
                _: &Client,
                _: &$device::$device_type,
                request: $device::Request,
                _: &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if let $device::Request::SetSelection {
                    source: Some(source),
                } = request
                {
                    state.selections.push(PeerSource::$variant(source));
                }
            }
        }
        impl ServerDispatch<$offer::$offer_type, usize> for PeerState {
            fn request(
                state: &mut Self,
                _: &Client,
                _: &$offer::$offer_type,
                request: $offer::Request,
                tag: &usize,
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                match request {
                    $offer::Request::Receive { mime_type, fd } => {
                        state.receives.push((*tag, mime_type, fd))
                    }
                    $offer::Request::Destroy => state.destroyed_offers.push(*tag),
                    _ => {}
                }
            }
        }
        impl ServerDispatch<$source::$source_type, ()> for PeerState {
            fn request(
                state: &mut Self,
                _: &Client,
                source: &$source::$source_type,
                request: $source::Request,
                _: &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if let $source::Request::Offer { mime_type } = request {
                    state.mimes.entry(source.id()).or_default().push(mime_type);
                }
            }
        }
    };
}
peer_dispatch!(
    se_manager,
    ExtDataControlManagerV1,
    se_device,
    ExtDataControlDeviceV1,
    se_offer,
    ExtDataControlOfferV1,
    se_source,
    ExtDataControlSourceV1,
    Ext
);
peer_dispatch!(
    sw_manager,
    ZwlrDataControlManagerV1,
    sw_device,
    ZwlrDataControlDeviceV1,
    sw_offer,
    ZwlrDataControlOfferV1,
    sw_source,
    ZwlrDataControlSourceV1,
    Wlr
);

#[test]
fn protocol_prefers_ext_and_falls_back_to_wlr() {
    for (protocol, expected) in [
        (Protocol::Both, "ext"),
        (Protocol::Ext, "ext"),
        (Protocol::Wlr, "wlr"),
    ] {
        let mut peer = Peer::new(protocol);
        assert!(peer.state.bound.contains(&expected));
        assert_eq!(peer.state.bound.len(), 2);
        peer.capture(1, b"native clipboard");
        peer.transport.copy(17, text(b"copied natively")).unwrap();
        peer.until(|peer| {
            !peer.state.selections.is_empty()
                && peer
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Copied(17, Ok(()))))
        });
        let source = peer.state.selections.last().unwrap().clone();
        assert_eq!(
            peer.paste(&source, "text/plain;charset=utf-8"),
            b"copied natively"
        );
    }
}

#[test]
fn protocol_spaced_utf8_offer_is_captured_and_copied_with_original_mime_and_bytes() {
    const MIME: &str = "text/plain; charset=utf-8";
    let bytes = "café 日本語\noriginal bytes".as_bytes();
    for protocol in [Protocol::Ext, Protocol::Wlr] {
        let mut peer = Peer::new(protocol);
        peer.offer(1, &[MIME], false);
        peer.until(|peer| peer.state.receives.iter().any(|(tag, _, _)| *tag == 1));
        assert_eq!(
            peer.state
                .receives
                .iter()
                .find(|(tag, _, _)| *tag == 1)
                .unwrap()
                .1,
            MIME
        );
        let writer = peer.receive(1);
        assert_eq!(rustix::io::write(&writer, bytes).unwrap(), bytes.len());
        drop(writer);
        peer.until(|peer| {
            peer.events
                .iter()
                .any(|event| matches!(event, Event::Selection(_)))
        });
        let payload = peer
            .events
            .iter()
            .find_map(|event| match event {
                Event::Selection(payload) => Some(payload.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(payload.mime, MIME);
        assert_eq!(&*payload.bytes, bytes);
        let mut history = crate::clipboard_service::history::History::memory();
        assert!(history.capture(payload));
        peer.transport
            .copy(29, history.entries()[0].payload.clone())
            .unwrap();
        peer.until(|peer| {
            !peer.state.selections.is_empty()
                && peer
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Copied(29, Ok(()))))
        });
        let source = peer.state.selections.last().unwrap().clone();
        assert!(
            !peer.state.mimes[&source.id()]
                .iter()
                .any(|mime| mime == "STRING" || mime == "TEXT")
        );
        assert_eq!(peer.paste(&source, MIME), bytes);
        assert_eq!(peer.paste(&source, "UTF8_STRING"), bytes);
    }
}

#[test]
fn protocol_null_followed_by_offer_does_not_adopt_old_data() {
    for protocol in [Protocol::Ext, Protocol::Wlr] {
        let mut peer = Peer::new(protocol);
        peer.capture(1, b"previous");
        peer.empty();
        peer.offer(2, &["text/plain;charset=utf-8"], false);
        // Both events are serialized in a single dispatch batch, not separated
        // by sleeps or scheduler-dependent timing.
        let writer = peer.receive(2);
        assert!(peer.state.selections.is_empty());
        rustix::io::write(&writer, b"replacement").unwrap();
        drop(writer);
        peer.until(|peer| peer.events.iter().any(|event| matches!(event, Event::Selection(payload) if &*payload.bytes == b"replacement")));
        peer.empty();
        peer.until(|peer| !peer.state.selections.is_empty());
        let source = peer.state.selections.last().unwrap().clone();
        assert_eq!(peer.paste(&source, "text/plain"), b"replacement");
    }
}

#[test]
fn protocol_new_selection_cancels_inflight_old_read() {
    let mut peer = Peer::new(Protocol::Ext);
    peer.offer(1, &["text/plain;charset=utf-8"], false);
    let old = peer.receive(1);
    rustix::io::write(&old, b"unfinished old bytes").unwrap();
    peer.offer(2, &["text/plain;charset=utf-8"], false);
    let new = peer.receive(2);
    // Receiving the second request is a protocol fence: the old read has now
    // been invalidated, independent of the worker's scheduling.
    assert_eq!(
        rustix::io::write(&old, b"obsolete"),
        Err(rustix::io::Errno::PIPE)
    );
    drop(old);
    rustix::io::write(&new, b"current").unwrap();
    drop(new);
    peer.until(|peer| {
        peer.events.iter().any(
            |event| matches!(event, Event::Selection(payload) if &*payload.bytes == b"current"),
        )
    });
    assert_eq!(
        peer.events
            .iter()
            .filter(|event| matches!(event, Event::Selection(_)))
            .count(),
        1
    );
}

#[test]
fn protocol_sensitive_primary_and_unsupported_offers_never_resurrect_backup() {
    let mut peer = Peer::new(Protocol::Both);
    peer.capture(1, b"old public value");
    peer.offer(2, &["text/plain;charset=utf-8", PASSWORD_HINT], false);
    peer.until(|peer| peer.state.destroyed_offers.contains(&2));
    assert!(peer.state.receives.is_empty());
    peer.empty();
    peer.offer(3, &["text/plain;charset=utf-8"], true);
    peer.offer(4, &["application/octet-stream"], false);
    peer.until(|peer| {
        peer.state.destroyed_offers.contains(&3) && peer.state.destroyed_offers.contains(&4)
    });
    assert!(peer.state.selections.is_empty());
    assert!(peer.state.receives.is_empty());
    assert_eq!(
        peer.events
            .iter()
            .filter(|event| matches!(event, Event::Selection(_)))
            .count(),
        1
    );
}

#[test]
fn protocol_sources_preserve_image_mime_and_refuse_unadvertised_requests() {
    for protocol in [Protocol::Ext, Protocol::Wlr] {
        let mut peer = Peer::new(protocol);
        let png = image::DynamicImage::new_rgb8(1, 1);
        let mut encoded = std::io::Cursor::new(Vec::new());
        png.write_to(&mut encoded, image::ImageFormat::Png).unwrap();
        let bytes = encoded.into_inner();
        peer.transport
            .copy(
                42,
                Payload {
                    mime: "image/png".into(),
                    bytes: bytes.clone().into(),
                    sensitive: false,
                },
            )
            .unwrap();
        peer.until(|peer| !peer.state.selections.is_empty());
        let source = peer.state.selections.last().unwrap().clone();
        assert_eq!(peer.state.mimes[&source.id()], vec!["image/png"]);
        assert_eq!(peer.paste(&source, "image/png"), bytes);
        assert!(peer.paste(&source, "image/jpeg").is_empty());
        source.cancelled();
        peer.display.flush_clients().unwrap();
    }
}

#[test]
fn selection_generations_discard_stale_completions_and_secret_backups() {
    let mut selection = Selection::default();
    selection.changed(false);
    let stale = selection.generation;
    selection.changed(false);
    assert!(!selection.complete(stale, &text(b"stale")));
    assert!(selection.backup.is_none());
    assert!(selection.complete(selection.generation, &text(b"current")));
    selection.changed(true);
    assert!(!selection.complete(selection.generation, &text(b"late EOF")));
    assert_eq!(selection.orphan().unwrap().bytes.as_ref(), b"current");
    selection.changed(false);
    let mut secret = text(b"password");
    secret.sensitive = true;
    selection.complete(selection.generation, &secret);
    selection.changed(true);
    assert!(selection.orphan().is_none());
}

#[test]
fn incoming_requires_eof_and_enforces_exact_byte_boundary() {
    let (reader, writer) = pipe_pair();
    let mut incoming = Incoming {
        fd: reader,
        generation: 3,
        mime: "text/plain;charset=utf-8".into(),
        bytes: Vec::new(),
        deadline: Instant::now() + TRANSFER_TIMEOUT,
    };
    rustix::io::write(&writer, b"1234").unwrap();
    assert!(incoming.progress(4).unwrap().is_none());
    drop(writer);
    assert_eq!(
        incoming.progress(4).unwrap().unwrap().bytes.as_ref(),
        b"1234"
    );

    let (reader, writer) = pipe_pair();
    let mut incoming = Incoming {
        fd: reader,
        bytes: Vec::new(),
        ..incoming
    };
    rustix::io::write(&writer, b"12345").unwrap();
    assert!(incoming.progress(4).is_err());
    assert!(incoming.bytes.len() <= 4);
}

#[test]
fn outgoing_resumes_exactly_after_backpressure_and_closes_on_timeout() {
    let (reader, writer) = pipe_pair();
    nonblocking(&writer).unwrap();
    rustix::pipe::fcntl_setpipe_size(&writer, 4096).unwrap();
    let bytes: Arc<[u8]> = (0..20_000)
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>()
        .into();
    let mut outgoing = Outgoing {
        fd: writer,
        bytes: bytes.clone(),
        offset: 0,
        deadline: Instant::now() + TRANSFER_TIMEOUT,
    };
    assert!(!outgoing.progress().unwrap());
    let first_offset = outgoing.offset;
    assert!(first_offset > 0 && first_offset < bytes.len());
    assert!(!outgoing.progress().unwrap());
    assert_eq!(outgoing.offset, first_offset);
    let mut received = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match rustix::io::read(&reader, &mut buffer) {
            Ok(count) => received.extend_from_slice(&buffer[..count]),
            Err(rustix::io::Errno::AGAIN) => {}
            result => panic!("unexpected read: {result:?}"),
        }
        if outgoing.progress().unwrap() {
            break;
        }
    }
    drop(outgoing);
    received.extend(read_all(reader));
    assert_eq!(received, &*bytes);

    let (reader, writer) = pipe_pair();
    let (sender, receiver) = mpsc::channel();
    let mut state = State::new(Box::new(move |event| sender.send(event).unwrap()));
    state.outgoing.push(Outgoing {
        fd: writer,
        bytes: b"expired".as_slice().into(),
        offset: 0,
        deadline: Instant::now(),
    });
    state.transfer(Instant::now());
    assert!(state.outgoing.is_empty());
    assert!(read_all(reader).is_empty());
    assert!(matches!(receiver.try_recv(), Ok(Event::TransferError(_))));
}

#[test]
fn incoming_timeout_releases_pipe_and_reports_error() {
    let (reader, writer) = pipe_pair();
    let (sender, receiver) = mpsc::channel();
    let mut state = State::new(Box::new(move |event| sender.send(event).unwrap()));
    state.incoming = Some(Incoming {
        fd: reader,
        generation: 0,
        mime: "text/plain;charset=utf-8".into(),
        bytes: Vec::new(),
        deadline: Instant::now(),
    });
    state.transfer(Instant::now());
    assert!(state.incoming.is_none());
    assert!(matches!(receiver.try_recv(), Ok(Event::TransferError(_))));
    assert_eq!(
        rustix::io::write(&writer, b"late"),
        Err(rustix::io::Errno::PIPE)
    );
}

#[test]
fn aliases_do_not_mislabel_legacy_or_non_ascii_text() {
    let unicode = text("日本語".as_bytes());
    let aliases = advertised_mimes(&unicode);
    assert!(aliases.contains(&"UTF8_STRING".into()));
    assert!(!aliases.contains(&"STRING".into()));
    let latin1 = Payload {
        mime: "STRING".into(),
        bytes: [0xe9].as_slice().into(),
        sensitive: false,
    };
    assert_eq!(advertised_mimes(&latin1), vec!["STRING"]);
    let mut secret = text(b"secret");
    secret.sensitive = true;
    assert!(advertised_mimes(&secret).contains(&PASSWORD_HINT.into()));
}

#[test]
fn incomplete_image_eof_is_not_published_or_adopted() {
    let mut peer = Peer::new(Protocol::Ext);
    peer.offer(1, &["image/png"], false);
    let writer = peer.receive(1);
    rustix::io::write(&writer, b"\x89PNG\r\n\x1a\npartial").unwrap();
    drop(writer);
    peer.until(|peer| {
        peer.events
            .iter()
            .any(|event| matches!(event, Event::TransferError(_)))
    });
    assert!(
        !peer
            .events
            .iter()
            .any(|event| matches!(event, Event::Selection(_)))
    );
    peer.empty();
    peer.offer(2, &["application/octet-stream"], false);
    peer.until(|peer| peer.state.destroyed_offers.contains(&2));
    assert!(peer.state.selections.is_empty());
    peer.capture(3, b"healthy after malformed image");
    peer.transport.copy(73, text(b"copy still works")).unwrap();
    peer.until(|peer| {
        !peer.state.selections.is_empty()
            && peer
                .events
                .iter()
                .any(|event| matches!(event, Event::Copied(73, Ok(()))))
    });
    let source = peer.state.selections.last().unwrap().clone();
    assert_eq!(
        peer.paste(&source, "text/plain;charset=utf-8"),
        b"copy still works"
    );
    assert!(
        !peer
            .events
            .iter()
            .any(|event| matches!(event, Event::Availability(Err(_))))
    );
}

#[test]
fn drop_wakes_initialization_and_joins_without_compositor_response() {
    let (client_socket, _unresponsive_server) = UnixStream::pair().unwrap();
    let transport =
        Transport::spawn(|_| {}, move || Connection::from_socket(client_socket)).unwrap();
    let started = Instant::now();
    drop(transport);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn shutdown_acknowledges_every_accepted_copy_token() {
    let (client_socket, _unresponsive_server) = UnixStream::pair().unwrap();
    let (sender, receiver) = mpsc::channel();
    let transport = Transport::spawn(
        move |event| sender.send(event).unwrap(),
        move || Connection::from_socket(client_socket),
    )
    .unwrap();
    let mut accepted = Vec::new();
    for request in 0..16 {
        if transport.copy(request, text(b"pending")).is_ok() {
            accepted.push(request);
        }
    }
    drop(transport);
    let mut acknowledged = receiver
        .try_iter()
        .filter_map(|event| match event {
            Event::Copied(request, Err(_)) => Some(request),
            _ => None,
        })
        .collect::<Vec<_>>();
    accepted.sort_unstable();
    acknowledged.sort_unstable();
    assert_eq!(acknowledged, accepted);
}

#[test]
fn disconnect_reports_unavailability_and_rejects_future_copy() {
    let (client_socket, server_socket) = UnixStream::pair().unwrap();
    let (sender, receiver) = mpsc::channel();
    let transport = Transport::spawn(
        move |event| sender.send(event).unwrap(),
        move || Connection::from_socket(client_socket),
    )
    .unwrap();
    drop(server_socket);
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(3)),
        Ok(Event::Availability(Err(_)))
    ));
    assert!(transport.copy(1, text(b"cannot copy")).is_err());
}

#[test]
fn oversized_history_selection_stays_internal_but_survives_owner_exit() {
    let (reader, writer) = pipe_pair();
    drop(writer);
    let (sender, receiver) = mpsc::channel();
    let mut state = State::new(Box::new(move |event| sender.send(event).unwrap()));
    state.selection.changed(false);
    state.incoming = Some(Incoming {
        fd: reader,
        generation: state.selection.generation,
        mime: "text/plain;charset=utf-8".into(),
        bytes: vec![b'x'; MAX_TEXT + 1],
        deadline: Instant::now() + TRANSFER_TIMEOUT,
    });
    state.transfer(Instant::now());
    assert!(receiver.try_recv().is_err());
    state.selection.changed(true);
    let backup = state.selection.orphan().unwrap();
    assert_eq!(backup.bytes.len(), MAX_TEXT + 1);
    assert!(validate_payload(&backup).is_ok());
}

#[test]
fn history_emission_limits_include_the_exact_boundary() {
    let mut text_payload = text(&vec![b'x'; MAX_TEXT]);
    assert!(text_payload.fits_history());
    text_payload.bytes = vec![b'x'; MAX_TEXT + 1].into();
    assert!(!text_payload.fits_history());
    let mut image_payload = Payload {
        mime: "image/tiff".into(),
        bytes: vec![0; MAX_IMAGE].into(),
        sensitive: false,
    };
    assert!(image_payload.fits_history());
    image_payload.bytes = vec![0; MAX_IMAGE + 1].into();
    assert!(!image_payload.fits_history());
}

#[test]
fn sensitive_source_serves_marker_instead_of_password_for_hint_mime() {
    let mut peer = Peer::new(Protocol::Ext);
    let mut payload = text(b"actual password");
    payload.sensitive = true;
    peer.transport.copy(99, payload).unwrap();
    peer.until(|peer| !peer.state.selections.is_empty());
    let source = peer.state.selections.last().unwrap().clone();
    assert_eq!(peer.paste(&source, PASSWORD_HINT), b"secret");
    assert_eq!(
        peer.paste(&source, "text/plain;charset=utf-8"),
        b"actual password"
    );
}

#[test]
fn copy_queue_is_bounded_before_wayland_connection_finishes() {
    let (client_socket, _server_socket) = UnixStream::pair().unwrap();
    let (release, gate) = mpsc::sync_channel(0);
    let transport = Transport::spawn(
        |_| {},
        move || {
            gate.recv().unwrap();
            Connection::from_socket(client_socket)
        },
    )
    .unwrap();
    for request in 0..MAX_COMMANDS {
        transport.copy(request as u64, text(b"bounded")).unwrap();
    }
    assert!(transport.copy(100, text(b"full")).is_err());
    release.send(()).unwrap();
    drop(transport);
}

#[test]
fn empty_wayland_socket_finishes_draining_on_every_backend() {
    let (client_socket, _server_socket) = UnixStream::pair().unwrap();
    let connection = Connection::from_socket(client_socket).unwrap();
    let mut queue = connection.new_event_queue::<State>();
    let mut state = State::new(Box::new(|_| {}));
    assert!(drain(&mut queue, &mut state).unwrap());
}
