//! Bus-only idle integration. No suspend, compositor, timer, or policy actions live here.

use anti_webui::prelude::UiSender;
use async_channel::{Receiver, Sender};
use futures_lite::{StreamExt, future};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use zbus::{Connection, MessageStream, Proxy, fdo, message::Header, zvariant::OwnedFd};

const LOGIN_NAME: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const LOGIN_INTERFACE: &str = "org.freedesktop.login1.Manager";
const SAVER_NAME: &str = "org.freedesktop.ScreenSaver";

#[derive(Debug)]
pub enum Event {
    Ready {
        sleep_delay: bool,
        screensaver: bool,
    },
    Inhibitors(usize),
    Caffeine(Result<bool, String>),
    PrepareForSleep {
        generation: u64,
        sleeping: bool,
    },
    Error(String),
}

#[derive(Clone)]
pub struct Bus(Arc<Handle>);

struct Handle {
    caffeine: Sender<(u64, bool)>,
    revision: Arc<AtomicU64>,
    ready: Sender<u64>,
    stop: Sender<()>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

impl Bus {
    /// Requests a logind block inhibitor without blocking the UI. Superseded
    /// requests never publish a stale result or retain a newly acquired FD.
    pub fn set_caffeine(&self, enabled: bool) {
        let revision = self.0.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.0.caffeine.try_send((revision, enabled));
    }

    /// Call only after the compositor has acknowledged Locked for this transition.
    pub fn sleep_ready(&self, generation: u64) {
        let _ = self.0.ready.try_send(generation);
    }

    /// Stops all lanes and closes inhibitor FDs. Does not join a worker from a UI callback.
    /// Dropping the last Bus handle has the same effect.
    pub fn stop(&self) {
        let _ = self.0.stop.try_send(());
    }
}

/// Starts one application worker, with zbus's per-connection executors handling
/// dispatch. System/session failures are independent; there is no polling.
pub fn start(to_ui: UiSender<Event>) -> Bus {
    let (caffeine, caffeine_rx) = async_channel::unbounded();
    let (ready, ready_rx) = async_channel::unbounded();
    let (stop, stop_rx) = async_channel::bounded(1);
    let revision = Arc::new(AtomicU64::new(0));
    let bus = Bus(Arc::new(Handle {
        caffeine,
        revision: revision.clone(),
        ready,
        stop,
    }));
    let availability = Arc::new(Availability {
        flags: Mutex::new((false, false)),
        to_ui: to_ui.clone(),
    });
    to_ui.send(Event::Ready {
        sleep_delay: false,
        screensaver: false,
    });
    let errors = to_ui.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("shoji idle bus".into())
        .spawn(move || {
            future::block_on(future::race(
                async {
                    future::zip(
                        system_lane(caffeine_rx, revision, ready_rx, availability.clone()),
                        screensaver_lane(availability),
                    )
                    .await;
                },
                async {
                    let _ = stop_rx.recv().await;
                },
            ));
        })
    {
        errors.send(Event::Error(format!(
            "Cannot start idle bus worker: {error}"
        )));
    }
    bus
}

struct Availability {
    flags: Mutex<(bool, bool)>,
    to_ui: UiSender<Event>,
}

impl Availability {
    fn set(&self, sleep_delay: Option<bool>, screensaver: Option<bool>) {
        let mut flags = self.flags.lock().expect("idle availability poisoned");
        if let Some(value) = sleep_delay {
            flags.0 = value;
        }
        if let Some(value) = screensaver {
            flags.1 = value;
        }
        self.to_ui.send(Event::Ready {
            sleep_delay: flags.0,
            screensaver: flags.1,
        });
    }

    fn error(&self, message: impl Into<String>) {
        self.to_ui.send(Event::Error(message.into()));
    }
}

async fn inhibit(
    connection: &Connection,
    what: &str,
    why: &str,
    mode: &str,
) -> Result<OwnedFd, String> {
    let proxy = Proxy::new(connection, LOGIN_NAME, LOGIN_PATH, LOGIN_INTERFACE)
        .await
        .map_err(|error| error.to_string())?;
    proxy
        .call("Inhibit", &(what, "shoji-bar-4", why, mode))
        .await
        .map_err(|error| error.to_string())
}

async fn owner_changes(connection: &Connection, name: Option<&str>) -> zbus::Result<MessageStream> {
    let mut rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .path("/org/freedesktop/DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?;
    if let Some(name) = name {
        rule = rule.add_arg(name)?;
    }
    MessageStream::for_match_rule(rule.build(), connection, None).await
}
// Directed signals can bypass the bus's match rules. zbus cannot resolve a
// well-known sender in its local matcher, so authenticate the bus daemon here.
async fn next_owner_change(stream: &mut MessageStream) -> Option<zbus::Result<zbus::Message>> {
    loop {
        match stream.next().await {
            Some(Ok(message))
                if message
                    .header()
                    .sender()
                    .is_some_and(|sender| sender.as_str() == "org.freedesktop.DBus") =>
            {
                return Some(Ok(message));
            }
            Some(Ok(_)) => continue,
            other => return other,
        }
    }
}

fn coalesce_request(mut request: (u64, bool), requests: &Receiver<(u64, bool)>) -> (u64, bool) {
    while let Ok(newer) = requests.try_recv() {
        // Cloned Bus handles may enqueue in a different order than revisions.
        if newer.0 > request.0 {
            request = newer;
        }
    }
    request
}

async fn system_lane(
    caffeine: Receiver<(u64, bool)>,
    revision: Arc<AtomicU64>,
    ready: Receiver<u64>,
    availability: Arc<Availability>,
) {
    let result = async {
        let connection = zbus::connection::Builder::system()?
            .method_timeout(Duration::from_secs(3))
            .build()
            .await?;
        // Queue signals before acquiring the FD so setup cannot miss a transition.
        // Nothing is delivered to the UI until the initial acquisition completes.
        let owners = owner_changes(&connection, Some(LOGIN_NAME)).await?;
        let owner = fdo::DBusProxy::new(&connection)
            .await?
            .get_name_owner(LOGIN_NAME.try_into()?)
            .await?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner.as_str())?
            .path(LOGIN_PATH)?
            .interface(LOGIN_INTERFACE)?
            .member("PrepareForSleep")?
            .build();
        let signals = MessageStream::for_match_rule(rule, &connection, None).await?;
        Ok::<_, zbus::Error>((connection, signals, owners))
    }
    .await;
    match result {
        Ok((connection, signals, mut owners)) => {
            let delay = match inhibit(
                &connection,
                "sleep",
                "Lock the compositor before sleep",
                "delay",
            )
            .await
            {
                Ok(fd) => Some(fd),
                Err(error) => {
                    availability.error(format!("logind sleep delay unavailable: {error}"));
                    None
                }
            };
            availability.set(Some(delay.is_some()), None);
            let failure = future::race(
                future::race(
                    sleep_signals(&connection, signals, ready, delay, &availability),
                    async {
                        caffeine_requests(&connection, &caffeine, &revision, &availability.to_ui)
                            .await;
                        "logind caffeine channel closed".to_owned()
                    },
                ),
                async {
                    match next_owner_change(&mut owners).await {
                        Some(Ok(_)) => {
                            "logind service owner changed; sleep protection is unavailable"
                                .to_owned()
                        }
                        Some(Err(error)) => format!("logind ownership monitoring failed: {error}"),
                        None => "System bus disconnected".to_owned(),
                    }
                },
            )
            .await;
            // Cancelling either lane drops its FD, including any pending acquisition.
            drop(connection);
            availability.error(failure.clone());
            availability.to_ui.send(Event::Caffeine(Err(failure)));
        }
        Err(error) => {
            availability.error(format!("System bus idle integration unavailable: {error}"))
        }
    }
    availability.set(Some(false), None);
    // Keep answering toggles truthfully even when only the session lane is usable.
    while let Ok(mut request) = caffeine.recv().await {
        request = coalesce_request(request, &caffeine);
        if request.0 == revision.load(Ordering::SeqCst) {
            availability.to_ui.send(Event::Caffeine(if request.1 {
                Err("logind caffeine inhibition is unavailable".into())
            } else {
                Ok(false)
            }));
        }
    }
}

async fn caffeine_requests(
    connection: &Connection,
    requests: &Receiver<(u64, bool)>,
    revision: &AtomicU64,
    to_ui: &UiSender<Event>,
) {
    let mut held = None;
    while let Ok(mut request) = requests.recv().await {
        request = coalesce_request(request, requests);
        if request.0 != revision.load(Ordering::SeqCst) {
            continue;
        }
        let result = if request.1 {
            if held.is_some() {
                Ok(true)
            } else {
                match inhibit(connection, "idle:sleep", "Caffeine is enabled", "block").await {
                    Ok(fd) => {
                        if request.0 != revision.load(Ordering::SeqCst) {
                            drop(fd);
                            continue;
                        }
                        held = Some(fd);
                        Ok(true)
                    }
                    Err(error) => Err(format!("Cannot enable caffeine: {error}")),
                }
            }
        } else {
            held = None;
            Ok(false)
        };
        if request.0 == revision.load(Ordering::SeqCst) {
            to_ui.send(Event::Caffeine(result));
        }
    }
}

/// Generic token makes the release invariant testable without opening real FDs.
struct SleepTransition<T> {
    generation: u64,
    pending: Option<u64>,
    delay: Option<T>,
}

impl<T> SleepTransition<T> {
    fn new(delay: Option<T>) -> Self {
        Self {
            generation: 0,
            pending: None,
            delay,
        }
    }

    fn begin(&mut self) -> u64 {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("sleep generation exhausted");
        self.pending = Some(self.generation);
        self.generation
    }

    fn ready(&mut self, generation: u64) {
        if self.pending == Some(generation) {
            self.delay = None;
            self.pending = None;
        }
    }

    fn resume(&mut self) {
        self.pending = None;
        self.delay = None;
    }
}

async fn sleep_signals(
    connection: &Connection,
    mut signals: MessageStream,
    ready: Receiver<u64>,
    delay: Option<OwnedFd>,
    availability: &Availability,
) -> String {
    enum Input {
        Signal(Option<zbus::Result<zbus::Message>>),
        Ready(Result<u64, async_channel::RecvError>),
    }
    let mut state = SleepTransition::new(delay);
    loop {
        let input = future::race(async { Input::Signal(signals.next().await) }, async {
            Input::Ready(ready.recv().await)
        })
        .await;
        match input {
            Input::Ready(Ok(generation)) => state.ready(generation),
            Input::Ready(Err(_)) => return "Sleep acknowledgement channel closed".into(),
            Input::Signal(Some(Ok(message))) => {
                let (sleeping,): (bool,) = match message.body().deserialize() {
                    Ok(value) => value,
                    Err(error) => {
                        availability.error(format!("Invalid logind sleep signal: {error}"));
                        continue;
                    }
                };
                if sleeping {
                    let generation = state.begin();
                    availability.to_ui.send(Event::PrepareForSleep {
                        generation,
                        sleeping: true,
                    });
                } else {
                    // Invalidate acknowledgements and close the old FD BEFORE any await.
                    state.resume();
                    availability.to_ui.send(Event::PrepareForSleep {
                        generation: state.generation,
                        sleeping: false,
                    });
                    match inhibit(
                        connection,
                        "sleep",
                        "Lock the compositor before sleep",
                        "delay",
                    )
                    .await
                    {
                        Ok(fd) => {
                            state.delay = Some(fd);
                            availability.set(Some(true), None);
                        }
                        Err(error) => {
                            availability.set(Some(false), None);
                            availability.error(format!("Cannot rearm logind sleep delay: {error}"));
                        }
                    }
                }
            }
            Input::Signal(Some(Err(error))) => {
                return format!("logind sleep monitoring failed: {error}");
            }
            Input::Signal(None) => return "System bus sleep stream disconnected".into(),
        }
    }
}

#[derive(Default)]
struct Cookies {
    next: u32,
    accepting: bool,
    owners: HashMap<u32, String>,
}

impl Cookies {
    fn inhibit(&mut self, owner: &str) -> Result<u32, String> {
        let first = self.next.wrapping_add(1).max(1);
        let mut cookie = first;
        loop {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.owners.entry(cookie) {
                entry.insert(owner.to_owned());
                self.next = cookie;
                return Ok(cookie);
            }
            cookie = cookie.wrapping_add(1).max(1);
            if cookie == first {
                return Err("ScreenSaver cookie space exhausted".into());
            }
        }
    }

    fn uninhibit(&mut self, owner: &str, cookie: u32) -> fdo::Result<()> {
        match self.owners.get(&cookie) {
            Some(actual) if actual == owner => {
                self.owners.remove(&cookie);
                Ok(())
            }
            Some(_) => Err(fdo::Error::AccessDenied(
                "Inhibition belongs to another caller".into(),
            )),
            None => Err(fdo::Error::InvalidArgs("Unknown inhibition cookie".into())),
        }
    }

    fn disconnect(&mut self, owner: &str) -> bool {
        let before = self.owners.len();
        self.owners.retain(|_, actual| actual != owner);
        self.owners.len() != before
    }
}

#[derive(Clone)]
struct ScreenSaver {
    cookies: Arc<Mutex<Cookies>>,
    to_ui: UiSender<Event>,
}

impl ScreenSaver {
    fn disconnect(&self, owner: &str) {
        let mut cookies = self.cookies.lock().expect("idle cookies poisoned");
        if cookies.disconnect(owner) {
            self.to_ui.send(Event::Inhibitors(cookies.owners.len()));
        }
    }

    fn clear(&self) {
        let mut cookies = self.cookies.lock().expect("idle cookies poisoned");
        cookies.accepting = false;
        cookies.owners.clear();
        self.to_ui.send(Event::Inhibitors(0));
    }
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    async fn inhibit(
        &self,
        application: &str,
        reason: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<u32> {
        let _ = (application, reason);
        let owner = header
            .sender()
            .ok_or_else(|| fdo::Error::AccessDenied("Missing caller identity".into()))?;
        let proxy = fdo::DBusProxy::new(connection).await?;
        let cookie = {
            let mut cookies = self.cookies.lock().expect("idle cookies poisoned");
            if !cookies.accepting {
                return Err(fdo::Error::Failed(
                    "ScreenSaver inhibition service is unavailable".into(),
                ));
            }
            let cookie = cookies
                .inhibit(owner.as_str())
                .map_err(fdo::Error::LimitsExceeded)?;
            self.to_ui.send(Event::Inhibitors(cookies.owners.len()));
            cookie
        };
        // NameOwnerChanged may have been handled just before this method inserted
        // its cookie. Checking AFTER insertion closes that disconnect/dispatch race.
        match proxy.name_has_owner(owner.clone().into()).await {
            Ok(true) => {
                let cookies = self.cookies.lock().expect("idle cookies poisoned");
                if cookies.accepting
                    && cookies
                        .owners
                        .get(&cookie)
                        .is_some_and(|actual| actual == owner.as_str())
                {
                    Ok(cookie)
                } else {
                    Err(fdo::Error::Failed(
                        "ScreenSaver inhibition service or caller disconnected".into(),
                    ))
                }
            }
            Ok(false) => {
                self.disconnect(owner.as_str());
                Err(fdo::Error::Failed("Inhibiting caller disconnected".into()))
            }
            Err(error) => {
                self.disconnect(owner.as_str());
                Err(error)
            }
        }
    }

    #[zbus(name = "UnInhibit")]
    fn uninhibit(&self, cookie: u32, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        let owner = header
            .sender()
            .ok_or_else(|| fdo::Error::AccessDenied("Missing caller identity".into()))?;
        let mut cookies = self.cookies.lock().expect("idle cookies poisoned");
        cookies.uninhibit(owner.as_str(), cookie)?;
        self.to_ui.send(Event::Inhibitors(cookies.owners.len()));
        Ok(())
    }

    fn get_active(&self) -> bool {
        false
    }
}

async fn screensaver_lane(availability: Arc<Availability>) {
    let saver = ScreenSaver {
        cookies: Arc::new(Mutex::new(Cookies::default())),
        to_ui: availability.to_ui.clone(),
    };
    let result = async {
        let connection = zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(3))
            .build()
            .await?;
        // Observe disconnects before publishing the service or accepting cookies.
        let owners = owner_changes(&connection, None).await?;
        connection
            .object_server()
            .at("/org/freedesktop/ScreenSaver", saver.clone())
            .await?;
        connection
            .object_server()
            .at("/ScreenSaver", saver.clone())
            .await?;
        // request_name() defaults to ReplaceExisting; explicitly do NOT use it.
        connection
            .request_name_with_flags(SAVER_NAME, fdo::RequestNameFlags::DoNotQueue.into())
            .await?;
        saver
            .cookies
            .lock()
            .expect("idle cookies poisoned")
            .accepting = true;
        Ok::<_, zbus::Error>((connection, owners))
    }
    .await;
    match result {
        Ok((connection, mut owners)) => {
            availability.set(None, Some(true));
            loop {
                match next_owner_change(&mut owners).await {
                    Some(Ok(message)) => {
                        match message.body().deserialize::<(String, String, String)>() {
                            Ok((name, old_owner, new_owner)) => {
                                if name == SAVER_NAME && connection.unique_name().is_some_and(|own| own.as_str() == old_owner)
                                    && new_owner != old_owner
                                {
                                    availability.error("ScreenSaver service ownership lost; application inhibition unavailable");
                                    break;
                                }
                                if name.starts_with(':') && new_owner.is_empty() {
                                    saver.disconnect(&name);
                                }
                            }
                            Err(error) => availability.error(format!("Invalid session owner signal: {error}")),
                        }
                    }
                    Some(Err(error)) => {
                        availability.error(format!("ScreenSaver bus monitoring failed: {error}"));
                        break;
                    }
                    None => {
                        availability.error("Session bus disconnected; ScreenSaver application inhibition unavailable");
                        break;
                    }
                }
            }
        }
        Err(error) => availability.error(format!(
            "ScreenSaver application inhibition unavailable (session bus or name already owned): {error}"
        )),
    }
    saver.clear();
    availability.set(None, Some(false));
    future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_caffeine_toggles_follow_revision_not_enqueue_order() {
        let (tx, rx) = async_channel::unbounded();
        tx.try_send((3, false)).unwrap();
        tx.try_send((2, true)).unwrap();
        assert_eq!(coalesce_request((1, true), &rx), (3, false));
        assert!(rx.is_empty());
    }

    #[test]
    fn cookies_are_owned_by_unique_callers() {
        let mut cookies = Cookies::default();
        let first = cookies.inhibit(":1.10").unwrap();
        let second = cookies.inhibit(":1.20").unwrap();
        assert!(matches!(
            cookies.uninhibit(":1.20", first),
            Err(fdo::Error::AccessDenied(_))
        ));
        assert_eq!(cookies.owners.len(), 2);
        cookies.uninhibit(":1.10", first).unwrap();
        assert!(cookies.owners.contains_key(&second));
        assert!(cookies.uninhibit(":1.10", first).is_err());
    }

    #[test]
    fn disconnect_releases_all_and_only_that_callers_cookies() {
        let mut cookies = Cookies::default();
        cookies.inhibit(":1.10").unwrap();
        cookies.inhibit(":1.10").unwrap();
        let other = cookies.inhibit(":1.20").unwrap();
        assert!(cookies.disconnect(":1.10"));
        assert_eq!(cookies.owners.len(), 1);
        assert!(cookies.owners.contains_key(&other));
        assert!(!cookies.disconnect(":1.10"));
        // Also models a cookie inserted after its disconnect signal was handled:
        // the post-insertion NameHasOwner check invokes the same cleanup.
        cookies.inhibit(":1.10").unwrap();
        assert!(cookies.disconnect(":1.10"));
        assert_eq!(cookies.owners.len(), 1);
    }

    #[test]
    fn cookie_wrap_skips_zero_and_live_cookies() {
        let mut cookies = Cookies::default();
        let first = cookies.inhibit(":1.1").unwrap();
        cookies.next = u32::MAX;
        let next = cookies.inhibit(":1.2").unwrap();
        assert_ne!(next, 0);
        assert_ne!(next, first);
    }

    #[test]
    fn only_current_sleep_acknowledgement_releases_delay() {
        let mut sleep = SleepTransition::new(Some("first delay"));
        sleep.ready(0);
        assert!(sleep.delay.is_some());
        let first = sleep.begin();
        sleep.ready(first + 1);
        assert!(sleep.delay.is_some());
        sleep.ready(first);
        assert!(sleep.delay.is_none());
        sleep.resume();
        sleep.delay = Some("rearmed delay");
        sleep.ready(first);
        assert!(sleep.delay.is_some());
        let second = sleep.begin();
        sleep.ready(first);
        assert!(sleep.delay.is_some());
        sleep.ready(second);
        assert!(sleep.delay.is_none());
    }

    #[test]
    fn resume_invalidates_even_unacknowledged_sleep() {
        let mut sleep = SleepTransition::new(Some("old delay"));
        let first = sleep.begin();
        sleep.resume();
        assert!(sleep.delay.is_none());
        sleep.delay = Some("new delay");
        sleep.ready(first);
        assert!(sleep.delay.is_some());
    }
}
