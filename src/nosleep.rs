//! Bar indicator and anchored status panel for manual `nosleep` and automatic Herdr agent protection.

use crate::ai_usage::tr;
use crate::nosleep_service::{self, Snapshot};
use crate::panels::{Panel, is_open, toggle};
use crate::placement::{AnchorPoint, BarPlacement};
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::{pressable, static_txt, sym};
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui_md3e::components::{ButtonSize, IconButtonStyle, styled_icon_button, switch};
use anti_webui_md3e::icons;

const PANEL_WIDTH: f32 = 348.0;
const EDGE: f32 = 14.0;

fn has_warning(snap: &Snapshot) -> bool {
    snap.error.is_some() || (snap.watcher_loaded && !snap.watcher_active)
}

fn has_badge(snap: &Snapshot) -> bool {
    (snap.manual_active && snap.auto_active) || has_warning(snap)
}

fn hint(target: Div, message: impl Fn() -> String + Copy + 'static) -> Div {
    tooltip_with(target, TooltipOptions::new(), move |_| {
        move || {
            TextStyle::BODY_SMALL
                .apply(
                    div()
                        .max_w(340.0)
                        .p(10.0_f32)
                        .rounded(12.0_f32)
                        .child(text(message)),
                )
                .dyn_style(|s| {
                    s.bg(colors().inverse_surface)
                        .text_color(colors().inverse_on_surface)
                })
        }
    })
}

fn wrapped_body(message: impl IntoView) -> Div {
    TextStyle::BODY_SMALL.apply(div().min_w(0.0).child(message))
}

fn wrapped_muted(message: impl IntoView) -> Div {
    wrapped_body(message).dyn_style(|s| s.text_color(colors().on_surface_variant))
}

fn rule() -> Div {
    div()
        .h(1.0)
        .flex_none()
        .dyn_style(|s| s.bg(colors().outline_variant))
}

fn manual_summary(snap: &Snapshot) -> String {
    if snap.manual_busy {
        tr("Manual: updating…", "手动保护：切换中…")
    } else if snap.manual_active {
        match &snap.saved_profile {
            Some(prev) => format!(
                "{} ({} ← {prev})",
                tr("Manual: suspend blocked", "手动保护：已开启（阻止休眠）"),
                "power-saver"
            ),
            None => tr("Manual: suspend blocked", "手动保护：已开启（阻止休眠）"),
        }
    } else {
        tr("Manual: off", "手动保护：关闭")
    }
}

fn auto_summary(snap: &Snapshot) -> String {
    if snap.watcher_loaded && !snap.watcher_active {
        return tr(
            "Agents: nosleep-agents.service stopped",
            "Agent 保护：nosleep-agents.service 未运行",
        );
    }
    if snap.auto_active {
        let count = snap.agents.len();
        if count > 0 {
            format!(
                "{} ({count})",
                tr(
                    "Agents: working · deferring lock/suspend",
                    "Agent 保护：工作中 · 推迟锁屏/休眠"
                )
            )
        } else {
            tr(
                "Agents: active · deferring lock/suspend",
                "Agent 保护：生效中 · 推迟锁屏/休眠",
            )
        }
    } else {
        tr("Agents: idle", "Agent 保护：空闲")
    }
}

fn island_hint() -> String {
    let snap = nosleep_service::state().get();
    let mut lines = vec![
        tr(
            "Nosleep · sleep & agent guard",
            "休眠保护 · 手动抑制与 Agent 守护",
        ),
        manual_summary(&snap),
        auto_summary(&snap),
    ];
    if snap.lock_deferred || snap.suspend_deferred || crate::idle::agents_deferring() {
        lines.push(tr(
            "Deferred idle action pending completion",
            "已有空闲锁屏/休眠等待 Agent 完成后执行",
        ));
    }
    for agent in snap.agents.iter().take(3) {
        lines.push(format!("• {agent}"));
    }
    if snap.agents.len() > 3 {
        lines.push(format!(
            "• +{} {}",
            snap.agents.len() - 3,
            tr("more", "个其他 Agent")
        ));
    }
    if let Some(error) = &snap.error {
        lines.push(error.clone());
    }
    lines.push(tr(
        "Click: toggle manual · Right-click: details",
        "左键：切换手动保护 · 右键：查看详情",
    ));
    lines.join("\n")
}

pub(crate) fn island(output: Output, at: BarPlacement) -> Div {
    let vertical = at.is_vertical();
    let state = nosleep_service::state();
    let open_output = output.clone();
    let emblem_output = output.clone();
    let icon_output = output.clone();
    let click_output = output.clone();
    let here = node_ref();

    let emblem = div()
        .size(26.0)
        .rounded(13.0_f32)
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .transition_of([Prop::Background], Transition::ms(180).ease_out())
        .dyn_style(move |s| {
            let snap = state.get();
            let c = colors();
            let open = is_open(&emblem_output, Panel::Nosleep);
            let bg = if open {
                c.surface_container.with_alpha(0.0)
            } else if snap.manual_active && !snap.manual_busy {
                c.primary.with_alpha(0.16)
            } else if snap.auto_active && !snap.manual_busy {
                c.secondary_container
            } else {
                c.surface_container.with_alpha(0.0)
            };
            s.bg(bg)
        })
        .child(dyn_view(move || {
            let snap = state.get();
            let open = is_open(&icon_output, Panel::Nosleep);
            if snap.manual_busy {
                sym(icons::HOURGLASS_EMPTY, 16.0, |c| c.on_surface_variant)
            } else if snap.manual_active {
                sym(icons::COFFEE, 17.0, move |c| {
                    if open {
                        c.on_primary_container
                    } else {
                        c.primary
                    }
                })
                .dyn_style(|s| s.font_axis(b"FILL", 1.0))
            } else if snap.auto_active {
                sym(icons::ROBOT_2, 16.0, move |c| {
                    if open {
                        c.on_primary_container
                    } else {
                        c.on_secondary_container
                    }
                })
            } else {
                sym(icons::COFFEE, 18.0, move |c| {
                    if open {
                        c.on_primary_container
                    } else {
                        c.on_surface_variant
                    }
                })
            }
        }));

    let badges = show(
        move || state.with(|s| has_badge(&s)),
        move || {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(2.0_f32)
                .child(show(
                    move || state.with(|s| s.manual_active && s.auto_active),
                    || sym(icons::ROBOT_2, 12.0, |c| c.secondary),
                ))
                .child(show(
                    move || state.with(|s| has_warning(&s)),
                    || sym(icons::WARNING, 12.0, |c| c.error),
                ))
        },
    );

    let base = pressable(crate::ui::island(), |c| c.on_surface)
        .node_ref(here)
        .justify_center()
        .select_none()
        .cursor(CursorIcon::Pointer)
        .focusable()
        .dyn_style(move |s| {
            let c = colors();
            let badge = state.with(|snap| has_badge(&snap));
            let s = s
                .bg(if is_open(&open_output, Panel::Nosleep) {
                    c.primary_container
                } else {
                    c.surface_container
                })
                .text_color(c.on_surface)
                .shadows(elevation(1));
            if vertical {
                s
            } else {
                s.px(if badge { 7.0_f32 } else { 3.0_f32 })
            }
        })
        .access(|a| a.role(Role::Button).label(island_hint()))
        .on_click(move |_| nosleep_service::toggle_manual())
        .on_aux_click(move |event| match event.button {
            Some(PointerButton::Secondary) => {
                event.stop_propagation();
                let opening = !is_open(&click_output, Panel::Nosleep);
                if let Some(r) = here.logical_rect() {
                    crate::panels::toggle_at(
                        &click_output,
                        Panel::Nosleep,
                        AnchorPoint {
                            x: r.x + r.width / 2.0,
                            y: r.y + r.height / 2.0,
                        },
                    );
                } else {
                    toggle(&click_output, Panel::Nosleep);
                }
                if opening {
                    nosleep_service::refresh();
                }
            }
            Some(PointerButton::Middle) => {
                event.stop_propagation();
                crate::quick_settings::open_at(crate::quick_settings::Page::Idle);
            }
            _ => {}
        })
        .child(emblem)
        .child(badges);

    let node = if vertical {
        base.flex_col()
            .w(40.0)
            .h(Length::Auto)
            .py(7.0_f32)
            .gap(3.0_f32)
            .rounded(20.0_f32)
    } else {
        base.flex_row().h(32.0).gap(4.0_f32).rounded(16.0_f32)
    };

    hint(node, island_hint)
}

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(10.0_f32)
        .p(12.0_f32)
        .rounded(20.0_f32)
        .dyn_style(|s| s.bg(colors().surface_container_high))
}

fn bubble(
    symbol: &'static icons::Symbol,
    active: impl Fn() -> bool + Copy + 'static,
    filled: bool,
    accent: fn(&anti_webui_md3e::ColorScheme) -> (Color, Color),
) -> Div {
    div()
        .size(34.0)
        .rounded(12.0_f32)
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .transition_of([Prop::Background], Transition::ms(180).ease_out())
        .dyn_style(move |s| {
            let c = colors();
            let (bg, fg) = if active() {
                accent(&c)
            } else {
                (c.secondary_container, c.on_secondary_container)
            };
            s.bg(bg)
                .text_color(fg)
                .font_axis(b"FILL", if filled && active() { 1.0 } else { 0.0 })
        })
        .child(anti_webui_md3e::components::sized_icon(symbol, 18.0))
}

pub(crate) fn panel(output: Output, anchor: Option<AnchorPoint>, at: BarPlacement) -> Div {
    let state = nosleep_service::state();
    let width = output.logical_size.width as f32;
    let height = output.logical_size.height as f32;
    let (x, y) = anchor
        .map(|a| (a.x, a.y))
        .unwrap_or((width - EDGE - PANEL_WIDTH / 2.0, height * 0.6));
    let left = (x - PANEL_WIDTH / 2.0).clamp(EDGE, (width - PANEL_WIDTH - EDGE).max(EDGE));
    let top = signal(y);

    let root = div().w(PANEL_WIDTH);
    let root = if at.is_vertical() {
        root.dyn_style(move |s| s.mt(top.get()))
            .on_resized(move |_, h| {
                top.set((y - h / 2.0).clamp(EDGE, (height - h - EDGE).max(EDGE)));
            })
    } else {
        root.ml(left)
    };

    let manual_binding = Binding::new(
        move || state.with(|s| s.manual_active),
        move |on| {
            if !state.with_untracked(|s| s.manual_busy) {
                nosleep_service::set_manual(on);
            }
        },
    );

    root.p(16.0_f32)
        .rounded(30.0_f32)
        .shadows(elevation(3))
        .flex()
        .flex_col()
        .gap(12.0_f32)
        .dyn_style(|s| {
            let c = colors();
            s.bg(c.surface_container).text_color(c.on_surface)
        })
        // Header
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(0.0)
                        .child(static_txt(TextStyle::TITLE_LARGE, tr("Nosleep", "休眠保护")))
                        .child(wrapped_muted(tr(
                            "Sleep block & agent guard",
                            "手动休眠抑制与 Agent 保护",
                        ))),
                )
                .child(
                    hint(
                        styled_icon_button(
                            icons::REFRESH,
                            IconButtonStyle::standard()
                                .size(ButtonSize::ExtraSmall)
                                .disabled_when(move || state.with(|s| s.manual_busy)),
                        )
                        .flex_none()
                        .access(|a| a.label(tr("Refresh", "刷新")))
                        .on_click(|_| nosleep_service::refresh()),
                        || tr("Refresh", "刷新"),
                    ),
                )
                .child(
                    hint(
                        styled_icon_button(
                            icons::TIMER,
                            IconButtonStyle::standard().size(ButtonSize::ExtraSmall),
                        )
                        .flex_none()
                        .access(|a| a.label(tr("Idle & sleep settings", "空闲与休眠设置")))
                        .on_click(|_| {
                            crate::quick_settings::open_at(crate::quick_settings::Page::Idle)
                        }),
                        || tr("Idle & sleep settings", "空闲与休眠设置"),
                    ),
                ),
        )
        // Error / stopped-watcher warning
        .child(show(
            move || state.with(|s| has_warning(&s)),
            move || {
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(8.0_f32)
                    .p(12.0_f32)
                    .rounded(16.0_f32)
                    .dyn_style(|s| {
                        let c = colors();
                        s.bg(c.error_container).text_color(c.on_error_container)
                    })
                    .child(sym(icons::WARNING, 16.0, |c| c.on_error_container).flex_none())
                    .child(wrapped_body(text(move || {
                        state.with(|s| {
                            if let Some(error) = &s.error {
                                error.clone()
                            } else {
                                tr(
                                    "nosleep-agents.service is not running. Agent protection is inactive.",
                                    "nosleep-agents.service 未运行，Agent 自动保护未生效。",
                                )
                            }
                        })
                    })))
            },
        ))
        // Manual protection card
        .child(
            card().child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(10.0_f32)
                    .child(bubble(
                        icons::COFFEE,
                        move || state.with(|s| s.manual_active),
                        true,
                        |c| (c.primary, c.on_primary),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(0.0)
                            .gap(2.0_f32)
                            .child(static_txt(
                                TextStyle::TITLE_SMALL,
                                tr("Manual protection", "手动休眠保护"),
                            ))
                            .child(wrapped_muted(text(move || {
                                state.with(|s| {
                                    if s.manual_busy {
                                        tr("Updating systemd inhibitor…", "正在切换休眠抑制…")
                                    } else if s.manual_active {
                                        let base = tr(
                                            "Suspend blocked · lock & screen-off allowed",
                                            "已阻止挂起 · 允许自动锁屏与熄屏",
                                        );
                                        match &s.saved_profile {
                                            Some(prev) => format!("{base} · power-saver ← {prev}"),
                                            None => base,
                                        }
                                    } else {
                                        tr("Normal sleep allowed", "允许正常休眠")
                                    }
                                })
                            }))),
                    )
                    .child(
                        switch(manual_binding)
                            .access(|a| a.label(tr("Manual protection", "手动休眠保护")))
                            .dyn_style(move |s| {
                                s.opacity(if state.with(|s| s.manual_busy) {
                                    0.55_f32
                                } else {
                                    1.0_f32
                                })
                            }),
                    ),
            ),
        )
        // Agent protection card
        .child(
            card()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(10.0_f32)
                        .child(bubble(
                            icons::ROBOT_2,
                            move || state.with(|s| s.auto_active),
                            false,
                            |c| (c.secondary, c.on_secondary),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(0.0)
                                .gap(2.0_f32)
                                .child(static_txt(
                                    TextStyle::TITLE_SMALL,
                                    tr("Agent protection", "Agent 自动保护"),
                                ))
                                .child(wrapped_muted(text(move || {
                                    let deferring_idle = crate::idle::agents_deferring();
                                    state.with(|s| {
                                        if s.watcher_loaded && !s.watcher_active {
                                            tr(
                                                "nosleep-agents.service is stopped",
                                                "nosleep-agents.service 未运行",
                                            )
                                        } else if s.auto_active
                                            && (s.lock_deferred
                                                || s.suspend_deferred
                                                || deferring_idle)
                                        {
                                            tr(
                                                "Idle lock/suspend deferred until agents finish",
                                                "已推迟空闲锁屏/挂起，完成后若仍空闲将补做",
                                            )
                                        } else if s.auto_active {
                                            tr(
                                                "Deferring automatic lock & suspend · screen-off active",
                                                "推迟自动锁屏与挂起 · 自动熄屏仍生效",
                                            )
                                        } else {
                                            tr(
                                                "Watching Herdr · no working agents",
                                                "正在监视 Herdr · 当前无工作 Agent",
                                            )
                                        }
                                    })
                                }))),
                        )
                        .child(show(
                            move || state.with(|s| s.auto_active),
                            move || {
                                TextStyle::LABEL_SMALL
                                    .apply(
                                        div()
                                            .px(8.0_f32)
                                            .py(3.0_f32)
                                            .rounded(10.0_f32)
                                            .flex_none()
                                            .child(text(move || {
                                                let count = state.with(|s| s.agents.len());
                                                if count > 0 {
                                                    count.to_string()
                                                } else {
                                                    tr("Active", "生效")
                                                }
                                            })),
                                    )
                                    .dyn_style(|s| {
                                        let c = colors();
                                        s.bg(c.secondary_container)
                                            .text_color(c.on_secondary_container)
                                    })
                            },
                        )),
                )
                .child(show(
                    move || state.with(|s| s.auto_active || !s.agents.is_empty()),
                    move || {
                        div()
                            .flex()
                            .flex_col()
                            .gap(6.0_f32)
                            .child(rule())
                            .child(
                                TextStyle::LABEL_SMALL
                                    .apply(div().child(tr("Working agents", "工作中的 Agent")))
                                    .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(6.0_f32)
                                    .max_h(148.0)
                                    .overflow_y_scroll()
                                    .child(dyn_view(move || {
                                        let agents = state.with(|s| s.agents.clone());
                                        if agents.is_empty() {
                                            return wrapped_muted(tr(
                                                "Active marker present · reading Herdr list…",
                                                "检测到工作标记 · 正在读取 Herdr 列表…",
                                            ));
                                        }
                                        let mut list = div().flex().flex_col().gap(6.0_f32);
                                        for agent in agents {
                                            list = list.child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_start()
                                                    .gap(6.0_f32)
                                                    .child(
                                                        sym(icons::TERMINAL, 14.0, |c| c.secondary)
                                                            .flex_none()
                                                            .mt(1.0_f32),
                                                    )
                                                    .child(wrapped_body(agent)),
                                            );
                                        }
                                        list
                                    })),
                            )
                    },
                )),
        )
        // Footer
        .child(rule())
        .child(wrapped_muted(tr(
            "Left-click the bar icon to toggle manual protection · Middle-click for Idle settings",
            "左键状态栏图标切换手动保护 · 中键打开空闲设置",
        )))
}
