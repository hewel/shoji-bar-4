//! Matugen is the dynamic palette authority. A single worker generates colors and
//! updates opted-in applications; the UI keeps its last valid palette on failure.

pub(crate) mod integrations;
pub(crate) mod matugen;
mod runtime;

use crate::settings::{ThemeMode, settings};
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui_md3e::motion::default_effects;
use anti_webui_md3e::{ColorScheme, set_color_scheme, set_color_scheme_animated};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

pub use anti_webui_md3e::color_scheme as colors;
pub use anti_webui_md3e::typography::TextStyle;

/// The seeds offered in quick settings, with what they are called.
pub const SEEDS: [(u32, &str); 8] = [
    (0x6750a4, "Lilac"),
    (0x0061a4, "Ocean"),
    (0x006a6a, "Lagoon"),
    (0x3a6a1f, "Forest"),
    (0x7c5800, "Amber"),
    (0xa23f00, "Ember"),
    (0xb0255c, "Rose"),
    (0x5d5f73, "Slate"),
];

#[derive(Clone, Debug)]
pub struct Status {
    pub busy: bool,
    pub message: String,
    pub error: Option<String>,
    pub gtk: Result<String, String>,
    pub zen: Result<String, String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            busy: false,
            message: "Matugen has not started".into(),
            error: None,
            gtk: Ok("Not linked".into()),
            zen: Ok("Not linked".into()),
        }
    }
}

#[derive(Clone, Copy)]
pub struct State {
    pub status: Signal<Status>,
    /// The committed target, rather than an in-between animation frame.
    pub applied: Signal<(ColorScheme, bool)>,
    wallpaper: Signal<Option<(PathBuf, u32)>>,
    retry: Signal<u64>,
}

thread_local! {
    static STATE: State = create_root(|| State {
        status: signal(Status::default()),
        applied: signal((ColorScheme::dark(), true)),
        wallpaper: signal(None),
        retry: signal(0),
    }).0;
    static WORKER: RefCell<Option<runtime::Worker>> = const { RefCell::new(None) };
}

pub fn state() -> State {
    STATE.with(|state| *state)
}

/// The effective mode stays with the last valid palette if generation fails.
pub fn is_dark() -> bool {
    state().applied.with(|(_, dark)| *dark)
}

pub fn wallpaper_seed() -> Option<u32> {
    let path = settings().with(|s| s.wallpaper.clone());
    state().wallpaper.with(|seed| {
        seed.as_ref()
            .filter(|(source, _)| Some(source) == path.as_ref())
            .map(|(_, color)| *color)
    })
}

pub fn retry() {
    state()
        .retry
        .update(|revision| *revision = revision.wrapping_add(1));
}

/// Previews use the same generator, but never persist or link external themes.
pub fn install(persistent: bool) {
    if WORKER.with(|worker| worker.borrow().is_some()) {
        return;
    }
    let state = state();
    let requested_dark = memo(move || match settings().with(|s| s.mode) {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::Auto => !(7..19).contains(&local_time(Every::Minute).hour),
    });
    let paths = integrations::Paths::from_environment();
    let cached = paths.as_ref().map_err(Clone::clone).and_then(|paths| {
        matugen::load_cache(&paths.state.join("shoji-bar-4/theme/last-good.json"))
    });
    let cache_error = cached.as_ref().err().cloned();
    let cached = cached.ok().flatten().map(Arc::new);
    let initial = cached
        .as_ref()
        .and_then(|theme| theme.scheme().ok().map(|scheme| (scheme, theme.dark)))
        .unwrap_or_else(|| {
            let dark = requested_dark.get();
            (
                if dark {
                    ColorScheme::dark()
                } else {
                    ColorScheme::light()
                },
                dark,
            )
        });
    state.applied.set(initial);
    set_color_scheme(initial.0);
    state.status.set(Status {
        busy: true,
        message: if cached.is_some() {
            "Using saved colors · generating with Matugen"
        } else {
            "Using startup colors · generating with Matugen"
        }
        .into(),
        error: cache_error,
        ..Status::default()
    });
    let worker = runtime::Worker::start(paths, cached, persistent, move |outcome| {
        if let Some(theme) = &outcome.theme {
            // Both generated and cached palettes have already passed this validation.
            if let Ok(scheme) = theme.scheme() {
                let next = (scheme, theme.dark);
                if state.applied.get_untracked() != next {
                    state.applied.set(next);
                    set_color_scheme_animated(scheme, default_effects());
                }
            }
            if let Some(matugen::Request {
                source: matugen::Source::Image(path),
                ..
            }) = &outcome.request
            {
                state
                    .wallpaper
                    .set(Some((path.clone(), theme.source_color)));
            }
        }
        state.status.set(Status {
            busy: outcome.busy,
            message: if outcome.busy {
                "Updating application themes"
            } else if outcome.error.is_some() {
                "Theme update needs attention"
            } else {
                "Matugen colors applied"
            }
            .into(),
            error: outcome.error,
            gtk: outcome.integrations.gtk,
            zen: outcome.integrations.zen,
        });
    });
    match worker {
        Ok(worker) => WORKER.with(|slot| *slot.borrow_mut() = Some(worker)),
        Err(error) => {
            state.status.update(|status| {
                status.busy = false;
                status.error = Some(error);
            });
            return;
        }
    }
    let input = memo(move || {
        settings().with(|s| runtime::Input {
            request: matugen::Request {
                source: if s.seed_from_wallpaper {
                    s.wallpaper
                        .clone()
                        .map(matugen::Source::Image)
                        .unwrap_or(matugen::Source::Color(s.seed))
                } else {
                    matugen::Source::Color(s.seed)
                },
                palette: s.palette,
                dark: requested_dark.get(),
            },
            gtk: s.gtk_theme_enabled,
            zen: s.zen_theme_enabled,
            retry: state.retry.get(),
        })
    });
    effect(move || {
        let input = input.get();
        state.status.update(|status| {
            status.busy = true;
            status.message = "Updating theme…".into();
        });
        WORKER.with(|worker| {
            if let Some(worker) = worker.borrow().as_ref() {
                worker.configure(input);
            }
        });
    });
    on_cleanup(|| {
        WORKER.with(|worker| worker.borrow_mut().take());
    });
}

/// The shadows of an elevation level of Material 3 (0 to 5): a key light and an ambient one.
pub fn elevation(level: u8) -> Vec<BoxShadow> {
    let (key, ambient) = match level {
        0 => return Vec::new(),
        1 => ((1.0, 2.0, 0.0), (1.0, 3.0, 1.0)),
        2 => ((1.0, 2.0, 0.0), (2.0, 6.0, 2.0)),
        3 => ((1.0, 3.0, 0.0), (4.0, 8.0, 3.0)),
        4 => ((2.0, 3.0, 0.0), (6.0, 10.0, 4.0)),
        _ => ((4.0, 4.0, 0.0), (8.0, 12.0, 6.0)),
    };
    let shadow = |(y, blur, spread): (f32, f32, f32), alpha: f32| BoxShadow {
        offset_x: 0.0,
        offset_y: y,
        blur,
        spread,
        color: rgba(0x00000000).with_alpha(alpha),
        inset: false,
    };
    vec![shadow(key, 0.30), shadow(ambient, 0.15)]
}
/// A color as `0xRRGGBB`.
pub fn hex_of(c: Color) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c.r) << 16) | (byte(c.g) << 8) | byte(c.b)
}
