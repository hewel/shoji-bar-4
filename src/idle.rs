//! Idle installation and the shell's observable status. Sequencing lives in the coordinator.
use crate::idle_config::IdleConfig;
use crate::idle_runtime::{Desktop, Runtime};
use anti_webui::prelude::*;
use anti_webui::shell::lock_state;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub caffeine: Signal<bool>,
    pub inhibitors: Signal<usize>,
    pub media_playing: Signal<bool>,
    pub warning: Signal<bool>,
    pub error: Signal<String>,
    pub installed: Signal<bool>,
    pub sleep_delay: Signal<bool>,
    pub screensaver: Signal<bool>,
}

thread_local! {
    static STATE: State = create_root(|| State {
        caffeine: signal(false), inhibitors: signal(0), warning: signal(false),
        media_playing: signal(false),
        error: signal(String::new()), installed: signal(false),
        sleep_delay: signal(false), screensaver: signal(false),
    }).0;
    static SENDER: RefCell<Option<UiSender<Message>>> = const { RefCell::new(None) };
}

pub(crate) fn state() -> State {
    STATE.with(|s| *s)
}

pub(crate) type Message = crate::idle_coordinator::Event;

fn send(message: Message) {
    SENDER.with(|s| {
        if let Some(sender) = s.borrow().as_ref() {
            sender.send(message);
        }
    });
}

pub fn caffeine() -> bool {
    state().caffeine.get()
}

pub fn set_caffeine(enabled: bool) {
    if state().installed.get_untracked() {
        send(Message::Caffeine(enabled));
    }
}

pub fn status_text() -> String {
    let s = state();
    if !s.installed.get() {
        return "Idle service is not running".into();
    }
    let mut base: String = if s.caffeine.get() {
        "Caffeine on · automatic idle actions paused".into()
    } else if s.media_playing.get() {
        "Media playing · automatic idle actions paused".into()
    } else if s.inhibitors.get() > 0 {
        format!("Paused by {} application inhibitor(s)", s.inhibitors.get())
    } else if s.warning.get() {
        "Idle action pending · move the pointer or press a key to cancel".into()
    } else if crate::settings::settings().with(|s| s.idle.behaviors.iter().all(|b| !b.enabled)) {
        "Automatic idle actions are off".into()
    } else {
        "Watching for inactivity".into()
    };
    if !s.sleep_delay.get() || !s.screensaver.get() {
        base.push_str(&format!(
            "\nIntegration: sleep delay {}, application inhibitors {}",
            if s.sleep_delay.get() {
                "ready"
            } else {
                "unavailable"
            },
            if s.screensaver.get() {
                "ready"
            } else {
                "unavailable"
            }
        ));
    }
    let error = s.error.get();
    if error.is_empty() {
        base
    } else {
        format!("{base}\n{error}")
    }
}

/// Manual Sleep uses the same compositor acknowledgement as automatic suspend.
pub fn request_suspend() {
    send(Message::Suspend);
}
pub fn cancel_warning() {
    send(Message::Cancel);
}

pub fn apply_config(value: &serde_json::Value) -> Result<(), String> {
    let config = IdleConfig::from_json(value)?;
    crate::settings::settings().update(|s| s.idle = config);
    Ok(())
}

pub fn install() {
    if state().installed.get_untracked() {
        return;
    }
    let runtime: Rc<RefCell<Option<Runtime<Desktop>>>> = Rc::new(RefCell::new(None));
    let target = runtime.clone();
    let sender = ui_channel(move |message| {
        if let Some(runtime) = target.borrow_mut().as_mut() {
            runtime.input(message);
        }
    });
    *runtime.borrow_mut() = Some(Runtime::new(sender.clone(), Desktop::new(sender.clone())));
    SENDER.with(|s| *s.borrow_mut() = Some(sender.clone()));
    state().installed.set(true);
    let configured = sender.clone();
    effect(move || {
        configured.send(Message::Configure(
            crate::settings::settings().with(|s| s.idle.clone()),
        ))
    });
    let locked = sender.clone();
    effect(move || {
        let _ = lock_state();
        locked.send(Message::LockChanged);
    });
    let media = anti_webui::shell::media();
    let playing = memo(move || {
        media
            .players()
            .iter()
            .any(|player| player.status == anti_webui::shell::PlaybackStatus::Playing)
    });
    let playback = sender.clone();
    effect(move || {
        let playing = playing.get();
        state().media_playing.set(playing);
        playback.send(Message::MediaPlaying(playing));
    });
    anti_webui::shell::session()
        .on_prepare_for_sleep(move |sleeping| sender.send(Message::SystemSleep(sleeping)));
    on_cleanup(move || {
        SENDER.with(|s| s.borrow_mut().take());
        state().installed.set(false);
        if let Some(runtime) = runtime.borrow_mut().as_mut() {
            runtime.cancel();
        }
    });
}

pub fn warning_view(seconds: u32) -> Div {
    let opacity = animated(0.0f32);
    opacity.to(0.94, Transition::ms(u64::from(seconds) * 1000));
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .dyn_style(move |s| s.bg(crate::theme::colors().surface).opacity(opacity.get()))
        .child(
            crate::ui::static_txt(
                crate::theme::TextStyle::TITLE_LARGE,
                "Move the pointer or press a key to stay active",
            )
            .dyn_style(|s| s.text_color(crate::theme::colors().on_surface)),
        )
}
