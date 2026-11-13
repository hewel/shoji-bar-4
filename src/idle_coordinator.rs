//! Idle coordination independent of a desktop session. Platform details stay behind the adapter.
use crate::display_power::Admission;
use crate::idle_bus::Event as BusEvent;
use crate::idle_config::{IdleAction, IdleBehavior, IdleConfig};
use crate::idle_policy::{Effect, Policy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockState {
    Unlocked,
    Locking,
    Locked,
    Refused,
}

#[derive(Debug)]
pub(crate) enum Event {
    Configure(IdleConfig),
    Idle {
        index: usize,
        token: u64,
    },
    Fade(u64),
    LockChanged,
    Bus(BusEvent),
    SystemSleep(bool),
    Caffeine(bool),
    MediaPlaying(bool),
    Suspend,
    Cancel,
    LockTimeout(u64),
    Completed {
        id: u64,
        result: Result<(), String>,
    },
    Abandoned(u64),
    SuspendDone(Result<(), String>),
    PowerDone {
        id: Option<u64>,
        on: bool,
        result: Result<(), String>,
    },
    Error(String),
}

#[derive(Debug)]
pub(crate) enum Command {
    Unwatch(usize),
    Watch {
        index: usize,
        token: u64,
        seconds: u32,
    },
    Fade {
        token: u64,
        seconds: u32,
    },
    HideFade,
    ReadLock,
    Caffeine(bool),
    LockTimeout(u64),
    CancelLockTimeout(u64),
    RequestLock,
    SleepReady(u64),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterLock {
    Action { id: u64, suspend: bool },
    ManualSuspend,
    ExternalSleep(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LockWait {
    token: u64,
    after: AfterLock,
}

#[derive(Debug)]
pub(crate) enum Action {
    Suspend,
    Screen {
        id: Option<u64>,
        on: bool,
        admission: Option<Admission>,
    },
    Command {
        id: Option<u64>,
        command: String,
        admission: Option<Admission>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Output {
    pub actions: Vec<Action>,
    pub commands: Vec<Command>,
    pub admission_cancelled: Vec<u64>,
    pub caffeine: Option<bool>,
    pub inhibitors: Option<usize>,
    pub capability: Option<(bool, bool)>,
    pub hold: Option<bool>,
    pub warning: bool,
}

impl Output {
    fn command(&mut self, command: Command) {
        self.commands.push(command);
    }
    fn error(&mut self, error: String) {
        self.command(Command::Error(error));
    }
}

/// Owns the full sequencing contract; policy remains an internal implementation detail.
pub(crate) struct Coordinator {
    policy: Policy,
    watches: Vec<Option<(u64, bool)>>,
    lock_waits: Vec<LockWait>,
    serial: u64,
    pub(crate) admissions: Vec<(u64, Admission)>,
    own_suspend: Option<bool>,
    suspend_in_flight: bool,
    suspend_ids: Vec<u64>,
    external_sleep: Option<u64>,
    system_sleeping: bool,
    caffeine: bool,
    media_playing: bool,
    inhibitors: usize,
}

impl Coordinator {
    pub fn new() -> Self {
        Self {
            policy: Policy::new(),
            watches: Vec::new(),
            lock_waits: Vec::new(),
            serial: 0,
            admissions: Vec::new(),
            own_suspend: None,
            suspend_in_flight: false,
            suspend_ids: Vec::new(),
            external_sleep: None,
            system_sleeping: false,
            caffeine: false,
            media_playing: false,
            inhibitors: 0,
        }
    }

    fn now_idle(&self, index: usize, token: u64) -> Option<bool> {
        self.watches
            .get(index)
            .and_then(Option::as_ref)
            .filter(|watch| watch.0 == token)
            .map(|watch| watch.1)
    }

    pub fn input(&mut self, event: Event) -> Output {
        let mut output = Output::default();
        let effects = match event {
            Event::Configure(config) => match config.validate() {
                Ok(()) => self.policy.configure(config),
                Err(error) => {
                    output.error(error);
                    Vec::new()
                }
            },
            Event::Idle { index, token } => match self.now_idle(index, token) {
                Some(idle) => self.policy.idle(index, token, idle),
                None => return output,
            },
            Event::Fade(token) => {
                for (index, token) in self
                    .watches
                    .iter()
                    .enumerate()
                    .filter_map(|(index, watch)| {
                        watch
                            .as_ref()
                            .filter(|watch| !watch.1)
                            .map(|watch| (index, watch.0))
                    })
                    .collect::<Vec<_>>()
                {
                    let effects = self.policy.idle(index, token, false);
                    self.apply(effects, &mut output);
                }
                self.policy.fade_complete(token)
            }
            Event::LockChanged => {
                output.command(Command::ReadLock);
                Vec::new()
            }
            Event::Bus(event) => {
                self.bus_event(event, &mut output);
                Vec::new()
            }
            Event::SystemSleep(sleeping) => {
                self.system_sleeping = sleeping;
                if sleeping && self.own_suspend != Some(false) {
                    output.command(Command::RequestLock);
                }
                if !sleeping {
                    self.own_suspend = None;
                }
                self.policy.sleeping(sleeping)
            }
            Event::Caffeine(enabled) => {
                self.caffeine = enabled;
                output.caffeine = Some(enabled);
                output.command(Command::Caffeine(enabled));
                self.policy
                    .inhibit(enabled || self.media_playing || self.inhibitors > 0)
            }
            Event::MediaPlaying(playing) => {
                self.media_playing = playing;
                self.policy
                    .inhibit(self.caffeine || playing || self.inhibitors > 0)
            }
            Event::Suspend => {
                if !self.suspend_in_flight
                    && !self
                        .lock_waits
                        .iter()
                        .any(|wait| wait.after == AfterLock::ManualSuspend)
                {
                    self.wait_for_lock(AfterLock::ManualSuspend, &mut output);
                }
                Vec::new()
            }
            Event::Cancel => self.policy.cancel(),
            Event::LockTimeout(token) => {
                if let Some(index) = self.lock_waits.iter().position(|wait| wait.token == token) {
                    let wait = self.lock_waits.remove(index);
                    output.command(Command::CancelLockTimeout(wait.token));
                    if let AfterLock::Action { id, .. } = wait.after {
                        self.finish_action(
                            id,
                            Err("Lock confirmation timed out".into()),
                            &mut output,
                        );
                    } else {
                        output.error(
                            "Lock confirmation timed out; automatic suspend was cancelled".into(),
                        );
                    }
                }
                Vec::new()
            }
            Event::Completed { id, result } => {
                self.finish_action(id, result, &mut output);
                Vec::new()
            }
            Event::Abandoned(id) => {
                self.admissions.retain(|(pending, _)| *pending != id);
                self.policy.completed(id, false)
            }
            Event::SuspendDone(result) => {
                self.suspend_in_flight = false;
                if result.is_err() {
                    self.own_suspend = None;
                }
                if let Err(error) = &result {
                    output.error(error.clone());
                }
                for id in std::mem::take(&mut self.suspend_ids) {
                    let effects = self.policy.completed(id, result.is_ok());
                    self.apply(effects, &mut output);
                }
                Vec::new()
            }
            Event::PowerDone { id, on, result } => {
                if !on && result.is_err() {
                    output.actions.push(Action::Screen {
                        id: None,
                        on: true,
                        admission: None,
                    });
                }
                if let Some(id) = id {
                    self.finish_action(id, result, &mut output);
                } else if let Err(error) = result {
                    output.error(error);
                }
                Vec::new()
            }
            Event::Error(error) => {
                output.error(error);
                Vec::new()
            }
        };
        self.apply(effects, &mut output);
        self.prune_lock_waits(&mut output);
        output.hold = Some(self.sleep_hold_wanted());
        output
    }

    pub fn lock_changed(&mut self, state: LockState) -> Output {
        let mut output = Output::default();
        let effects = self.policy.locked(state == LockState::Locked);
        self.apply(effects, &mut output);
        if matches!(state, LockState::Locked | LockState::Refused) {
            let waits = std::mem::take(&mut self.lock_waits);
            for wait in waits {
                output.command(Command::CancelLockTimeout(wait.token));
                if let AfterLock::Action { id, .. } = wait.after
                    && !self.admit_action(id, &mut output)
                {
                    continue;
                }
                if state == LockState::Refused {
                    let error =
                        "Compositor refused the session lock; suspend was cancelled".to_owned();
                    if let AfterLock::Action { id, .. } = wait.after {
                        self.finish_action(id, Err(error), &mut output);
                    } else {
                        output.error(error);
                    }
                    continue;
                }
                match wait.after {
                    AfterLock::Action { id, suspend: true } => {
                        self.suspend(Some(id), true, &mut output)
                    }
                    AfterLock::Action { id, suspend: false } => {
                        self.finish_action(id, Ok(()), &mut output)
                    }
                    AfterLock::ManualSuspend => self.suspend(None, true, &mut output),
                    AfterLock::ExternalSleep(generation)
                        if self.external_sleep == Some(generation) =>
                    {
                        output.command(Command::SleepReady(generation));
                    }
                    AfterLock::ExternalSleep(_) => {}
                }
            }
        }
        self.prune_lock_waits(&mut output);
        output.hold = Some(self.sleep_hold_wanted());
        output
    }

    #[cfg(test)]
    pub fn watch_identities(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.watches
            .iter()
            .enumerate()
            .filter_map(|(index, watch)| watch.as_ref().map(|watch| (index, watch.0)))
    }

    pub fn refresh_idle(&mut self, index: usize, idle: bool) {
        if let Some(watch) = self.watches.get_mut(index).and_then(Option::as_mut) {
            watch.1 = idle;
        }
    }

    pub fn watch_state(&mut self, index: usize, token: u64, available: bool, idle: bool) -> Output {
        if let Some(watch) = self.watches.get_mut(index).and_then(Option::as_mut)
            && watch.0 == token
        {
            watch.1 = idle;
        }
        let mut output = Output::default();
        if !available {
            output.error("Compositor does not provide ext-idle-notify; automatic idle actions are unavailable".into());
        }
        output
    }

    fn bus_event(&mut self, event: BusEvent, output: &mut Output) {
        let effects = match event {
            BusEvent::Ready {
                sleep_delay,
                screensaver,
            } => {
                output.capability = Some((sleep_delay, screensaver));
                Vec::new()
            }
            BusEvent::Inhibitors(count) => {
                self.inhibitors = count;
                output.inhibitors = Some(count);
                self.policy
                    .inhibit(self.caffeine || self.media_playing || count > 0)
            }
            BusEvent::Caffeine(result) => {
                if let Err(error) = result {
                    output.error(format!("Caffeine applies to Shoji only: {error}"));
                }
                Vec::new()
            }
            BusEvent::PrepareForSleep {
                generation,
                sleeping,
            } => {
                if sleeping {
                    self.external_sleep = Some(generation);
                    self.system_sleeping = true;
                    let effects = self.policy.sleeping(true);
                    if self.own_suspend == Some(false) {
                        output.command(Command::SleepReady(generation));
                    } else {
                        self.wait_for_lock(AfterLock::ExternalSleep(generation), output);
                    }
                    effects
                } else {
                    self.system_sleeping = false;
                    self.external_sleep = None;
                    self.own_suspend = None;
                    self.policy.sleeping(false)
                }
            }
            BusEvent::Error(error) => {
                output.error(error);
                Vec::new()
            }
        };
        self.apply(effects, output);
    }

    fn apply(&mut self, effects: Vec<Effect>, output: &mut Output) {
        for effect in effects {
            match effect {
                Effect::Unwatch(index) => output.command(Command::Unwatch(index)),
                Effect::Watch {
                    index,
                    token,
                    seconds,
                } => {
                    output.command(Command::Watch {
                        index,
                        token,
                        seconds,
                    });
                    if self.watches.len() <= index {
                        self.watches.resize_with(index + 1, || None);
                    }
                    self.watches[index] = Some((token, true));
                }
                Effect::Fade { token, seconds } => {
                    output.command(Command::Fade { token, seconds });
                    output.warning = true;
                }
                Effect::HideFade => {
                    output.command(Command::HideFade);
                    output.warning = false;
                }
                Effect::Run { id, behavior } => self.run(id, behavior, output),
                Effect::Resume(behavior) => {
                    if behavior.action == IdleAction::ScreenOff {
                        output.actions.push(Action::Screen {
                            id: None,
                            on: true,
                            admission: None,
                        });
                    }
                    if !behavior.resume_command.trim().is_empty() {
                        output.actions.push(Action::Command {
                            id: None,
                            command: behavior.resume_command,
                            admission: None,
                        });
                    }
                }
            }
        }
        output.admission_cancelled.extend(
            self.admissions
                .iter()
                .filter_map(|(id, _)| (!self.policy.action_pending(*id)).then_some(*id)),
        );
    }

    fn new_admission(&mut self, id: u64) -> Admission {
        let admission = Admission::default();
        self.admissions.push((id, admission.clone()));
        admission
    }

    fn run(&mut self, id: u64, behavior: IdleBehavior, output: &mut Output) {
        if !self.admit_action(id, output) {
            return;
        }
        match behavior.action {
            IdleAction::Lock | IdleAction::LockAndSuspend | IdleAction::Suspend
                if behavior.action != IdleAction::Suspend || behavior.lock_before_suspend =>
            {
                self.wait_for_lock(
                    AfterLock::Action {
                        id,
                        suspend: behavior.action != IdleAction::Lock,
                    },
                    output,
                );
            }
            IdleAction::Suspend => self.suspend(Some(id), false, output),
            IdleAction::ScreenOff => {
                output.actions.push(Action::Screen {
                    id: Some(id),
                    on: false,
                    admission: Some(self.new_admission(id)),
                });
            }
            IdleAction::Command => {
                output.actions.push(Action::Command {
                    id: Some(id),
                    command: behavior.command,
                    admission: Some(self.new_admission(id)),
                });
            }
            IdleAction::Lock | IdleAction::LockAndSuspend => unreachable!(),
        }
    }

    fn admit_action(&mut self, id: u64, output: &mut Output) -> bool {
        let watch = self.policy.action_watch(id);
        let current = watch.is_some_and(|(index, token)| self.now_idle(index, token) == Some(true));
        if !current {
            if let Some((index, token)) = watch {
                let effects = self.policy.idle(index, token, false);
                self.apply(effects, output);
            }
            let effects = self.policy.completed(id, false);
            self.apply(effects, output);
        }
        current
    }

    pub(crate) fn sleep_hold_wanted(&self) -> bool {
        self.lock_waits.iter().any(|wait| {
            matches!(
                wait.after,
                AfterLock::Action { suspend: true, .. }
                    | AfterLock::ManualSuspend
                    | AfterLock::ExternalSleep(_)
            )
        }) || self.own_suspend == Some(true)
            || ((self.system_sleeping || self.external_sleep.is_some())
                && self.own_suspend != Some(false))
    }

    fn wait_for_lock(&mut self, after: AfterLock, output: &mut Output) {
        self.serial += 1;
        let token = self.serial;
        self.lock_waits.push(LockWait { token, after });
        output.command(Command::LockTimeout(token));
        output.command(Command::RequestLock);
    }

    fn prune_lock_waits(&mut self, output: &mut Output) {
        let mut i = 0;
        while i < self.lock_waits.len() {
            let wait = self.lock_waits[i];
            let cancelled = match wait.after {
                AfterLock::Action { id, .. } => !self.policy.action_pending(id),
                AfterLock::ExternalSleep(generation) => self.external_sleep != Some(generation),
                AfterLock::ManualSuspend => false,
            };
            if cancelled {
                self.lock_waits.remove(i);
                output.command(Command::CancelLockTimeout(wait.token));
                if let AfterLock::Action { id, .. } = wait.after {
                    let effects = self.policy.completed(id, false);
                    self.apply(effects, output);
                }
            } else {
                i += 1;
            }
        }
    }

    fn suspend(&mut self, id: Option<u64>, lock_required: bool, output: &mut Output) {
        if let Some(id) = id {
            self.suspend_ids.push(id);
        }
        self.own_suspend = Some(self.own_suspend.unwrap_or(false) || lock_required);
        if !self.suspend_in_flight {
            self.suspend_in_flight = true;
            output.actions.push(Action::Suspend);
        }
    }

    fn finish_action(&mut self, id: u64, result: Result<(), String>, output: &mut Output) {
        self.admissions.retain(|(pending, _)| *pending != id);
        if let Err(error) = &result {
            output.error(error.clone());
        }
        let effects = self.policy.completed(id, result.is_ok());
        self.apply(effects, output);
    }
}
