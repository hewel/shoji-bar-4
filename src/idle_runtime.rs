//! Production and scenario adapters share one coordinator command-dispatch implementation.

use crate::display_power::{Admission, Power};
use crate::idle::Message;
use crate::idle_bus::{self, Bus};
use crate::idle_coordinator::{Action, Command, Coordinator, Event, LockState, Output};
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui::shell::{Idle, idle as watch_idle, lock_state};
use std::process::{Command as ProcessCommand, Stdio};

pub(crate) struct Runtime<P: Platform> {
    coordinator: Coordinator,
    pub(crate) platform: P,
    sender: UiSender<Message>,
    sleep_hold: Option<crate::lock::SleepHold>,
    admissions: Vec<(u64, Admission)>,
}

impl<P: Platform> Runtime<P> {
    pub fn new(sender: UiSender<Message>, platform: P) -> Self {
        Self {
            coordinator: Coordinator::new(),
            platform,
            sender,
            sleep_hold: None,
            admissions: Vec::new(),
        }
    }

    pub fn input(&mut self, event: Event) {
        if let Event::Completed { id, .. }
        | Event::Abandoned(id)
        | Event::PowerDone { id: Some(id), .. } = event
        {
            self.admissions.retain(|(current, _)| *current != id);
        }
        if let Event::Idle { index, token } = event {
            let idle = self.platform.idle_now(index, token);
            let refresh = self.coordinator.watch_state(index, token, true, idle);
            self.dispatch(refresh);
        } else if matches!(
            event,
            Event::Fade(_) | Event::LockChanged | Event::AgentWorking(false)
        ) {
            self.platform
                .refresh_activity(&mut self.coordinator, self.sender.clone());
        }
        let output = self.coordinator.input(event);
        self.dispatch(output);
    }

    fn dispatch(&mut self, output: Output) {
        let Output {
            actions,
            commands,
            admission_cancelled,
            caffeine,
            inhibitors,
            capability,
            agents_working,
            agents_deferring,
            hold,
            warning,
        } = output;
        if let Some(enabled) = caffeine {
            self.platform.caffeine(enabled);
        }
        if let Some(count) = inhibitors {
            self.platform.inhibitors(count);
        }
        if let Some((sleep_delay, screensaver)) = capability {
            self.platform.capability(sleep_delay, screensaver);
        }
        if let Some(working) = agents_working {
            self.platform.agents_working(working);
        }
        if let Some(deferring) = agents_deferring {
            self.platform.agents_deferring(deferring);
        }
        let new_hold = match hold {
            Some(true) => self
                .sleep_hold
                .take()
                .or_else(|| Some(crate::lock::SleepHold::acquire())),
            Some(false) => None,
            None => self.sleep_hold.take(),
        };
        self.sleep_hold = new_hold;
        if warning {
            self.platform.warning(true);
        }
        for command in commands {
            match command {
                Command::Unwatch(index) => self.platform.unwatch(index),
                Command::Watch {
                    index,
                    token,
                    seconds,
                } => {
                    let available = self
                        .platform
                        .watch(index, token, seconds, self.sender.clone());
                    let idle = self.platform.idle_now(index, token);
                    let output = self.coordinator.watch_state(index, token, available, idle);
                    self.dispatch(output);
                }
                Command::Fade { token, seconds } => {
                    self.platform.fade(token, seconds, self.sender.clone())
                }
                Command::HideFade => {
                    self.platform.hide_fade();
                    self.platform.warning(false);
                }
                Command::ReadLock => {
                    let state = self.platform.lock_state();
                    let output = self.coordinator.lock_changed(state);
                    self.dispatch(output);
                }
                Command::Caffeine(enabled) => self.platform.set_caffeine(enabled),
                Command::LockTimeout(token) => {
                    self.platform.lock_timeout(token, self.sender.clone())
                }
                Command::CancelLockTimeout(token) => self.platform.cancel_lock_timeout(token),
                Command::RequestLock => {
                    self.platform.request_lock();
                    let state = self.platform.lock_state();
                    let output = self.coordinator.lock_changed(state);
                    self.dispatch(output);
                }
                Command::SleepReady(generation) => self.platform.sleep_ready(generation),
                Command::Error(error) => self.platform.error(error),
            }
        }
        // Only identities invalidated by the coordinator are canceled.
        for id in admission_cancelled {
            if let Some(index) = self
                .admissions
                .iter()
                .position(|(current, _)| *current == id)
            {
                self.admissions.remove(index).1.cancel();
            }
        }
        for action in actions {
            match action {
                Action::Suspend => self.platform.suspend(self.sender.clone()),
                Action::Screen { id, on, admission } => {
                    if let Some(id) = id
                        && let Some(admission) = admission.as_ref()
                    {
                        self.admissions.push((id, admission.clone()));
                    }
                    if let Err(error) = self.platform.power(id, on, admission) {
                        if let Some(id) = id {
                            self.input(Event::Completed {
                                id,
                                result: Err(error),
                            });
                        } else if !error.is_empty() {
                            self.input(Event::Error(error));
                        }
                    }
                }
                Action::Command {
                    id,
                    command,
                    admission,
                } => {
                    if let Some(id) = id
                        && let Some(admission) = admission.as_ref()
                    {
                        self.admissions.push((id, admission.clone()));
                    }
                    self.platform
                        .command(id, command, admission, self.sender.clone());
                }
            }
        }
    }

    pub fn cancel(&mut self) {
        self.input(Event::Cancel);
        for (_, admission) in &self.admissions {
            admission.cancel();
        }
        self.platform.close();
        self.sleep_hold = None;
    }
}

pub(crate) trait Platform {
    fn caffeine(&mut self, enabled: bool);
    fn inhibitors(&mut self, count: usize);
    fn capability(&mut self, sleep_delay: bool, screensaver: bool);
    fn agents_working(&mut self, working: bool);
    fn agents_deferring(&mut self, deferring: bool);
    fn warning(&mut self, enabled: bool);
    fn error(&mut self, error: String);
    fn set_caffeine(&mut self, enabled: bool);
    fn sleep_ready(&mut self, generation: u64);
    fn lock_state(&mut self) -> LockState;
    fn request_lock(&mut self);
    fn watch(&mut self, index: usize, token: u64, seconds: u32, sender: UiSender<Message>) -> bool;
    fn idle_now(&mut self, index: usize, _token: u64) -> bool;
    fn refresh_activity(&mut self, coordinator: &mut Coordinator, sender: UiSender<Message>);
    fn unwatch(&mut self, index: usize);
    fn fade(&mut self, token: u64, seconds: u32, sender: UiSender<Message>);
    fn hide_fade(&mut self);
    fn lock_timeout(&mut self, token: u64, sender: UiSender<Message>);
    fn cancel_lock_timeout(&mut self, token: u64);
    fn suspend(&mut self, sender: UiSender<Message>);
    fn power(
        &mut self,
        id: Option<u64>,
        on: bool,
        admission: Option<Admission>,
    ) -> Result<(), String>;
    fn command(
        &mut self,
        id: Option<u64>,
        command: String,
        admission: Option<Admission>,
        sender: UiSender<Message>,
    );
    fn close(&mut self);
}

pub(crate) struct Desktop {
    bus: Bus,
    power: Power,
    watches: Vec<Option<(Idle, Owner)>>,
    overlay: Option<Owner>,
    fade_timer: Option<Timeout>,
    lock_timeouts: Vec<(u64, Timeout)>,
}

impl Desktop {
    pub fn new(sender: UiSender<Message>) -> Self {
        let events = sender.clone();
        Self {
            bus: idle_bus::start(ui_channel(move |event| events.send(Message::Bus(event)))),
            power: Power::new(sender),
            watches: Vec::new(),
            overlay: None,
            fade_timer: None,
            lock_timeouts: Vec::new(),
        }
    }
}

impl Platform for Desktop {
    fn caffeine(&mut self, enabled: bool) {
        crate::idle::state().caffeine.set(enabled);
    }
    fn inhibitors(&mut self, count: usize) {
        crate::idle::state().inhibitors.set(count);
    }
    fn capability(&mut self, sleep_delay: bool, screensaver: bool) {
        let state = crate::idle::state();
        state.sleep_delay.set(sleep_delay);
        state.screensaver.set(screensaver);
    }
    fn agents_working(&mut self, working: bool) {
        crate::idle::state().agents_working.set(working);
    }
    fn agents_deferring(&mut self, deferring: bool) {
        crate::idle::state().agents_deferring.set(deferring);
    }
    fn warning(&mut self, enabled: bool) {
        crate::idle::state().warning.set(enabled);
    }
    fn error(&mut self, error: String) {
        eprintln!("shoji-bar-4 idle: {error}");
        crate::idle::state().error.set(error);
    }
    fn set_caffeine(&mut self, enabled: bool) {
        self.bus.set_caffeine(enabled);
    }
    fn sleep_ready(&mut self, generation: u64) {
        self.bus.sleep_ready(generation);
    }
    fn lock_state(&mut self) -> LockState {
        match lock_state() {
            anti_webui::shell::LockState::Unlocked => LockState::Unlocked,
            anti_webui::shell::LockState::Locking => LockState::Locking,
            anti_webui::shell::LockState::Locked => LockState::Locked,
            anti_webui::shell::LockState::Refused => LockState::Refused,
        }
    }
    fn request_lock(&mut self) {
        crate::lock::lock();
    }
    fn watch(&mut self, index: usize, token: u64, seconds: u32, sender: UiSender<Message>) -> bool {
        let (detector, owner) = create_root(move || {
            let detector = watch_idle(std::time::Duration::from_secs(u64::from(seconds)));
            let watched = detector.clone();
            effect(move || {
                let _ = watched.is_idle();
                sender.send(Message::Idle { index, token });
            });
            detector
        });
        if self.watches.len() <= index {
            self.watches.resize_with(index + 1, || None);
        }
        let available = detector.available();
        self.watches[index] = Some((detector, owner));
        available
    }
    fn idle_now(&mut self, index: usize, _token: u64) -> bool {
        self.watches
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|(detector, _)| detector.is_idle())
    }
    fn refresh_activity(&mut self, coordinator: &mut Coordinator, _sender: UiSender<Message>) {
        for (index, watch) in self.watches.iter().enumerate() {
            if let Some((detector, _)) = watch {
                coordinator.refresh_idle(index, detector.is_idle());
            }
        }
    }
    fn unwatch(&mut self, index: usize) {
        if let Some((_, owner)) = self.watches.get_mut(index).and_then(Option::take) {
            owner.dispose();
        }
    }
    fn fade(&mut self, token: u64, seconds: u32, sender: UiSender<Message>) {
        let (_, owner) = create_root(move || {
            for_each_output(move |output| {
                open_layer(
                    LayerOptions::overlay()
                        .anchor(Anchor::ALL)
                        .exclusive(Exclusive::Ignore)
                        .output(&output)
                        .namespace("shoji-bar-4-idle"),
                    move || crate::idle::warning_view(seconds),
                );
            })
        });
        self.overlay = Some(owner);
        if let Some(timer) = self.fade_timer.take() {
            timer.cancel();
        }
        self.fade_timer = Some(set_timeout(
            std::time::Duration::from_secs(u64::from(seconds)),
            move || sender.send(Message::Fade(token)),
        ));
    }
    fn hide_fade(&mut self) {
        if let Some(timer) = self.fade_timer.take() {
            timer.cancel();
        }
        if let Some(owner) = self.overlay.take() {
            owner.dispose();
        }
    }
    fn lock_timeout(&mut self, token: u64, sender: UiSender<Message>) {
        self.lock_timeouts.push((
            token,
            set_timeout(std::time::Duration::from_secs(8), move || {
                sender.send(Message::LockTimeout(token))
            }),
        ));
    }
    fn cancel_lock_timeout(&mut self, token: u64) {
        if let Some(index) = self
            .lock_timeouts
            .iter()
            .position(|(current, _)| *current == token)
        {
            self.lock_timeouts.remove(index).1.cancel();
        }
    }
    fn suspend(&mut self, sender: UiSender<Message>) {
        session().suspend().then(move |result| {
            sender.send(Message::SuspendDone(result.map_err(|e| e.to_string())))
        });
    }
    fn power(
        &mut self,
        id: Option<u64>,
        on: bool,
        admission: Option<Admission>,
    ) -> Result<(), String> {
        self.power.set(id, on, admission)
    }
    fn command(
        &mut self,
        id: Option<u64>,
        command: String,
        admission: Option<Admission>,
        sender: UiSender<Message>,
    ) {
        spawn_command(
            command,
            id.map(|id| (id, admission.expect("admitted command"))),
            sender,
        );
    }
    fn close(&mut self) {
        self.hide_fade();
        for (_, timeout) in std::mem::take(&mut self.lock_timeouts) {
            timeout.cancel();
        }
        for watch in std::mem::take(&mut self.watches).into_iter().flatten() {
            watch.1.dispose();
        }
        self.bus.stop();
    }
}

pub(crate) fn spawn_command(
    command: String,
    action: Option<(u64, Admission)>,
    sender: UiSender<Message>,
) {
    let id = action.as_ref().map(|(id, _)| *id);
    let failure = sender.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("shoji-idle-command".into())
        .spawn(move || {
            if let Some((id, admission)) = &action
                && !admission.start()
            {
                sender.send(Message::Abandoned(*id));
                return;
            }
            match ProcessCommand::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .stdin(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(id) = id {
                        sender.send(Message::Completed { id, result: Ok(()) });
                    }
                    match child.wait() {
                        Ok(status) if status.success() => {}
                        Ok(status) => sender
                            .send(Message::Error(format!("Idle command exited with {status}"))),
                        Err(error) => sender
                            .send(Message::Error(format!("Idle command wait failed: {error}"))),
                    }
                }
                Err(error) => {
                    let error = format!("Cannot start idle command: {error}");
                    if let Some(id) = id {
                        sender.send(Message::Completed {
                            id,
                            result: Err(error),
                        });
                    } else {
                        sender.send(Message::Error(error));
                    }
                }
            }
        })
    {
        let error = format!("Cannot start command worker: {error}");
        if let Some(id) = id {
            failure.send(Message::Completed {
                id,
                result: Err(error),
            });
        } else {
            failure.send(Message::Error(error));
        }
    }
}

#[cfg(test)]
mod tests;
