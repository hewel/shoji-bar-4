use super::*;
use crate::idle_config::{IdleAction, IdleBehavior, IdleConfig};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Observation {
    Watch { index: usize, token: u64, seconds: u32 },
    Warning(bool),
    Lock,
    LockTimeout(u64),
    CancelTimeout(u64),
    Suspend,
    SleepReady(u64),
    Command { id: Option<u64>, text: String },
    Screen { id: Option<u64>, on: bool },
    Error(String),
    Caffeine(bool),
}

#[derive(Default)]
struct Scenario {
    observations: Vec<Observation>,
    idle: HashMap<(usize, u64), bool>,
    lock: LockState,
    lock_request: usize,
}

impl Default for LockState { fn default() -> Self { Self::Locking } }

impl Runtime<Scenario> {
    fn activity(&mut self, index: usize, token: u64, idle: bool) {
        self.platform.idle.insert((index, token), idle);
        self.input(Event::Idle { index, token });
    }
    fn lock_changed(&mut self, lock: LockState) {
        self.platform.lock = lock;
        self.input(Event::LockChanged);
    }
}

impl Scenario {}

impl Platform for Scenario {
    fn caffeine(&mut self, enabled: bool) { self.observations.push(Observation::Caffeine(enabled)); }
    fn inhibitors(&mut self, _count: usize) {}
    fn capability(&mut self, _sleep_delay: bool, _screensaver: bool) {}
    fn warning(&mut self, enabled: bool) { self.observations.push(Observation::Warning(enabled)); }
    fn error(&mut self, error: String) { self.observations.push(Observation::Error(error)); }
    fn set_caffeine(&mut self, _enabled: bool) {}
    fn sleep_ready(&mut self, generation: u64) { self.observations.push(Observation::SleepReady(generation)); }
    fn lock_state(&mut self) -> LockState { self.lock }
    fn request_lock(&mut self) { self.lock_request += 1; self.observations.push(Observation::Lock); }
    fn watch(&mut self, index: usize, token: u64, seconds: u32, _sender: UiSender<Message>) -> bool {
        self.observations.push(Observation::Watch { index, token, seconds });
        true
    }
    fn idle_now(&mut self, index: usize, token: u64) -> bool { self.idle.get(&(index, token)).copied().unwrap_or(false) }
    fn refresh_activity(&mut self, coordinator: &mut Coordinator, _sender: UiSender<Message>) {
        let watches: Vec<_> = coordinator.watch_identities().collect();
        for (index, token) in watches {
            coordinator.refresh_idle(index, self.idle.get(&(index, token)).copied().unwrap_or(false));
        }
    }
    fn unwatch(&mut self, _index: usize) {}
    fn fade(&mut self, _token: u64, _seconds: u32, _sender: UiSender<Message>) {}
    fn hide_fade(&mut self) {}
    fn lock_timeout(&mut self, token: u64, _sender: UiSender<Message>) { self.observations.push(Observation::LockTimeout(token)); }
    fn cancel_lock_timeout(&mut self, token: u64) { self.observations.push(Observation::CancelTimeout(token)); }
    fn suspend(&mut self, _sender: UiSender<Message>) { self.observations.push(Observation::Suspend); }
    fn power(&mut self, id: Option<u64>, on: bool, _admission: Option<Admission>) -> Result<(), String> {
        self.observations.push(Observation::Screen { id, on });
        Ok(())
    }
    fn command(&mut self, id: Option<u64>, command: String, _admission: Option<Admission>, _sender: UiSender<Message>) {
        self.observations.push(Observation::Command { id, text: command });
    }
    fn close(&mut self) {}
}

fn scenario(action: IdleAction, fade: u32) -> (Runtime<Scenario>, usize, u64) {
    let (sender, _) = create_root(|| ui_channel(|_: Message| {}));
    let mut runtime = Runtime::new(sender, Scenario::default());
    runtime.input(Event::Configure(IdleConfig {
        pre_action_fade_seconds: fade,
        behaviors: vec![IdleBehavior {
            name: "test".into(), enabled: true, timeout_seconds: 10,
            action, command: "record".into(), ..IdleBehavior::default()
        }],
    }));
    let (index, token) = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::Watch { index, token, .. } => Some((*index, *token)), _ => None,
    }).unwrap();
    (runtime, index, token)
}

fn has(runtime: &Runtime<Scenario>, expected: Observation) -> bool {
    runtime.platform.observations.contains(&expected)
}

#[test]
fn media_playback_cancels_pending_sleep_and_restarts_a_full_countdown() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity(index, token, true);
    assert!(has(&runtime, Observation::Lock));
    runtime.input(Event::MediaPlaying(true));
    runtime.lock_changed(LockState::Locked);
    assert!(!has(&runtime, Observation::Suspend));
    assert!(runtime.sleep_hold.is_none());

    runtime.platform.observations.clear();
    runtime.input(Event::MediaPlaying(false));
    let fresh = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::Watch { index: i, token, seconds: 10 } if *i == index => Some(*token),
        _ => None,
    }).expect("stopping playback starts the complete idle timeout");
    assert_ne!(fresh, token);
    runtime.activity(index, token, true);
    assert!(!has(&runtime, Observation::Suspend));
    runtime.activity(index, fresh, true);
    assert!(has(&runtime, Observation::Suspend));
}

#[test]
fn media_caffeine_and_application_inhibitors_release_independently() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.input(Event::MediaPlaying(true));
    runtime.input(Event::Caffeine(true));
    runtime.input(Event::Bus(crate::idle_bus::Event::Inhibitors(1)));
    runtime.input(Event::Caffeine(false));
    runtime.input(Event::Bus(crate::idle_bus::Event::Inhibitors(0)));
    runtime.platform.observations.clear();
    runtime.activity(index, token, true);
    assert!(!has(&runtime, Observation::Lock));
    assert!(!has(&runtime, Observation::Suspend));
    assert!(!runtime.platform.observations.iter().any(|o| matches!(o, Observation::Watch { .. })));

    // Explicit Sleep is not an idle action and remains available while music plays.
    runtime.lock_changed(LockState::Locked);
    runtime.input(Event::Suspend);
    assert!(has(&runtime, Observation::Suspend));
}

#[test]
fn stopping_media_does_not_release_caffeine_or_application_inhibition() {
    for application in [false, true] {
        let (mut runtime, _, _) = scenario(IdleAction::LockAndSuspend, 0);
        runtime.input(Event::MediaPlaying(true));
        runtime.input(if application {
            Event::Bus(crate::idle_bus::Event::Inhibitors(1))
        } else {
            Event::Caffeine(true)
        });
        runtime.platform.observations.clear();
        runtime.input(Event::MediaPlaying(false));
        assert!(!runtime.platform.observations.iter().any(|o| matches!(o, Observation::Watch { .. })));
        runtime.input(if application {
            Event::Bus(crate::idle_bus::Event::Inhibitors(0))
        } else {
            Event::Caffeine(false)
        });
        assert!(runtime.platform.observations.iter().any(|o| matches!(o, Observation::Watch { seconds: 10, .. })));
    }
}

#[test]
fn activity_before_admission_cancels_lock_and_sleep_without_unlock_gap() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity( index, token, true);
    assert!(has(&runtime, Observation::Lock), "observations: {:?}", runtime.platform.observations);
    runtime.activity( index, token, false);
    runtime.lock_changed( LockState::Locked);
    assert!(!has(&runtime, Observation::Suspend));
    assert!(runtime.sleep_hold.is_none());
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 9, sleeping: false }));
    assert!(!has(&runtime, Observation::SleepReady(9)));
}

#[test]
fn acknowledged_lock_still_rechecks_before_preparing_sleep() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity( index, token, true);
    runtime.lock_changed( LockState::Locked);
    assert!(has(&runtime, Observation::Suspend));
    assert!(runtime.sleep_hold.is_some());
    runtime.lock_changed( LockState::Locking);
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 8, sleeping: true }));
    assert!(!has(&runtime, Observation::SleepReady(8)));
    assert_eq!(runtime.platform.lock_request, 2);
    runtime.lock_changed( LockState::Locked);
    assert!(has(&runtime, Observation::SleepReady(8)));
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 8, sleeping: false }));
    assert!(runtime.sleep_hold.is_none());
}

#[test]
fn lock_refusal_and_timeout_never_suspend_or_release_external_delay() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity( index, token, true);
    runtime.lock_changed( LockState::Refused);
    assert!(!has(&runtime, Observation::Suspend));
    assert!(runtime.sleep_hold.is_none());
    let timeout = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::LockTimeout(token) => Some(*token), _ => None,
    }).unwrap();
    runtime.input(Event::LockTimeout(timeout));
    assert!(!has(&runtime, Observation::Error("Lock confirmation timed out; automatic suspend was cancelled".into())));
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 1, sleeping: true }));
    let external = runtime.platform.observations.iter().filter_map(|observation| match observation {
        Observation::LockTimeout(token) => Some(*token), _ => None,
    }).last().unwrap();
    runtime.input(Event::LockTimeout(external));
    assert!(!has(&runtime, Observation::SleepReady(1)));
}

#[test]
fn canceled_pending_command_is_invalidated_before_launch() {
    let (mut runtime, index, token) = scenario(IdleAction::Command, 0);
    runtime.activity( index, token, true);
    assert_eq!(runtime.admissions.len(), 1);
    let admission = runtime.admissions[0].1.clone();
    runtime.input(Event::Configure(IdleConfig { pre_action_fade_seconds: 0, behaviors: vec![] }));
    assert!(!admission.start());
    assert!(!has(&runtime, Observation::Command { id: None, text: "resume".into() }));
}

#[test]
fn resume_only_after_wake_and_stale_lock_callback_is_inert() {
    let (mut runtime, index, token) = scenario(IdleAction::ScreenOff, 0);
    runtime.activity( index, token, true);
    let id = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::Screen { id, on: false } => *id, _ => None,
    }).expect("the idle action requests screen off");
    runtime.input(Event::PowerDone { id: Some(id), on: false, result: Ok(()) });
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 4, sleeping: true }));
    assert!(!has(&runtime, Observation::Screen { id: None, on: true }));
    runtime.input(Event::Bus(crate::idle_bus::Event::PrepareForSleep { generation: 4, sleeping: false }));
    assert!(has(&runtime, Observation::Screen { id: None, on: true }));
    let timeout = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::LockTimeout(token) => Some(*token), _ => None,
    }).unwrap();
    runtime.input(Event::LockTimeout(timeout));
    assert!(!has(&runtime, Observation::Error("Lock confirmation timed out; automatic suspend was cancelled".into())));
}

#[test]
fn watch_refresh_preserves_sleep_hold_during_locked_rearm() {
    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity(index, token, true);
    assert!(runtime.sleep_hold.is_some());
    runtime.lock_changed(LockState::Locked);
    assert!(has(&runtime, Observation::Suspend));
    assert!(runtime.sleep_hold.is_some(), "watch refresh must not release the pending sleep hold");
}

#[test]
fn already_locked_manual_suspend_does_not_need_a_new_notification() {
    let (sender, _) = create_root(|| ui_channel(|_: Message| {}));
    let mut runtime = Runtime::new(sender, Scenario { lock: LockState::Locked, ..Scenario::default() });
    runtime.input(Event::Suspend);
    assert!(has(&runtime, Observation::Suspend));
    assert!(runtime.coordinator.sleep_hold_wanted());
}

#[test]
fn screen_and_command_admissions_cancel_independently() {
    let (sender, _) = create_root(|| ui_channel(|_: Message| {}));
    let mut runtime = Runtime::new(sender, Scenario::default());
    runtime.input(Event::Configure(IdleConfig {
        pre_action_fade_seconds: 0,
        behaviors: vec![
            IdleBehavior { name: "screen".into(), enabled: true, timeout_seconds: 1, action: IdleAction::ScreenOff, ..IdleBehavior::default() },
            IdleBehavior { name: "command".into(), enabled: true, timeout_seconds: 1, action: IdleAction::Command, command: "safe".into(), ..IdleBehavior::default() },
        ],
    }));
    runtime.activity(0, 1, true);
    runtime.activity(1, 2, true);
    runtime.platform.idle.insert((1, 2), false);
    runtime.activity(0, 1, false);
    assert_eq!(runtime.admissions.len(), 1, "only the still-active rule retains admission control");
    assert!(runtime.admissions[0].1.start(), "the other rule must remain executable");
}

#[test]
fn fade_and_lock_completion_refresh_activity_before_admission() {
    let (mut runtime, index, token) = scenario(IdleAction::Command, 2);
    runtime.activity(index, token, true);
    runtime.platform.idle.insert((index, token), false);
    runtime.input(Event::Fade(2));
    assert!(!runtime.platform.observations.iter().any(|observation| matches!(observation, Observation::Command { id: Some(_), .. })));

    let (mut runtime, index, token) = scenario(IdleAction::LockAndSuspend, 0);
    runtime.activity(index, token, true);
    runtime.platform.idle.insert((index, token), false);
    runtime.lock_changed(LockState::Locked);
    assert!(!has(&runtime, Observation::Suspend));
}

#[test]
fn terminal_screen_and_abandoned_command_release_admissions() {
    let (mut runtime, index, token) = scenario(IdleAction::ScreenOff, 0);
    runtime.activity(index, token, true);
    let id = runtime.platform.observations.iter().find_map(|observation| match observation {
        Observation::Screen { id, on: false } => *id, _ => None,
    }).unwrap();
    runtime.input(Event::PowerDone { id: Some(id), on: false, result: Ok(()) });
    assert!(runtime.admissions.is_empty());
    let (mut runtime, index, token) = scenario(IdleAction::Command, 0);
    runtime.activity(index, token, true);
    assert!(runtime.coordinator.admissions.len() == 1);
    let id = runtime.coordinator.admissions[0].0;
    runtime.input(Event::Abandoned(id));
    assert!(runtime.coordinator.admissions.is_empty());
}

#[test]
fn warning_timer_activity_cancels_action_and_stale_deadline() {
    let (mut runtime, index, watch_token) = scenario(IdleAction::Command, 2);
    runtime.activity( index, watch_token, true);
    runtime.input(Event::Fade(2));
    assert!(runtime.platform.observations.iter().any(|observation| matches!(observation, Observation::Command { id: Some(_), .. })));
    runtime.activity( index, watch_token, false);
    runtime.input(Event::Fade(2));
    assert_eq!(runtime.platform.observations.iter().filter(|observation| matches!(observation, Observation::Command { id: Some(_), .. })).count(), 1);
}
