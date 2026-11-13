//! Session-shared AI quota polling. CLI diagnostics and credential files never
//! enter published state. Usage and public-reset jobs have independent workers,
//! singleflight requests, bounded refresh mailboxes, and monotonic backoff.

mod adapters;
mod model;
mod runtime;

use anti_webui::prelude::*;
pub(crate) use model::{
    Entry, Metric, Report, ResetEvent, Resets, date, now, parse_report, parse_resets,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AiUsageConfig {
    pub enabled: bool,
    pub provider: String,
    pub refresh_minutes: u64,
    pub resets_enabled: bool,
    pub resets_refresh_minutes: u64,
    pub show_countdown: bool,
    pub show_reset_indicator: bool,
}

impl Default for AiUsageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "auto".into(),
            refresh_minutes: 5,
            resets_enabled: true,
            resets_refresh_minutes: 10,
            show_countdown: true,
            show_reset_indicator: true,
        }
    }
}

impl AiUsageConfig {
    pub(crate) fn normalized(mut self) -> Self {
        self.refresh_minutes = self.refresh_minutes.clamp(1, 120);
        self.resets_refresh_minutes = self.resets_refresh_minutes.clamp(5, 120);
        if self.provider.is_empty() {
            self.provider = "auto".into();
        }
        self
    }

    /// A malformed field must not erase another field's explicit privacy opt-out.
    pub(crate) fn from_json(value: &serde_json::Value) -> Self {
        let default = Self::default();
        Self {
            enabled: value["enabled"].as_bool().unwrap_or(default.enabled),
            provider: value["provider"]
                .as_str()
                .unwrap_or(&default.provider)
                .to_owned(),
            refresh_minutes: value["refresh_minutes"]
                .as_u64()
                .unwrap_or(default.refresh_minutes),
            resets_enabled: value["resets_enabled"]
                .as_bool()
                .unwrap_or(default.resets_enabled),
            resets_refresh_minutes: value["resets_refresh_minutes"]
                .as_u64()
                .unwrap_or(default.resets_refresh_minutes),
            show_countdown: value["show_countdown"]
                .as_bool()
                .unwrap_or(default.show_countdown),
            show_reset_indicator: value["show_reset_indicator"]
                .as_bool()
                .unwrap_or(default.show_reset_indicator),
        }
        .normalized()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    MissingCli,
    Cli,
    Timeout,
    Format,
    Network,
    RateLimit,
    Start,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Job<T> {
    pub data: Option<Arc<T>>,
    pub busy: bool,
    pub error: Option<Error>,
    pub updated_at: Option<i64>,
    pub next_at: Option<i64>,
}

impl<T> Default for Job<T> {
    fn default() -> Self {
        Self {
            data: None,
            busy: false,
            error: None,
            updated_at: None,
            next_at: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub usage: Signal<Job<Report>>,
    pub resets: Signal<Job<Resets>>,
}

thread_local! {
    static STATE: State = create_root(|| State {
        usage: signal(Job::default()),
        resets: signal(Job::default()),
    }).0;
    static WORKERS: RefCell<Option<Workers>> = const { RefCell::new(None) };
}

/// Allocating observable state alone performs no I/O (including native previews).
pub(crate) fn state() -> State {
    STATE.with(|state| *state)
}

struct Workers {
    usage: runtime::Worker,
    resets: runtime::Worker,
}

/// Install once at session startup, never for each output or panel opening.
pub(crate) fn install() {
    if WORKERS.with(|workers| workers.borrow().is_some()) {
        return;
    }
    let state = state();
    WORKERS.with(|workers| {
        *workers.borrow_mut() = Some(Workers {
            usage: runtime::Worker::start("shoji AI usage", state.usage, 300, 5, adapters::usage),
            resets: runtime::Worker::start(
                "shoji AI public resets",
                state.resets,
                600,
                60,
                adapters::resets,
            ),
        });
    });
    effect(move || {
        let config = crate::settings::settings()
            .with(|settings| settings.ai_usage.clone())
            .normalized();
        WORKERS.with(|workers| {
            if let Some(workers) = workers.borrow().as_ref() {
                if workers
                    .usage
                    .configure(config.enabled, config.refresh_minutes * 60)
                {
                    state.usage.set(Job::default());
                }
                if workers.resets.configure(
                    config.enabled && config.resets_enabled,
                    config.resets_refresh_minutes * 60,
                ) {
                    state.resets.set(Job::default());
                }
            }
        });
    });
    on_cleanup(move || {
        WORKERS.with(|workers| workers.borrow_mut().take());
        state.usage.set(Job::default());
        state.resets.set(Job::default());
    });
}

/// Requests coalesce and respect each job's minimum gap and failure backoff.
pub(crate) fn refresh() {
    WORKERS.with(|workers| {
        if let Some(workers) = workers.borrow().as_ref() {
            workers.usage.refresh();
            workers.resets.refresh();
        }
    });
}

#[cfg(test)]
mod tests;
