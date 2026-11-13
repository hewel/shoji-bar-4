//! Being the notification daemon, when no one else is: notifications pop up at the top right as
//! cards that slide in, and stay in a list in quick settings until they are cleared. Application
//! icons stay separate from optional pictures; same-id replacements refresh both surfaces.

use crate::appinfo::{app_by_id, app_icon, icon_file};
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::*;
use anti_webui::core::images::Bitmap;
use anti_webui::prelude::*;
use anti_webui::shell::Urgency;
use anti_webui::shell::prelude::*;
use anti_webui_md3e::components::{ButtonSize, ButtonStyle, styled_button};
use anti_webui_md3e::icons;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

thread_local! {
    static SERVER: OnceCell<Option<NotificationServer>> = const { OnceCell::new() };
}

/// The notification server, if this got the name on the bus.
pub fn server() -> Option<NotificationServer> {
    SERVER.with(|s| {
        s.get_or_init(|| match notification_server() {
            Ok(server) => Some(server),
            Err(e) => {
                eprintln!("shoji-bar-4: not showing notifications: {e}");
                None
            }
        })
        .clone()
    })
}

pub(crate) const WIDTH: f32 = 352.0;
pub(crate) const POPUP_MAX_HEIGHT: f32 = 560.0;
/// At most this many pop up at once, the newest; the rest are in quick settings.
const MAX_POPUPS: usize = 3;

/// The cards, newest on top. Those below make room for a new one and close up after one that
/// goes, sliding to their places; `region` follows where the cards are.
pub fn popups(server: NotificationServer, region: Signal<InputRegion>) -> Div {
    div()
        .flex()
        .flex_col()
        .p(14.0_f32)
        .max_h(POPUP_MAX_HEIGHT)
        .overflow_y_scroll()
        .select_none()
        .on_resized(move |w, h| {
            region.set(if h > 28.5 {
                InputRegion::Rects(vec![RectI::new(0, 0, w.ceil() as i32, h.ceil() as i32)])
            } else {
                InputRegion::Empty
            })
        })
        .child(notification_cards(
            move || server.popups().into_iter().take(MAX_POPUPS).collect(),
            true,
        ))
}

/// Keyed rows for both surfaces; only their card contents change on same-id replacement.
fn notification_cards(
    list: impl Fn() -> Vec<Notification> + 'static,
    popup: bool,
) -> impl IntoView {
    let snapshot = memo(move || {
        let notifications = list();
        let ids = notifications.iter().map(|n| n.id).collect::<Vec<_>>();
        let by_id = notifications
            .into_iter()
            .map(|n| (n.id, n))
            .collect::<HashMap<_, _>>();
        (ids, by_id)
    });
    let rows = for_each(
        move || snapshot.with(|s| s.0.clone()),
        |id| *id,
        move |id| {
            let initial = untrack(|| snapshot.with(|s| s.1.get(&id).cloned()))
                .expect("a keyed notification row starts with its notification");
            let current = signal(initial);
            effect(move || {
                let next = snapshot.with(|s| {
                    s.1.get(&id).and_then(|next| {
                        current
                            .with_untracked(|old| old != next)
                            .then(|| next.clone())
                    })
                });
                if let Some(next) = next {
                    current.set(next);
                }
            });
            let content = dyn_view(move || {
                let card = card(current.get(), popup);
                if popup { card } else { card.w_full() }
            });
            let row = div().flex_none().child(content);
            if popup {
                row.mb(10.0)
            } else {
                row.w_full().mb(8.0)
            }
        },
    );
    if popup {
        rows.appear()
            .enter(|s| s.opacity(0.0_f32).translate_x(WIDTH * 0.6).scale(0.9_f32))
            .exit(|s| s.opacity(0.0_f32).translate_x(WIDTH * 0.6).scale(0.9_f32))
            .moves(emphasized(380))
            .transition(overshoot(480))
    } else {
        rows.enter(|s| s.opacity(0.0_f32).scale(0.9_f32))
            .exit(|s| s.opacity(0.0_f32).translate_x(120.0_f32))
            .moves(emphasized(320))
            .transition(emphasized(300))
    }
}

/// Resolves a local path, file URL, or theme icon without interpreting remote sources.
fn named_image_path(name: &str, size: u32) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return Some(name.into());
    }
    if let Ok(url) = url::Url::parse(name) {
        return match url.scheme() {
            "file" => url.to_file_path().ok(),
            _ => None,
        };
    }
    icon_file(name, size)
}

/// The application's icon, independently of the notification's picture.
fn icon_of(n: &Notification) -> Div {
    let mut paths = Vec::new();
    paths.extend(named_image_path(&n.app_icon, 64));
    paths.extend(
        n.desktop_entry
            .as_deref()
            .and_then(app_by_id)
            .and_then(|a| app_icon(&a, 64)),
    );
    let candidate = signal(0usize);
    div()
        .size(36.0)
        .flex_none()
        .rounded(12.0_f32)
        .overflow_hidden()
        .child(dyn_view(move || {
            let current = candidate.get();
            if let Some(path) = paths.get(current) {
                div().size_full().child(
                    img(path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .on_error(move |_| candidate.update(|i| *i += 1)),
                )
            } else {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .dyn_style(|s| s.bg(colors().secondary_container))
                    .child(sym(icons::NOTIFICATIONS, 18.0, |c| {
                        c.on_secondary_container
                    }))
            }
        }))
}

/// The notification's own picture, hidden until an in-memory or file image is available.
fn picture_of(n: &Notification) -> Div {
    let picture: Signal<Option<Arc<Bitmap>>> = signal(n.image.clone());
    if picture.get_untracked().is_none()
        && let Some(path) = n
            .image_path
            .as_deref()
            .and_then(|name| named_image_path(name, 648))
    {
        // This task belongs to the card; replacement disposes its callback, even for the same path.
        task(move || {
            anti_webui::core::images::decode_bitmap(path, (648, 288))
                .ok()
                .map(Arc::new)
        })
        .then(move |decoded| picture.set(decoded));
    }
    let area = signal((0.0_f32, 0.0_f32));
    div()
        .w_full()
        .h(144.0)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(14.0_f32)
        .overflow_hidden()
        .on_resized(move |w, h| area.set((w, h)))
        .dyn_style(move |s| {
            let s = s.bg(colors().surface_container);
            if picture.get().is_some() {
                s
            } else {
                s.hidden()
            }
        })
        .child(
            bitmap_view(move || picture.get())
                .size_full()
                .object_fit(ObjectFit::Contain)
                // BitmapView's fallback has no natural size upstream, so fit its box explicitly.
                .dyn_style(move |s| {
                    let Some(bitmap) = picture.get() else {
                        return s.hidden();
                    };
                    let (width, height) = area.get();
                    if width <= 0.0 || height <= 0.0 {
                        return s.hidden();
                    }
                    let scale =
                        (width / bitmap.width() as f32).min(height / bitmap.height() as f32);
                    s.w(bitmap.width() as f32 * scale)
                        .h(bitmap.height() as f32 * scale)
                        .flex_none()
                }),
        )
}

/// A notification as a card: its icon, who sent it, what it says, its actions and a close button.
pub fn card(n: Notification, popup: bool) -> Div {
    let urgent = n.urgency == Urgency::Critical;
    let when = n.received();
    let app = n.app_name.clone();
    let gone = n.clone();
    let default = n.clone();
    let has_default = n.actions.iter().any(|(k, _)| k == "default");
    let mut actions = div().flex().flex_row().gap(8.0_f32).pt(4.0_f32);
    let mut any_action = false;
    for (key, label) in n.actions.clone() {
        if key == "default" {
            continue;
        }
        any_action = true;
        let n = n.clone();
        actions = actions.child(
            styled_button(label, ButtonStyle::tonal().size(ButtonSize::ExtraSmall)).on_click(
                move |_| {
                    n.invoke(&key);
                },
            ),
        );
    }
    let hovered = signal(false);
    let mut body = div()
        .w(WIDTH)
        .flex()
        .flex_col()
        .gap(6.0_f32)
        .p(14.0_f32)
        .rounded(20.0_f32)
        .overflow_hidden()
        .shadows(if popup { elevation(3) } else { Vec::new() })
        .on_hover(move |h| hovered.set(h))
        .on_click(move |_| {
            if has_default {
                default.invoke("default");
            }
        })
        .dyn_style(move |s| {
            let c = colors();
            if urgent {
                s.bg(c.error_container).text_color(c.on_error_container)
            } else if popup {
                s.bg(c.surface_container_high).text_color(c.on_surface)
            } else {
                s.bg(c.surface_container_highest).text_color(c.on_surface)
            }
        })
        .child(
            div()
                .flex()
                .flex_row()
                .gap(12.0_f32)
                .child(icon_of(&n))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(0.0)
                        .gap(2.0_f32)
                        .child(
                            txt(TextStyle::LABEL_SMALL, move || {
                                let ago = when.elapsed().as_secs();
                                let ago = match ago {
                                    0..60 => "now".to_owned(),
                                    60..3600 => format!("{} min", ago / 60),
                                    _ => format!("{} h", ago / 3600),
                                };
                                local_time(Every::Minute);
                                if app.is_empty() {
                                    ago
                                } else {
                                    format!("{app} · {ago}")
                                }
                            })
                            .dyn_style(move |s| s.opacity(0.75_f32)),
                        )
                        .child(
                            static_txt(TextStyle::TITLE_SMALL, n.summary.clone()).overflow_hidden(),
                        )
                        .child(
                            TextStyle::BODY_SMALL
                                .apply(div())
                                .max_h(48.0)
                                .overflow_hidden()
                                .dyn_style(move |s| s.opacity(0.85_f32))
                                .child(n.body_text()),
                        ),
                )
                .child(
                    icon_btn(icons::CLOSE, 28.0, |c| c.on_surface_variant)
                        .on_click(move |e| {
                            e.stop_propagation();
                            gone.dismiss();
                        })
                        .dyn_style(move |s| {
                            s.opacity(if hovered.get() || !popup {
                                1.0
                            } else {
                                0.0_f32
                            })
                        })
                        .transition_of([Prop::Opacity], Transition::ms(150).ease_out()),
                ),
        );
    body = body.child(picture_of(&n));
    if any_action {
        body = body.child(actions);
    }
    pressable(body, |c| c.on_surface)
}

/// The notifications that are kept, for quick settings, with a button that clears them; with none,
/// it says so. They come in as the `order`th part of the panel.
pub fn history(order: usize) -> Div {
    let server = server();
    let any = {
        let server = server.clone();
        memo(move || {
            server
                .as_ref()
                .is_some_and(|s| !s.notifications().is_empty())
        })
    };
    let clear = server.clone();
    let list = server.clone();
    let elsewhere = server.is_none();
    settle_in(order, div())
        .flex()
        .flex_col()
        .gap(8.0_f32)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .h(28.0)
                .px(6.0_f32)
                .child(sym(icons::NOTIFICATIONS, 18.0, |c| c.primary))
                .child(static_txt(TextStyle::TITLE_SMALL, "Notifications").flex_1())
                .child(
                    show(
                        move || any.get(),
                        move || {
                            let clear = clear.clone();
                            styled_button(
                                "Clear all",
                                ButtonStyle::text().size(ButtonSize::ExtraSmall),
                            )
                            .on_click(move |_| {
                                if let Some(s) = &clear {
                                    s.clear();
                                }
                            })
                        },
                    )
                    .enter(|s| s.opacity(0.0_f32).scale(0.8_f32))
                    .exit(|s| s.opacity(0.0_f32).scale(0.8_f32))
                    .transition(overshoot(320)),
                ),
        )
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                // About three cards, with a little of the next one showing that there is more to
                // scroll to.
                .max_h(268.0)
                .overflow_y_scroll()
                .child(notification_cards(
                    move || list.as_ref().map(|s| s.notifications()).unwrap_or_default(),
                    false,
                )),
        )
        .child(
            show(
                move || !any.get(),
                move || {
                    let (symbol, said) = if elsewhere {
                        (
                            icons::NOTIFICATIONS_PAUSED,
                            "Another app shows notifications",
                        )
                    } else {
                        (icons::NOTIFICATIONS_OFF, "No notifications")
                    };
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(8.0_f32)
                        .h(64.0)
                        .rounded(22.0_f32)
                        .dyn_style(|s| s.bg(colors().surface_container_high))
                        .child(sym(symbol, 20.0, |c| c.outline))
                        .child(
                            static_txt(TextStyle::BODY_MEDIUM, said)
                                .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                        )
                },
            )
            .enter(|s| s.opacity(0.0_f32).scale(0.95_f32))
            .transition(emphasized(300)),
        )
}

#[cfg(test)]
mod tests;
