//! Idle policy without clocks or platform calls. Tokens reject callbacks from replaced watches.
use crate::idle_config::{IdleBehavior, IdleConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Deferred,
    Fading,
    Running(u64),
    Applied,
    Failed,
}

struct Rule {
    behavior: IdleBehavior,
    watch: u64,
    phase: Phase,
}

struct Pending {
    id: u64,
    behavior: IdleBehavior,
    cancelled: bool,
}

#[derive(Debug)]
pub(crate) enum Effect {
    Watch {
        index: usize,
        token: u64,
        seconds: u32,
    },
    Unwatch(usize),
    Fade {
        token: u64,
        seconds: u32,
    },
    HideFade,
    Run {
        id: u64,
        behavior: IdleBehavior,
    },
    Resume(IdleBehavior),
}

pub(crate) struct Policy {
    config: IdleConfig,
    rules: Vec<Rule>,
    pending: Vec<Pending>,
    serial: u64,
    fade: Option<u64>,
    locked: bool,
    inhibited: bool,
    sleeping: bool,
    agents_working: bool,
    deferred_resume: Vec<IdleBehavior>,
}

impl Policy {
    pub fn new() -> Self {
        Self {
            config: IdleConfig {
                pre_action_fade_seconds: 0,
                behaviors: Vec::new(),
            },
            rules: Vec::new(),
            pending: Vec::new(),
            serial: 0,
            fade: None,
            locked: false,
            inhibited: false,
            sleeping: false,
            agents_working: false,
            deferred_resume: Vec::new(),
        }
    }

    pub fn has_deferred(&self) -> bool {
        self.rules.iter().any(|r| r.phase == Phase::Deferred)
    }

    fn token(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    fn watch(&mut self, index: usize, effects: &mut Vec<Effect>) {
        let token = self.token();
        let rule = &mut self.rules[index];
        rule.watch = token;
        effects.push(Effect::Unwatch(index));
        let seconds = if self.locked && rule.behavior.locked_timeout_seconds > 0 {
            rule.behavior.locked_timeout_seconds
        } else {
            rule.behavior.timeout_seconds
        };
        if rule.behavior.enabled && seconds > 0 && !self.inhibited && !self.sleeping {
            effects.push(Effect::Watch {
                index,
                token,
                seconds,
            });
        }
    }

    fn reset_rule(&mut self, index: usize, effects: &mut Vec<Effect>) {
        match self.rules[index].phase {
            Phase::Applied => effects.push(Effect::Resume(self.rules[index].behavior.clone())),
            Phase::Running(id) => {
                if let Some(p) = self.pending.iter_mut().find(|p| p.id == id) {
                    p.cancelled = true;
                }
            }
            _ => {}
        }
        self.rules[index].phase = Phase::Waiting;
    }

    fn hide_fade(&mut self, effects: &mut Vec<Effect>) {
        if self.fade.take().is_some() {
            effects.push(Effect::HideFade);
        }
    }

    fn reset(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.hide_fade(&mut effects);
        for i in 0..self.rules.len() {
            self.reset_rule(i, &mut effects);
            self.watch(i, &mut effects);
        }
        effects
    }

    pub fn configure(&mut self, config: IdleConfig) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.hide_fade(&mut effects);
        for i in 0..self.rules.len() {
            self.reset_rule(i, &mut effects);
            effects.push(Effect::Unwatch(i));
        }
        self.rules = config
            .behaviors
            .iter()
            .cloned()
            .map(|behavior| Rule {
                behavior,
                watch: 0,
                phase: Phase::Waiting,
            })
            .collect();
        self.config = config;
        for i in 0..self.rules.len() {
            self.watch(i, &mut effects);
        }
        effects
    }

    pub fn inhibit(&mut self, inhibited: bool) -> Vec<Effect> {
        if self.inhibited == inhibited {
            return Vec::new();
        }
        self.inhibited = inhibited;
        // Beginning inhibition also restores screens we turned off. Release starts fresh countdowns.
        self.reset()
    }

    pub fn sleeping(&mut self, sleeping: bool) -> Vec<Effect> {
        if self.sleeping == sleeping {
            return Vec::new();
        }
        self.sleeping = sleeping;
        if !sleeping {
            let mut effects = self.reset();
            effects.extend(self.deferred_resume.drain(..).map(Effect::Resume));
            return effects;
        }
        let mut effects = Vec::new();
        self.hide_fade(&mut effects);
        for i in 0..self.rules.len() {
            // Keep completed effects until waking, not PrepareForSleep(true).
            if self.rules[i].phase != Phase::Applied {
                self.reset_rule(i, &mut effects);
            }
            self.watch(i, &mut effects);
        }
        effects
    }

    pub fn locked(&mut self, locked: bool) -> Vec<Effect> {
        if self.locked == locked {
            return Vec::new();
        }
        self.locked = locked;
        if !locked {
            return self.reset();
        }
        let mut effects = Vec::new();
        // A completed screen-off action must retain its resume listener when the session locks.
        for i in 0..self.rules.len() {
            if self.rules[i].phase == Phase::Waiting {
                self.watch(i, &mut effects);
            }
        }
        effects
    }

    pub fn idle(&mut self, index: usize, token: u64, idle: bool) -> Vec<Effect> {
        let Some(rule) = self.rules.get(index) else {
            return Vec::new();
        };
        if rule.watch != token || self.inhibited || self.sleeping {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if !idle {
            // A fresh watch starts active; only an actual pending/applied rule needs cancelling.
            if rule.phase == Phase::Waiting {
                return effects;
            }
            self.reset_rule(index, &mut effects);
            // A running/applied rule can have retained its pre-lock notification.
            self.watch(index, &mut effects);
            if self.fade.is_some() {
                self.hide_fade(&mut effects);
                for i in 0..self.rules.len() {
                    if self.rules[i].phase == Phase::Fading {
                        self.rules[i].phase = Phase::Waiting;
                        self.watch(i, &mut effects);
                    }
                }
            }
            return effects;
        }
        if rule.phase != Phase::Waiting {
            return effects;
        }
        if self.agents_working && rule.behavior.action.deferred_by_agents() {
            self.rules[index].phase = Phase::Deferred;
            return effects;
        }
        self.trigger(index, &mut effects);
        effects
    }

    fn trigger(&mut self, index: usize, effects: &mut Vec<Effect>) {
        if self.config.pre_action_fade_seconds == 0 {
            self.run(index, effects);
        } else {
            self.rules[index].phase = Phase::Fading;
            if self.fade.is_none() {
                let token = self.token();
                self.fade = Some(token);
                effects.push(Effect::Fade {
                    token,
                    seconds: self.config.pre_action_fade_seconds,
                });
            }
        }
    }

    pub fn set_agents_working(
        &mut self,
        working: bool,
        mut still_idle: impl FnMut(usize, u64) -> bool,
    ) -> Vec<Effect> {
        if self.agents_working == working {
            return Vec::new();
        }
        self.agents_working = working;
        let mut effects = Vec::new();
        if working {
            for i in 0..self.rules.len() {
                if !self.rules[i].behavior.action.deferred_by_agents() {
                    continue;
                }
                match self.rules[i].phase {
                    Phase::Fading => self.rules[i].phase = Phase::Deferred,
                    Phase::Running(id) => {
                        if let Some(p) = self.pending.iter_mut().find(|p| p.id == id) {
                            p.cancelled = true;
                        }
                        self.rules[i].phase = Phase::Deferred;
                    }
                    _ => {}
                }
            }
            if self.fade.is_some() && !self.rules.iter().any(|r| r.phase == Phase::Fading) {
                self.hide_fade(&mut effects);
            }
            return effects;
        }
        if self.inhibited || self.sleeping {
            for rule in &mut self.rules {
                if rule.phase == Phase::Deferred {
                    rule.phase = Phase::Waiting;
                }
            }
            return effects;
        }
        for i in 0..self.rules.len() {
            if self.rules[i].phase != Phase::Deferred {
                continue;
            }
            let watch = self.rules[i].watch;
            if !still_idle(i, watch) {
                self.rules[i].phase = Phase::Waiting;
                self.watch(i, &mut effects);
                continue;
            }
            self.trigger(i, &mut effects);
        }
        effects
    }

    fn run(&mut self, index: usize, effects: &mut Vec<Effect>) {
        let id = self.token();
        let behavior = self.rules[index].behavior.clone();
        self.rules[index].phase = Phase::Running(id);
        self.pending.push(Pending {
            id,
            behavior: behavior.clone(),
            cancelled: false,
        });
        effects.push(Effect::Run { id, behavior });
    }

    pub fn fade_complete(&mut self, token: u64) -> Vec<Effect> {
        if self.fade != Some(token) || self.inhibited || self.sleeping {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.hide_fade(&mut effects);
        for i in 0..self.rules.len() {
            if self.rules[i].phase == Phase::Fading {
                if self.agents_working && self.rules[i].behavior.action.deferred_by_agents() {
                    self.rules[i].phase = Phase::Deferred;
                } else {
                    self.run(i, &mut effects);
                }
            }
        }
        effects
    }

    pub fn action_pending(&self, id: u64) -> bool {
        self.pending.iter().any(|p| p.id == id && !p.cancelled)
    }

    pub fn action_watch(&self, id: u64) -> Option<(usize, u64)> {
        if !self.action_pending(id) {
            return None;
        }
        self.rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.phase == Phase::Running(id))
            .map(|(index, rule)| (index, rule.watch))
    }

    pub fn completed(&mut self, id: u64, success: bool) -> Vec<Effect> {
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return Vec::new();
        };
        let pending = self.pending.remove(index);
        if pending.cancelled {
            if !success {
                return Vec::new();
            }
            if self.sleeping {
                self.deferred_resume.push(pending.behavior);
                return Vec::new();
            }
            return vec![Effect::Resume(pending.behavior)];
        }
        if let Some(rule) = self
            .rules
            .iter_mut()
            .find(|r| r.phase == Phase::Running(id))
        {
            // Do not retry a failed command in a tight loop: the next activity cycle can try again.
            rule.phase = if success {
                Phase::Applied
            } else {
                Phase::Failed
            };
        }
        Vec::new()
    }

    pub fn cancel(&mut self) -> Vec<Effect> {
        self.reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idle_config::IdleAction;

    fn configured(fade: u32) -> (Policy, usize, u64) {
        let mut p = Policy::new();
        let mut behavior = IdleBehavior::default();
        behavior.enabled = true;
        behavior.timeout_seconds = 60;
        behavior.locked_timeout_seconds = 10;
        behavior.action = IdleAction::ScreenOff;
        let effects = p.configure(IdleConfig {
            pre_action_fade_seconds: fade,
            behaviors: vec![behavior],
        });
        let (index, token) = effects
            .iter()
            .find_map(|e| match e {
                Effect::Watch { index, token, .. } => Some((*index, *token)),
                _ => None,
            })
            .unwrap();
        (p, index, token)
    }

    fn run_id(effects: &[Effect]) -> u64 {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Run { id, .. } => Some(*id),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn activity_cancels_warning_and_stale_deadline() {
        let (mut p, i, t) = configured(2);
        let effects = p.idle(i, t, true);
        let fade = effects
            .iter()
            .find_map(|e| match e {
                Effect::Fade { token, .. } => Some(*token),
                _ => None,
            })
            .unwrap();
        assert!(
            p.idle(i, t, false)
                .iter()
                .any(|e| matches!(e, Effect::HideFade))
        );
        assert!(p.fade_complete(fade).is_empty());
    }

    #[test]
    fn late_screen_off_completion_is_undone_after_inhibition() {
        let (mut p, i, t) = configured(0);
        let id = run_id(&p.idle(i, t, true));
        p.inhibit(true);
        assert!(!p.action_pending(id));
        assert!(matches!(
            p.completed(id, true).as_slice(),
            [Effect::Resume(_)]
        ));
        assert!(p.completed(id, true).is_empty());
        assert!(p.idle(i, t, true).is_empty());
        assert!(
            p.inhibit(false)
                .iter()
                .any(|e| matches!(e, Effect::Watch { seconds: 60, .. }))
        );
    }

    #[test]
    fn locking_preserves_completed_screen_off_and_unlock_restores_once() {
        let (mut p, i, t) = configured(0);
        let id = run_id(&p.idle(i, t, true));
        p.completed(id, true);
        assert!(p.locked(true).is_empty());
        assert_eq!(
            p.locked(false)
                .iter()
                .filter(|e| matches!(e, Effect::Resume(_)))
                .count(),
            1
        );
        assert!(p.idle(i, t, false).is_empty());
    }

    #[test]
    fn locked_timeout_replaces_only_waiting_watch() {
        let (mut p, i, t) = configured(0);
        let effects = p.locked(true);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Watch { seconds: 10, .. }))
        );
        assert!(p.idle(i, t, true).is_empty());
    }

    #[test]
    fn activity_after_lock_rearms_retained_watch_with_locked_timeout() {
        let (mut p, i, old) = configured(0);
        let id = run_id(&p.idle(i, old, true));
        p.completed(id, true);
        p.locked(true);
        let effects = p.idle(i, old, false);
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, Effect::Resume(_)))
                .count(),
            1
        );
        let new = effects
            .iter()
            .find_map(|e| match e {
                Effect::Watch {
                    token, seconds: 10, ..
                } => Some(*token),
                _ => None,
            })
            .expect("next cycle must use the locked timeout");
        assert!(p.idle(i, old, true).is_empty());
        assert!(
            p.idle(i, new, true)
                .iter()
                .any(|e| matches!(e, Effect::Run { .. }))
        );
    }

    #[test]
    fn reload_and_sleep_invalidate_pending_suspend() {
        let (mut p, i, t) = configured(0);
        let id = run_id(&p.idle(i, t, true));
        p.sleeping(true);
        assert!(!p.action_pending(id));
        assert!(p.idle(i, t, true).is_empty());
        p.configure(IdleConfig {
            pre_action_fade_seconds: 0,
            behaviors: vec![],
        });
        assert!(p.sleeping(false).is_empty());
    }

    #[test]
    fn sleep_defers_resume_commands_until_waking() {
        let (mut p, i, t) = configured(0);
        let id = run_id(&p.idle(i, t, true));
        assert!(
            !p.sleeping(true)
                .iter()
                .any(|e| matches!(e, Effect::Resume(_)))
        );
        assert!(p.completed(id, true).is_empty());
        assert_eq!(
            p.sleeping(false)
                .iter()
                .filter(|e| matches!(e, Effect::Resume(_)))
                .count(),
            1
        );
        assert!(p.sleeping(false).is_empty());
    }

    #[test]
    fn failed_action_waits_for_activity_and_uses_current_locked_timeout() {
        let (mut p, i, t) = configured(0);
        let id = run_id(&p.idle(i, t, true));
        p.locked(true);
        assert!(p.completed(id, false).is_empty());
        assert!(
            p.idle(i, t, true).is_empty(),
            "failure must not retry while still idle"
        );
        let effects = p.idle(i, t, false);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Resume(_) | Effect::Run { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Watch { seconds: 10, .. }))
        );
    }

    #[test]
    fn agents_working_defers_only_lock_and_suspend_and_catches_up_with_fade() {
        let mut p = Policy::new();
        let mut screen = IdleBehavior::default();
        screen.name = "screen".into();
        screen.enabled = true;
        screen.timeout_seconds = 30;
        screen.action = IdleAction::ScreenOff;

        let mut lock = IdleBehavior::default();
        lock.name = "lock".into();
        lock.enabled = true;
        lock.timeout_seconds = 60;
        lock.action = IdleAction::Lock;

        let effects = p.configure(IdleConfig {
            pre_action_fade_seconds: 2,
            behaviors: vec![screen, lock],
        });
        let tokens: Vec<u64> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Watch { token, .. } => Some(*token),
                _ => None,
            })
            .collect();
        assert_eq!(tokens.len(), 2);

        assert!(p.set_agents_working(true, |_, _| true).is_empty());
        // ScreenOff fades and runs while agents work.
        let screen_effects = p.idle(0, tokens[0], true);
        let fade = screen_effects
            .iter()
            .find_map(|e| match e {
                Effect::Fade { token, .. } => Some(*token),
                _ => None,
            })
            .unwrap();
        assert!(p.idle(1, tokens[1], true).is_empty());
        assert!(p.has_deferred());

        let completed = p.fade_complete(fade);
        assert!(completed.iter().any(|e| matches!(e, Effect::Run { .. })));
        assert!(p.has_deferred());

        // When agents stop and seat is still idle, deferred Lock enters Fade.
        let catchup = p.set_agents_working(false, |_, _| true);
        assert!(!p.has_deferred());
        assert!(catchup.iter().any(|e| matches!(e, Effect::Fade { .. })));
    }
}
