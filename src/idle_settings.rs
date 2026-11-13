//! An explicit-apply editor: typing or opening this page never activates an idle action.

use crate::idle_config::{IdleAction, IdleBehavior, IdleConfig};
use crate::quick_settings::{Page, go, section};
use crate::settings::settings;
use crate::theme::{TextStyle, colors};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui_md3e::components::{
    ButtonSize, ButtonStyle, TextFieldStyle, filter_chip, styled_button, switch, text_field,
};
use anti_webui_md3e::icons;

#[derive(Clone, Copy)]
struct BehaviorEditor {
    id: usize,
    name: Signal<String>,
    enabled: Signal<bool>,
    minutes: Signal<String>,
    seconds: Signal<String>,
    locked_minutes: Signal<String>,
    locked_seconds: Signal<String>,
    action: Signal<IdleAction>,
    command: Signal<String>,
    resume_command: Signal<String>,
    lock_before_suspend: Signal<bool>,
}

impl BehaviorEditor {
    fn new(id: usize, behavior: IdleBehavior) -> Self {
        Self {
            id,
            name: signal(behavior.name),
            enabled: signal(behavior.enabled),
            minutes: signal((behavior.timeout_seconds / 60).to_string()),
            seconds: signal((behavior.timeout_seconds % 60).to_string()),
            locked_minutes: signal((behavior.locked_timeout_seconds / 60).to_string()),
            locked_seconds: signal((behavior.locked_timeout_seconds % 60).to_string()),
            action: signal(behavior.action),
            command: signal(behavior.command),
            resume_command: signal(behavior.resume_command),
            lock_before_suspend: signal(behavior.lock_before_suspend),
        }
    }

    fn read(self) -> Result<IdleBehavior, String> {
        let name = self.name.get_untracked();
        Ok(IdleBehavior {
            timeout_seconds: duration(self.minutes, self.seconds)
                .map_err(|error| format!("{name:?}: idle timeout {error}"))?,
            locked_timeout_seconds: duration(self.locked_minutes, self.locked_seconds)
                .map_err(|error| format!("{name:?}: locked timeout {error}"))?,
            name,
            enabled: self.enabled.get_untracked(),
            action: self.action.get_untracked(),
            command: self.command.get_untracked(),
            resume_command: self.resume_command.get_untracked(),
            lock_before_suspend: self.lock_before_suspend.get_untracked(),
        })
    }
}

fn duration(minutes: Signal<String>, seconds: Signal<String>) -> Result<u32, &'static str> {
    let minutes = minutes.get_untracked().trim().parse::<u32>().ok();
    let seconds = seconds.get_untracked().trim().parse::<u32>().ok();
    match (minutes, seconds) {
        (Some(m), Some(s)) if m <= 1440 && s < 60 && m * 60 + s <= 86_400 => Ok(m * 60 + s),
        _ => Err("needs whole minutes and 0–59 seconds, totaling no more than 24 hours."),
    }
}

fn note(text: &'static str) -> Div {
    TextStyle::BODY_SMALL
        .apply(div().child(text))
        .dyn_style(|s| s.text_color(colors().on_surface_variant))
}

pub fn idle_page() -> Div {
    let config = settings().with_untracked(|s| s.idle.clone());
    let rows = signal(
        config
            .behaviors
            .into_iter()
            .enumerate()
            .map(|(id, behavior)| BehaviorEditor::new(id, behavior))
            .collect::<Vec<_>>(),
    );
    let next_id = signal(rows.with_untracked(Vec::len));
    let expanded = signal(None::<usize>);
    let advanced = signal(false);
    let fade = signal(config.pre_action_fade_seconds.to_string());
    let error = signal(String::new());
    let saved = signal(false);
    // Only explicit Apply writes settings. Resetting this acknowledgement never writes policy.
    effect(move || {
        fade.with(|_| ());
        for row in rows.get() {
            row.name.with(|_| ());
            row.enabled.get();
            row.minutes.with(|_| ());
            row.seconds.with(|_| ());
            row.locked_minutes.with(|_| ());
            row.locked_seconds.with(|_| ());
            row.action.get();
            row.command.with(|_| ());
            row.resume_command.with(|_| ());
            row.lock_before_suspend.get();
        }
        saved.set(false);
    });
    let apply = move || {
        let result = (|| {
            let fade = fade
                .get_untracked()
                .trim()
                .parse::<u32>()
                .map_err(|_| "Warning fade needs a whole number of seconds.".to_owned())?;
            let config = IdleConfig {
                pre_action_fade_seconds: fade,
                behaviors: rows
                    .get_untracked()
                    .into_iter()
                    .map(BehaviorEditor::read)
                    .collect::<Result<Vec<_>, _>>()?,
            };
            config.validate()?;
            Ok::<_, String>(config)
        })();
        match result {
            Ok(config) => {
                settings().update(|s| s.idle = config);
                error.set(String::new());
                saved.set(true);
            }
            Err(message) => {
                saved.set(false);
                error.set(message);
            }
        }
    };
    div()
        .flex()
        .flex_col()
        .gap(12.0_f32)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(6.0_f32)
                .child(
                    icon_btn(icons::ARROW_BACK, 38.0, |c| c.on_surface)
                        .on_click(|_| go(Page::Settings)),
                )
                .child(static_txt(TextStyle::TITLE_LARGE, "Idle & sleep").flex_1()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(8.0_f32)
                .max_h(480.0)
                .overflow_y_scroll()
                .child(note("Automatic actions pause while media is playing."))
                .child(for_each(
                    move || rows.get(),
                    |row| row.id,
                    move |row| behavior_row(row, rows, expanded),
                ))
                .child(disclosure("Advanced", advanced))
                .child(show(
                    move || advanced.get(),
                    move || {
                        div()
                            .flex()
                            .flex_col()
                            .gap(12.0_f32)
                            .child(
                                text_field(
                                    fade,
                                    TextFieldStyle::new().label("Warning (seconds, 0–60)"),
                                )
                                .w_full(),
                            )
                            .child(show(
                                move || rows.with(|rows| rows.len() < 64),
                                move || {
                                    styled_button(
                                        "Add custom action",
                                        ButtonStyle::tonal().icon(icons::ADD),
                                    )
                                    .on_click(move |_| {
                                        let id = next_id.get_untracked();
                                        next_id.set(id + 1);
                                        let name = rows.with_untracked(|rows| {
                                            let mut number = id + 1;
                                            loop {
                                                let name = format!("custom-{number}");
                                                if !rows.iter().any(|row| {
                                                    row.name.get_untracked().trim() == name
                                                }) {
                                                    break name;
                                                }
                                                number += 1;
                                            }
                                        });
                                        let row = BehaviorEditor::new(
                                            id,
                                            IdleBehavior {
                                                name,
                                                action: IdleAction::Command,
                                                ..IdleBehavior::default()
                                            },
                                        );
                                        rows.update(|rows| rows.push(row));
                                        expanded.set(Some(id));
                                    })
                                },
                            ))
                            .child(static_txt(TextStyle::TITLE_SMALL, "Service status"))
                            .child(
                                txt(TextStyle::BODY_SMALL, crate::idle::status_text)
                                    .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                            )
                    },
                )),
        )
        .child(show(
            move || !error.with(String::is_empty),
            move || {
                TextStyle::BODY_SMALL
                    .apply(div().child(text(move || error.get())))
                    .dyn_style(|s| s.text_color(colors().error))
            },
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .child(
                    txt(TextStyle::BODY_SMALL, move || {
                        if saved.get() {
                            "Applied".to_owned()
                        } else {
                            "Changes need Apply".to_owned()
                        }
                    })
                    .flex_1()
                    .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                )
                .child(styled_button("Apply", ButtonStyle::filled()).on_click(move |_| apply())),
        )
}

fn behavior_row(
    row: BehaviorEditor,
    rows: Signal<Vec<BehaviorEditor>>,
    expanded: Signal<Option<usize>>,
) -> Div {
    let advanced = signal(false);
    section()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .child(
                    txt(TextStyle::TITLE_SMALL, move || {
                        let name = row.name.get();
                        match name.as_str() {
                            "lock" | "screen-off" | "lock-and-suspend" => {
                                action_label(row.action.get()).to_owned()
                            }
                            _ => name,
                        }
                    })
                    .flex_1()
                    .min_w(0.0)
                    .overflow_hidden(),
                )
                .child(switch(row.enabled))
                .child(
                    icon_btn(icons::EDIT, 36.0, |c| c.on_surface_variant).on_click(move |_| {
                        expanded.update(|open| {
                            *open = if *open == Some(row.id) {
                                None
                            } else {
                                Some(row.id)
                            }
                        });
                    }),
                ),
        )
        .child(
            txt(TextStyle::BODY_SMALL, move || {
                let minutes = row.minutes.get();
                let seconds = row.seconds.get();
                let timing = if seconds == "0" {
                    format!("{minutes} min")
                } else {
                    format!("{minutes} min {seconds} sec")
                };
                let timing = if minutes == "0" && seconds == "0" {
                    if row.locked_minutes.get() == "0" && row.locked_seconds.get() == "0" {
                        "No timer".to_owned()
                    } else {
                        "Locked screen only".to_owned()
                    }
                } else {
                    format!("After {timing}")
                };
                match row.name.get().as_str() {
                    "lock" | "screen-off" | "lock-and-suspend" => timing,
                    _ => format!("{} · {timing}", action_label(row.action.get())),
                }
            })
            .dyn_style(|s| s.text_color(colors().on_surface_variant)),
        )
        .child(show(
            move || expanded.get() == Some(row.id),
            move || {
                div()
                    .flex()
                    .flex_col()
                    .gap(10.0_f32)
                    .child(time_fields("After", row.minutes, row.seconds))
                    .child(disclosure("More options", advanced))
                    .child(show(
                        move || advanced.get(),
                        move || {
                            let mut choices = div().flex().flex_col().gap(6.0_f32);
                            let groups: [&[IdleAction]; 2] = [
                                &[IdleAction::Lock, IdleAction::ScreenOff, IdleAction::Suspend],
                                &[IdleAction::LockAndSuspend, IdleAction::Command],
                            ];
                            for actions in groups {
                                let mut group = div().flex().flex_row().flex_wrap().gap(6.0_f32);
                                for &action in actions {
                                    group = group.child(filter_chip(
                                        action_label(action),
                                        Binding::new(
                                            move || row.action.get() == action,
                                            move |on| {
                                                if on {
                                                    row.action.set(action);
                                                }
                                            },
                                        ),
                                    ));
                                }
                                choices = choices.child(group);
                            }
                            div().flex().flex_col().gap(10.0_f32)
                    .child(
                        text_field(row.name, TextFieldStyle::new().label("Unique action name"))
                            .w_full(),
                    )
                    .child(choices)
                    .child(time_fields(
                        "Locked",
                        row.locked_minutes,
                        row.locked_seconds,
                    ))
                    .child(note("Locked time 0 uses the normal timer. Normal time 0 disables it."))
                    .child(show(
                        move || row.action.get() == IdleAction::Suspend,
                        move || {
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(
                                    static_txt(TextStyle::BODY_MEDIUM, "Lock before suspend")
                                        .flex_1(),
                                )
                                .child(switch(row.lock_before_suspend))
                        },
                    ))
                    .child(show(
                        move || row.action.get() == IdleAction::LockAndSuspend,
                        || note("Suspends only after the session is locked."),
                    ))
                    .child(show(
                        move || row.action.get() == IdleAction::Command,
                        move || {
                            div()
                                .flex()
                                .flex_col()
                                .gap(8.0_f32)
                                .child(note(
                                    "Commands run as your user. Only enable commands you trust.",
                                ))
                                .child(
                                    text_field(
                                        row.command,
                                        TextFieldStyle::new().label("Idle command").rows(2),
                                    )
                                    .w_full(),
                                )
                        },
                    ))
                    .child(
                        text_field(
                            row.resume_command,
                            TextFieldStyle::new()
                                .label("Resume command (optional)")
                                .rows(2),
                        )
                        .w_full(),
                    )
                    .child(
                        styled_button(
                            "Remove action",
                            ButtonStyle::text()
                                .size(ButtonSize::Small)
                                .icon(icons::DELETE),
                        )
                        .on_click(move |_| {
                            rows.update(|rows| rows.retain(|other| other.id != row.id));
                            expanded.set(None);
                        }),
                    )
                        },
                    ))
            },
        ))
}

fn time_fields(label: &'static str, minutes: Signal<String>, seconds: Signal<String>) -> Div {
    div()
        .flex()
        .flex_row()
        .gap(8.0_f32)
        .child(
            text_field(
                minutes,
                TextFieldStyle::new().label(format!("{label} minutes")),
            )
            .flex_1()
            .min_w(0.0),
        )
        .child(
            text_field(
                seconds,
                TextFieldStyle::new().label(format!("{label} seconds")),
            )
            .flex_1()
            .min_w(0.0),
        )
}

fn disclosure(label: &'static str, open: Signal<bool>) -> Div {
    let button = move |icon| {
        styled_button(
            label,
            ButtonStyle::text().size(ButtonSize::Small).icon(icon),
        )
        .on_click(move |_| open.update(|value| *value = !*value))
    };
    div()
        .min_h(40.0)
        .child(show(
            move || !open.get(),
            move || button(icons::EXPAND_MORE),
        ))
        .child(show(move || open.get(), move || button(icons::EXPAND_LESS)))
}

fn action_label(action: IdleAction) -> &'static str {
    match action {
        IdleAction::Lock => "Lock",
        IdleAction::ScreenOff => "Screen off",
        IdleAction::Suspend => "Suspend",
        IdleAction::LockAndSuspend => "Lock & suspend",
        IdleAction::Command => "Command",
    }
}
