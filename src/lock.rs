//! The lock screen, on every output (`ext_session_lock_v1`): the wallpaper under a veil, a big
//! clock, the user's picture and a field of shapes for the password, checked with PAM. A wrong
//! password shakes the field; the right one sends everything away and unlocks.
//!
//! It locks on `shoji-bar-4 msg lock`, when logind asks (`loginctl lock-session`, the power menu's
//! Lock) and before the computer sleeps. Nothing from outside can unlock it. If the shell dies while
//! locked, ShojiWM keeps the session locked, and a new shell can lock it again to take over.

use crate::pam::{Refused, authenticate};
use crate::settings::settings;
use crate::theme::{TextStyle, colors};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui::shell::{LockState, SessionLock, lock_session};
use anti_webui_md3e::components::{MaterialShape, ShapeStyle, loading_indicator, styled_shape};
use anti_webui_md3e::icons;
use anti_webui_md3e::motion::fast_spatial;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

/// How long everything takes to go after the right password, before the lock goes.
const LEAVING: Duration = Duration::from_millis(480);
/// How many shapes the field shows at most, however long the password.
const SHOWN_DOTS: usize = 18;
const MAX_LENGTH: usize = 1024;
/// The shapes the password is shown as, one after another.
const DOTS: [MaterialShape; 6] = [
    MaterialShape::Cookie4Sided,
    MaterialShape::Clover4Leaf,
    MaterialShape::Sunny,
    MaterialShape::Circle,
    MaterialShape::Cookie6Sided,
    MaterialShape::Flower,
];
/// Text on the veiled wallpaper.
const ON_VEIL: u32 = 0xffffff;
const ERROR: u32 = 0xffb4ab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Typing,
    Checking,
    Leaving,
}

/// What every output's lock screen shows, shared.
#[derive(Clone, Copy)]
struct State {
    typed: Signal<String>,
    phase: Signal<Phase>,
    said: Signal<String>,
    /// How far the field is pushed aside, for the shake.
    shake: Animated<f32>,
}

/// Checks a password; PAM, or something else for pictures of it.
type Check = fn(&str, String) -> Result<(), Refused>;

thread_local! {
    static LOCK: RefCell<Option<(SessionLock, Owner)>> = const { RefCell::new(None) };
    static CHECK: Cell<Check> = const { Cell::new(authenticate) };
    /// Where PAM's answers come. Made once, by `install`, outside every window: one made while a
    /// panel's button is handled would answer into the panel, which is gone by then.
    static ANSWER: RefCell<Option<UiSender<(u64, Result<(), Refused>)>>> = const { RefCell::new(None) };
    /// What the lock screens of the lock there is show.
    static CURRENT: Cell<Option<State>> = const { Cell::new(None) };
    static SLEEP_HOLD: RefCell<Weak<()>> = const { RefCell::new(Weak::new()) };
    static AUTH_GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Whether it is locked, or locking.
pub fn is_locked() -> bool {
    LOCK.with(|l| l.borrow().is_some())
}

/// Locks the session, if it isn't.
pub fn lock() {
    if is_locked() {
        return;
    }
    crate::panels::close();
    let (state, owner) = create_root(State::new);
    begin(state);
    let session = lock_session(move |output| screen(output, state));
    LOCK.with(|l| *l.borrow_mut() = Some((session.clone(), owner)));
    // The compositor won't lock, as when something else has: nothing to keep.
    owner.with(|| {
        effect(move || {
            if session.state() == LockState::Refused {
                eprintln!("shoji-bar-4: the compositor wouldn't lock the session");
                set_timeout(Duration::ZERO, finish_refused);
            }
        })
    });
}

/// Keeps the lock intact until the last sleep hold is dropped, even if locking starts later.
/// Releasing a hold never unlocks: a fresh successful authentication is required afterwards.
#[must_use = "dropping the hold permits authentication again"]
pub struct SleepHold(Rc<()>);

impl SleepHold {
    /// Invalidates pending authentication and unlock animations, protecting the current or next lock.
    pub fn acquire() -> Self {
        let hold = SLEEP_HOLD.with(|current| {
            let mut current = current.borrow_mut();
            let hold = current.upgrade().unwrap_or_else(|| Rc::new(()));
            *current = Rc::downgrade(&hold);
            Self(hold)
        });
        next_authentication();
        if let Some(state) = CURRENT.with(Cell::get) {
            state.wait_for_sleep();
        }
        hold
    }
}

impl Drop for SleepHold {
    fn drop(&mut self) {
        if Rc::strong_count(&self.0) == 1
            && let Some(state) = CURRENT.with(Cell::get)
        {
            state.said.set(String::new());
        }
    }
}

fn sleep_held() -> bool {
    SLEEP_HOLD.with(|hold| hold.borrow().strong_count() != 0)
}

fn next_authentication() -> u64 {
    AUTH_GENERATION.with(|generation| {
        let next = generation.get().wrapping_add(1);
        generation.set(next);
        next
    })
}

fn authentication_current(generation: u64) -> bool {
    generation == AUTH_GENERATION.with(Cell::get) && !sleep_held()
}

fn begin(state: State) {
    next_authentication();
    CURRENT.with(|current| current.set(Some(state)));
    if sleep_held() {
        state.wait_for_sleep();
    }
}

/// Unlocks only for the authentication that scheduled this finish.
fn finish(generation: u64) {
    if authentication_current(generation) {
        clear_lock();
    }
}

/// A refused request has no acknowledged lock to protect, even while sleep holds remain alive.
fn finish_refused() {
    if LOCK.with(|lock| {
        lock.borrow()
            .as_ref()
            .is_some_and(|(session, _)| session.state() == LockState::Refused)
    }) {
        clear_lock();
    }
}

fn clear_lock() {
    next_authentication();
    CURRENT.with(|current| current.set(None));
    if let Some((session, owner)) = LOCK.with(|lock| lock.borrow_mut().take()) {
        session.unlock();
        owner.dispose();
    }
}

/// Makes where PAM's answers come, handing each to the lock there is.
fn answers() {
    let to_ui = ui_channel(|(generation, result): (u64, Result<(), Refused>)| {
        if authentication_current(generation)
            && let Some(state) = CURRENT.with(Cell::get)
        {
            state.answered(result)
        }
    });
    ANSWER.with(|a| *a.borrow_mut() = Some(to_ui));
}

/// Installs authentication and explicit logind lock requests. Idle owns sleep coordination.
pub fn install() {
    answers();
    session().on_lock(lock);
}

/// A lock screen as it is when locked, for pictures of it (`preview lock`). It takes `shoji`
/// instead of asking PAM, for the rest of the process: never call it in the shell.
pub fn view(output: Output) -> Div {
    CHECK.with(|c| {
        c.set(|_, typed| {
            if typed == "shoji" {
                Ok(())
            } else {
                // As PAM does, a moment before saying no.
                std::thread::sleep(Duration::from_millis(900));
                Err(Refused::WrongPassword)
            }
        })
    });
    let state = State::new();
    begin(state);
    answers();
    screen(output, state)
}

impl State {
    fn new() -> Self {
        Self {
            typed: signal(String::new()),
            phase: signal(Phase::Typing),
            said: signal(String::new()),
            shake: animated(0.0f32),
        }
    }

    fn wait_for_sleep(self) {
        self.typed.set(String::new());
        self.phase.set(Phase::Typing);
        self.said.set("Waiting for system sleep".into());
    }

    fn key(self, e: &KeyboardEvent) {
        if self.phase.get_untracked() != Phase::Typing {
            return;
        }
        match &e.key {
            Key::Named(NamedKey::Enter) => self.submit(),
            Key::Named(NamedKey::Escape) => self.typed.set(String::new()),
            Key::Named(NamedKey::Backspace) if e.modifiers.ctrl => self.typed.set(String::new()),
            Key::Named(NamedKey::Backspace) => self.typed.update(|t| _ = t.pop()),
            Key::Character(c) if e.modifiers.ctrl && c.eq_ignore_ascii_case("u") => {
                self.typed.set(String::new())
            }
            _ => {
                if let Some(text) = e.text.as_deref()
                    && !e.modifiers.ctrl
                    && !e.modifiers.logo
                    && !text.is_empty()
                {
                    self.said.set(String::new());
                    self.typed.update(|t| {
                        if t.len() + text.len() <= MAX_LENGTH {
                            t.push_str(text)
                        }
                    });
                }
            }
        }
    }

    fn submit(self) {
        if sleep_held() {
            self.wait_for_sleep();
            return;
        }
        if self.typed.with_untracked(String::is_empty) {
            return;
        }
        let Some(to_ui) = ANSWER.with(|a| a.borrow().clone()) else {
            return;
        };
        self.phase.set(Phase::Checking);
        self.said.set(String::new());
        let mut password = String::new();
        self.typed.update(|t| password = std::mem::take(t));
        let check = CHECK.with(Cell::get);
        let user = crate::user::name();
        let generation = next_authentication();
        let _ = std::thread::Builder::new()
            .name("shoji-bar-4 pam".into())
            .spawn(move || to_ui.send((generation, check(&user, password))));
    }

    fn answered(self, result: Result<(), Refused>) {
        match result {
            Ok(()) => {
                self.phase.set(Phase::Leaving);
                let generation = AUTH_GENERATION.with(Cell::get);
                set_timeout(LEAVING, move || finish(generation));
            }
            Err(refused) => {
                self.phase.set(Phase::Typing);
                self.said.set(match refused {
                    Refused::WrongPassword => "Wrong password".into(),
                    Refused::Told(why) | Refused::Other(why) => why,
                });
                // Pushed aside and let go on a loose spring, it shakes.
                self.shake.set(28.0);
                self.shake.spring_to(0.0, Spring::from_ratio(900.0, 0.12));
            }
        }
    }
}

fn screen(output: Output, state: State) -> Div {
    let _ = output;
    let focus = node_ref();
    request_frame(move |_| focus.focus());
    let leaving = follow(
        move || {
            if state.phase.get() == Phase::Leaving {
                1.0f32
            } else {
                0.0
            }
        },
        fast_spatial(),
    );
    let typing = follow(
        move || {
            if state.typed.with(String::is_empty) {
                0.0f32
            } else {
                1.0
            }
        },
        fast_spatial(),
    );
    let wallpaper = memo(|| settings().with(|s| s.wallpaper.clone()));
    div()
        .node_ref(focus)
        .focusable()
        .size_full()
        .relative()
        .overflow_hidden()
        .select_none()
        .on_key_down(move |e| state.key(e))
        .on_click(move |_| focus.focus())
        .dyn_style(|s| s.bg(colors().surface_dim))
        .child(
            for_each(
                move || wallpaper.get().into_iter().collect::<Vec<_>>(),
                Clone::clone,
                |path| {
                    img(path)
                        .absolute()
                        .inset(0.0)
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                },
            )
            .appear()
            .enter(|s| s.scale(1.08_f32))
            .transition(Transition::ms(1100).cubic_bezier(0.2, 0.0, 0.0, 1.0)),
        )
        // The veil, deeper while a password is typed, lifting as it unlocks.
        .child(div().absolute().inset(0.0).dyn_style(move |s| {
            let away = leaving.get().clamp(0.0, 1.0);
            s.bg(rgb(0x000000).with_alpha((0.3 + 0.18 * typing.get()) * (1.0 - away)))
        }))
        .child(
            div()
                .absolute()
                .inset(0.0)
                .flex()
                .flex_col()
                .items_center()
                .text_color(rgb(ON_VEIL))
                .dyn_style(move |s| {
                    let t = leaving.get();
                    s.opacity((1.0 - t).clamp(0.0, 1.0)).scale(1.0 + 0.08 * t)
                })
                .child(div().flex_grow(2.0_f32))
                .child(settle_in(0, clock()))
                .child(settle_in(1, date()))
                .child(div().flex_grow(3.0_f32))
                .child(settle_in(2, crate::user::avatar(104.0)))
                .child(settle_in(
                    3,
                    static_txt(TextStyle::TITLE_LARGE, crate::user::name()).mt(14.0),
                ))
                .child(settle_in(4, field(state)))
                .child(message(state))
                .child(div().flex_grow(1.6_f32)),
        )
        .child(
            settle_in(5, status())
                .dyn_style(move |s| s.opacity((1.0 - leaving.get()).clamp(0.0, 1.0))),
        )
}

/// The time, its digits rolling over, large and light.
fn clock() -> Div {
    let size = 112.0;
    let style = TextStyle {
        size,
        line_height: 124.0,
        letter_spacing: 0.0,
        weight: 300,
    };
    let time = move || {
        let s = settings().get();
        local_time(Every::Minute).format(if s.clock_24h { "%H:%M" } else { "%-I:%M" })
    };
    rolling_text(time, style, move |c| match c {
        ':' => size * 0.3,
        _ => size * 0.58,
    })
    .font_axis(b"wght", 300.0)
}

fn date() -> Div {
    txt(TextStyle::TITLE_LARGE, || {
        local_time(Every::Minute).format("%A, %-d %B")
    })
    .opacity(0.85_f32)
}

/// The password, as shapes that pop in as it is typed, on a glassy pill; a turning shape while it
/// is checked. It shakes when the password is wrong.
fn field(state: State) -> Div {
    let checking = memo(move || state.phase.get() == Phase::Checking);
    let failed = memo(move || !state.said.with(String::is_empty));
    let count = memo(move || state.typed.with(|t| t.chars().count()));
    div()
        .mt(22.0)
        .w(320.0)
        .h(56.0)
        .relative()
        .rounded(28.0_f32)
        .border(1.5_f32)
        .flex()
        .items_center()
        .justify_center()
        .transition_of(
            [Prop::BorderColor, Prop::Background],
            Transition::ms(250).ease_out(),
        )
        .dyn_style(move |s| {
            s.translate_x(state.shake.get())
                .bg(rgb(0xffffff).with_alpha(if count.get() > 0 { 0.2 } else { 0.14 }))
                .border_color(if failed.get() {
                    rgb(ERROR)
                } else {
                    rgb(0xffffff).with_alpha(0.28)
                })
        })
        // What to do, while there is nothing typed.
        .child(
            show(
                move || count.get() == 0 && !checking.get(),
                || {
                    div()
                        .absolute()
                        .inset(0.0)
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.7_f32)
                        .child(static_txt(TextStyle::BODY_LARGE, "Enter password"))
                },
            )
            .appear()
            .enter(|s| s.opacity(0.0_f32))
            .exit(|s| s.opacity(0.0_f32))
            .transition(Transition::ms(180).ease_out()),
        )
        .child(
            div().flex().flex_row().items_center().child(
                for_each(
                    move || (0..count.get().min(SHOWN_DOTS)).collect::<Vec<_>>(),
                    |i| *i,
                    dot,
                )
                // The place opens and closes, so that the others slide over rather than jump.
                .enter(|s| s.w(0.0).opacity(0.0_f32))
                .exit(|s| s.w(0.0).opacity(0.0_f32).scale(0.3_f32))
                .transition(emphasized(300)),
            ),
        )
        .child(
            show(
                move || checking.get(),
                || {
                    div()
                        .absolute()
                        .inset(0.0)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(loading_indicator().scale(0.85_f32))
                },
            )
            .enter(|s| s.opacity(0.0_f32).scale(0.4_f32))
            .exit(|s| s.opacity(0.0_f32).scale(0.4_f32))
            .transition(overshoot(360)),
        )
}

/// How wide the place of a shape of the password is, the room between them included.
const DOT_PLACE: f32 = 22.0;

/// A shape of the password: it springs up from small, turning into place.
fn dot(i: usize) -> Div {
    let pop = animated(0.0f32);
    pop.spring_to(1.0, Spring::from_ratio(520.0, 0.55));
    let rest = (i * 37 % 90) as f32;
    div()
        .w(DOT_PLACE)
        .h(16.0)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            styled_shape(
                move || DOTS[i % DOTS.len()],
                ShapeStyle::new()
                    .color(|_| rgb(ON_VEIL))
                    .rotation(move || rest - 90.0 * (1.0 - pop.get())),
            )
            .size(16.0)
            .dyn_style(move |s| s.scale(pop.get().max(0.0))),
        )
}

/// Why it didn't unlock, under the field.
fn message(state: State) -> Div {
    div().h(28.0).mt(10.0).flex().items_center().child(
        show(
            move || !state.said.with(String::is_empty),
            move || {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(6.0_f32)
                    .text_color(rgb(ERROR))
                    .child(anti_webui_md3e::components::sized_icon(icons::ERROR, 18.0))
                    .child(txt(TextStyle::LABEL_LARGE, move || state.said.get()))
            },
        )
        .enter(|s| s.opacity(0.0_f32).translate_y(-8.0_f32))
        .exit(|s| s.opacity(0.0_f32))
        .transition(emphasized(300)),
    )
}

/// The battery, at the top right, when there is one.
fn status() -> Div {
    let b = battery();
    div()
        .absolute()
        .top(20.0)
        .right(24.0)
        .text_color(rgb(ON_VEIL))
        .child(show(
            move || b.available(),
            move || {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(4.0_f32)
                    .opacity(0.9_f32)
                    .child(dyn_sym(
                        move || crate::bar::battery_icon(b.percentage(), b.state()),
                        18.0,
                        |_| rgb(ON_VEIL),
                    ))
                    .child(txt(TextStyle::LABEL_LARGE, move || {
                        format!("{:.0}%", b.percentage())
                    }))
            },
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real lock lifecycle and PAM answer channel without opening compositor surfaces.
    struct LockScreen {
        state: State,
        owner: Owner,
    }

    impl LockScreen {
        fn new() -> Self {
            let (state, owner) = create_root(|| {
                answers();
                State::new()
            });
            begin(state);
            Self { state, owner }
        }

        fn checking(&self) -> u64 {
            self.state.phase.set(Phase::Checking);
            next_authentication()
        }

        fn answer(&self, generation: u64) {
            let answer = ANSWER.with(|answer| answer.borrow().as_ref().unwrap().clone());
            answer.send((generation, Ok(())));
            anti_webui::core::channel::pump();
        }

        fn assert_waiting(&self) {
            self.state.typed.set("must not reach PAM".into());
            self.state.submit();
            assert!(self.state.typed.get_untracked().is_empty());
            assert_eq!(self.state.phase.get_untracked(), Phase::Typing);
            assert_eq!(self.state.said.get_untracked(), "Waiting for system sleep");
            assert!(CURRENT.with(Cell::get).is_some());
        }
    }

    impl Drop for LockScreen {
        fn drop(&mut self) {
            clear_lock();
            ANSWER.with(|answer| answer.borrow_mut().take());
            self.owner.dispose();
        }
    }

    #[test]
    fn duplicate_holds_require_the_last_release_and_fresh_authentication() {
        let screen = LockScreen::new();
        let first = SleepHold::acquire();
        let second = SleepHold::acquire();
        drop(first);
        screen.assert_waiting();

        drop(second);
        assert!(CURRENT.with(Cell::get).is_some());
        assert_eq!(screen.state.phase.get_untracked(), Phase::Typing);
        assert!(screen.state.said.get_untracked().is_empty());

        let generation = screen.checking();
        screen.answer(generation);
        assert_eq!(screen.state.phase.get_untracked(), Phase::Leaving);
        finish(generation);
        assert!(CURRENT.with(Cell::get).is_none());
    }

    #[test]
    fn pre_acquire_pam_answers_stay_invalid_after_release() {
        let screen = LockScreen::new();
        let generation = screen.checking();
        let hold = SleepHold::acquire();
        screen.answer(generation);
        screen.assert_waiting();
        drop(hold);

        screen.answer(generation);
        assert_eq!(screen.state.phase.get_untracked(), Phase::Typing);
        assert!(CURRENT.with(Cell::get).is_some());
    }

    #[test]
    fn pre_acquire_finish_stays_invalid_after_release() {
        let screen = LockScreen::new();
        let generation = screen.checking();
        screen.answer(generation);
        assert_eq!(screen.state.phase.get_untracked(), Phase::Leaving);

        let hold = SleepHold::acquire();
        finish(generation);
        screen.assert_waiting();
        drop(hold);
        finish(generation);
        assert!(CURRENT.with(Cell::get).is_some());
        assert_eq!(screen.state.phase.get_untracked(), Phase::Typing);
    }

    #[test]
    fn a_hold_acquired_before_lock_creation_protects_the_new_lock() {
        let hold = SleepHold::acquire();
        let screen = LockScreen::new();
        screen.assert_waiting();
        drop(hold);
        assert!(CURRENT.with(Cell::get).is_some());
        assert_eq!(screen.state.phase.get_untracked(), Phase::Typing);
    }

    #[test]
    fn holds_survive_refused_cleanup_without_releasing_later_holds() {
        let screen = LockScreen::new();
        let old_generation = screen.checking();
        let old_hold = SleepHold::acquire();
        // This is the same cleanup used when the compositor refuses the lock.
        clear_lock();
        begin(screen.state);
        let new_hold = SleepHold::acquire();
        drop(old_hold);
        screen.assert_waiting();
        drop(new_hold);

        screen.answer(old_generation);
        finish(old_generation);
        assert!(CURRENT.with(Cell::get).is_some());
        assert_eq!(screen.state.phase.get_untracked(), Phase::Typing);

        let generation = screen.checking();
        screen.answer(generation);
        finish(generation);
        assert!(CURRENT.with(Cell::get).is_none());
    }
}
