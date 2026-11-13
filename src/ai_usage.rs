//! Compact remaining-quota overview. Personal reset windows and public announcements stay separate.

use crate::ai_usage_service::{self as service, AiUsageConfig, Entry, Error, Metric, ResetEvent};
use crate::panels::{Panel, is_open, toggle};
use crate::placement::BarPlacement;
use crate::settings::settings;
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::{static_txt, sym, txt};
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui_md3e::components::{
    ButtonSize, ButtonStyle, IconButtonStyle, SliderStyle, circular_progress, filter_chip,
    styled_button, styled_icon_button, styled_slider, switch,
};
use anti_webui_md3e::icons;
use std::sync::LazyLock;

const SETUP: &str = "https://github.com/akitaonrails/ai-usagebar";
const RESETS: &str = "https://codex-resets.com";

pub(crate) fn tr(en: &str, zh: &str) -> String {
    static CHINESE: LazyLock<bool> = LazyLock::new(|| {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
            .is_some_and(|locale| locale.to_ascii_lowercase().starts_with("zh"))
    });
    if *CHINESE { zh } else { en }.to_owned()
}

fn minute_now() -> i64 {
    // Subscribe to the shell's shared minute clock, never to an animation/frame clock.
    local_time(Every::Minute);
    service::now()
}

fn duration(seconds: i64) -> String {
    let minutes = seconds.max(0).saturating_add(59) / 60;
    let (major, minor, unit, subunit) = if minutes < 60 {
        return format!("{minutes}{}", tr("m", "分"));
    } else if minutes < 1440 {
        (minutes / 60, minutes % 60, tr("h", "时"), tr("m", "分"))
    } else {
        (
            minutes / 1440,
            minutes % 1440 / 60,
            tr("d", "天"),
            tr("h", "时"),
        )
    };
    if minor == 0 {
        format!("{major}{unit}")
    } else {
        format!("{major}{unit} {minor}{subunit}")
    }
}

fn countdown(at: Option<i64>) -> String {
    match at {
        Some(at) if at > minute_now() => format!(
            "{} {}",
            tr("Resets in", "重置还需"),
            duration(at - service::now())
        ),
        Some(_) => tr("Reset due · awaiting update", "已到重置时间 · 等待更新"),
        None => tr("Reset time unknown", "重置时间未知"),
    }
}

fn error_text(error: &Error) -> String {
    match error {
        Error::MissingCli => tr(
            "Install ai-usagebar and make it available on PATH.",
            "请安装 ai-usagebar 并将其加入 PATH。",
        ),
        Error::Cli => tr(
            "ai-usagebar could not read usage. Check your account setup in the CLI.",
            "ai-usagebar 无法读取用量。请在 CLI 中检查账户配置。",
        ),
        Error::Timeout => tr(
            "The request timed out. A retry is scheduled.",
            "请求超时，将自动重试。",
        ),
        Error::Format => tr(
            "The source returned an unsupported report. Check for an ai-usagebar update.",
            "数据格式不受支持。请检查 ai-usagebar 更新。",
        ),
        Error::Network => tr(
            "The public source is unreachable. A retry is scheduled.",
            "无法连接公开数据源，将自动重试。",
        ),
        Error::RateLimit => tr(
            "The source asked us to wait. Refresh respects its retry delay.",
            "数据源要求稍后重试，手动刷新也会遵守等待时间。",
        ),
        Error::Start => tr(
            "The background request could not start. Try refreshing again.",
            "无法启动后台请求，请再次刷新。",
        ),
    }
}

fn body(message: impl Into<String>) -> Div {
    TextStyle::BODY_SMALL.apply(div().min_w(0.0).child(message.into()))
}

fn muted(message: impl Into<String>) -> Div {
    body(message).dyn_style(|s| s.text_color(colors().on_surface_variant))
}

fn rule() -> Div {
    div()
        .h(1.0)
        .flex_none()
        .dyn_style(|s| s.bg(colors().outline_variant))
}

fn action(label: impl IntoTextSource) -> Div {
    styled_button(label, ButtonStyle::text().size(ButtonSize::Small)).flex_none()
}

fn disclosure(label: String, open: Signal<bool>) -> Div {
    styled_button(label, ButtonStyle::text().size(ButtonSize::Small))
        .justify_start()
        .gap(6.0_f32)
        .max_w(Length::Percent(100.0))
        .child(
            sym(icons::EXPAND_MORE, 16.0, |c| c.on_surface_variant)
                .dyn_style(move |s| s.rotate(if open.get() { 180.0_f32 } else { 0.0_f32 })),
        )
        .access(move |a| a.expanded(open.get()))
        .on_click(move |_| open.update(|value| *value = !*value))
}

fn icon_action(
    symbol: &'static icons::Symbol,
    label: impl Fn() -> String + Copy + 'static,
    style: IconButtonStyle,
) -> Div {
    hint(
        styled_icon_button(symbol, style.size(ButtonSize::ExtraSmall))
            .flex_none()
            .access(move |a| a.label(label())),
        label,
    )
}

fn refresh_hint() -> String {
    let state = service::state();
    let busy = state.usage.with(|job| job.busy) || state.resets.with(|job| job.busy);
    let mut lines = vec![if busy {
        tr("Refreshing…", "刷新中…")
    } else {
        tr("Refresh", "刷新")
    }];
    state.usage.with(|job| {
        if let Some(at) = job.updated_at {
            lines.push(format!("{} {}", tr("Updated", "更新于"), service::date(at)));
        }
        if let Some(error) = &job.error {
            lines.push(error_text(error));
        }
    });
    lines.join("\n")
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

fn link(label: String, url: String) -> Div {
    let failed = signal(false);
    div()
        .flex()
        .flex_col()
        .items_start()
        .child(
            action(label)
                .access(|a| a.role(Role::Link))
                .on_click(move |_| {
                    // Model URLs are validated too; keep this boundary safe for preview data and future callers.
                    let safe = !url.chars().any(char::is_control)
                        && url::Url::parse(&url).is_ok_and(|u| {
                            u.scheme() == "https"
                                && u.host_str().is_some()
                                && u.username().is_empty()
                                && u.password().is_none()
                        });
                    if !safe {
                        failed.set(true);
                        return;
                    }
                    match std::process::Command::new("xdg-open")
                        .arg(&url)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(mut child) => {
                            failed.set(false);
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            });
                        }
                        Err(_) => failed.set(true),
                    }
                }),
        )
        .child(show(
            move || failed.get(),
            || {
                body(tr(
                    "Could not open the link. Check that xdg-open is installed.",
                    "无法打开链接，请检查是否已安装 xdg-open。",
                ))
                .dyn_style(|s| s.text_color(colors().error))
            },
        ))
}

fn percent(metric: &Metric) -> String {
    metric.remaining().map_or_else(
        || {
            if metric.value.is_empty() {
                "—".to_owned()
            } else {
                metric.value.clone()
            }
        },
        |remaining| {
            if remaining > 0.0 && remaining < 1.0 {
                "<1%".to_owned()
            } else {
                format!("{remaining:.0}%")
            }
        },
    )
}

fn window_name(metric: &Metric) -> String {
    if !metric.label.is_empty() {
        metric.label.clone()
    } else if let Some(seconds) = metric.window_secs {
        format!("{} {}", duration(seconds as i64), tr("window", "额度周期"))
    } else {
        tr("Usage", "用量")
    }
}

fn has_scheduled_reset() -> bool {
    settings().with(|s| {
        s.ai_usage.enabled && s.ai_usage.resets_enabled && s.ai_usage.show_reset_indicator
    }) && service::state().resets.with(|job| {
        job.data
            .as_ref()
            .is_some_and(|data| data.scheduled.is_some())
    })
}

fn island_remaining() -> Option<f64> {
    settings().with(|s| {
        service::state().usage.with(|job| {
            job.data
                .as_ref()
                .and_then(|report| report.select(&s.ai_usage.provider))
                .and_then(Entry::headline)
                .and_then(Metric::remaining)
        })
    })
}

fn quota_ring() -> Div {
    let remaining = memo(island_remaining);
    div()
        .relative()
        .size(26.0)
        .flex_none()
        .child(show(
            move || remaining.get().is_some(),
            move || {
                circular_progress(move || (remaining.get().unwrap_or(0.0) / 100.0) as f32)
                    .size(26.0)
            },
        ))
        .child(
            div()
                .absolute()
                .inset(0.0)
                .flex()
                .items_center()
                .justify_center()
                .child(dyn_view(move || {
                    let symbol = match remaining.get() {
                        Some(0.0) => icons::BLOCK,
                        Some(_) => icons::AUTO_AWESOME,
                        None if service::state().usage.with(|job| job.busy) => {
                            icons::HOURGLASS_EMPTY
                        }
                        None => icons::HELP_OUTLINE,
                    };
                    sym(
                        symbol,
                        if remaining.get().is_some() {
                            12.0
                        } else {
                            18.0
                        },
                        |c| c.on_surface,
                    )
                })),
        )
}

fn island_warning() -> bool {
    let config = settings().with(|s| s.ai_usage.clone());
    service::state().usage.with(|job| {
        let entry = job
            .data
            .as_ref()
            .and_then(|report| report.select(&config.provider));
        job.error.is_some()
            || entry.is_some_and(|entry| entry.stale || entry.failed)
            || (job.data.is_some() && entry.is_none() && config.provider != "auto")
    })
}

fn island_hint() -> String {
    let config = settings().with(|s| s.ai_usage.clone());
    service::state().usage.with(|job| {
        let mut lines = vec![tr("AI usage · remaining quota", "AI 用量 · 剩余额度")];
        if let Some(entry) = job
            .data
            .as_ref()
            .and_then(|report| report.select(&config.provider))
        {
            lines.push(entry.name.clone());
            if let Some(metric) = entry.headline() {
                lines.push(format!("{} · {}", window_name(metric), percent(metric)));
                lines.push(countdown(metric.reset_at));
            }
            if entry.stale {
                lines.push(tr("Cached reading", "缓存数据"));
            }
            if entry.failed {
                lines.push(tr("Account unavailable", "账户不可用"));
            }
        } else if config.provider != "auto" {
            lines.push(format!(
                "{}: {}",
                tr("Pinned account missing", "固定账户缺失"),
                config.provider
            ));
        } else {
            lines.push(tr("No usage data yet", "暂无用量数据"));
        }
        if let Some(error) = &job.error {
            lines.push(error_text(error));
        }
        if has_scheduled_reset() {
            lines.push(tr(
                "Public reset announcement · open for details",
                "公开重置公告 · 打开查看详情",
            ));
        }
        lines.push(tr(
            "Click: overview · Right-click: refresh",
            "左键：总览 · 右键：刷新",
        ));
        lines.join("\n")
    })
}

pub(crate) fn island(output: Output, at: BarPlacement) -> Div {
    let vertical = at.is_vertical();
    let open_output = output.clone();
    let node = crate::ui::pressable(crate::ui::island(), |c| c.on_surface)
        .justify_center()
        .select_none()
        .cursor(CursorIcon::Pointer)
        .focusable()
        .gap(if vertical { 0.0_f32 } else { 6.0_f32 })
        .px(if vertical { 2.0_f32 } else { 10.0_f32 })
        .rounded(if vertical { 20.0_f32 } else { 16.0_f32 })
        .dyn_style(move |s| {
            let c = colors();
            s.bg(if is_open(&open_output, Panel::AiUsage) {
                c.primary_container
            } else {
                c.surface_container
            })
            .text_color(c.on_surface)
            .shadows(elevation(1))
        })
        .access(|a| a.role(Role::Button).label(island_hint()))
        .on_click(move |_| toggle(&output, Panel::AiUsage))
        .on_aux_click(|event| match event.button {
            Some(PointerButton::Secondary) => {
                event.stop_propagation();
                service::refresh();
            }
            Some(PointerButton::Middle) => {
                event.stop_propagation();
                crate::quick_settings::open_at(crate::quick_settings::Page::Settings);
            }
            _ => {}
        })
        .child(quota_ring())
        .child(show(
            || island_warning() || has_scheduled_reset(),
            || {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(2.0_f32)
                    .child(show(island_warning, || {
                        sym(icons::WARNING, 12.0, |c| c.error)
                    }))
                    .child(show(has_scheduled_reset, || {
                        sym(icons::EVENT, 12.0, |c| c.primary)
                    }))
            },
        ));
    let node = if vertical {
        node.flex_col().w(40.0).h(Length::Auto).py(6.0_f32)
    } else {
        node.flex_row().h(32.0).child(show(
            || settings().with(|s| s.ai_usage.show_countdown),
            || {
                txt(TextStyle::LABEL_SMALL, || {
                    let config = settings().with(|s| s.ai_usage.clone());
                    service::state().usage.with(|job| {
                        job.data
                            .as_ref()
                            .and_then(|report| report.select(&config.provider))
                            .and_then(Entry::headline)
                            .and_then(|metric| metric.reset_at)
                            .map(|at| {
                                if at > minute_now() {
                                    duration(at - service::now())
                                } else {
                                    tr("Due", "已到期")
                                }
                            })
                            .unwrap_or_default()
                    })
                })
                .dyn_style(|s| s.text_color(colors().on_surface_variant))
            },
        ))
    };
    hint(node, island_hint)
}

fn metric_row(metric: &Metric) -> Div {
    let remaining = metric.remaining();
    let label = window_name(metric);
    let value = percent(metric);
    let reset = metric.reset_at;
    let critical = remaining.is_some_and(|value| value <= 0.0) || metric.severity == "critical";
    let warning = remaining.is_some_and(|value| value <= 20.0) || metric.severity == "high";
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(10.0_f32)
        .child(body(label.clone()).w(102.0).flex_none());
    if let Some(remaining) = remaining {
        row = row.child(
            div()
                .relative()
                .flex_1()
                .min_w(24.0)
                .h(4.0)
                .rounded(2.0_f32)
                .dyn_style(|s| s.bg(colors().surface_container_highest))
                .access(move |a| {
                    a.role(Role::ProgressBar)
                        .label(format!("{} · {}", label, tr("remaining", "剩余")))
                        .value(remaining, 0.0, 100.0)
                })
                .child(
                    div()
                        .absolute()
                        .left(0.0)
                        .top(0.0)
                        .h(4.0)
                        .rounded(2.0_f32)
                        .w(Length::Percent(remaining as f32))
                        .dyn_style(move |s| {
                            s.bg(if critical {
                                colors().error
                            } else if warning {
                                colors().secondary
                            } else {
                                colors().primary
                            })
                        }),
                ),
        );
    } else {
        row = row.child(div().flex_1());
    }
    row = row.child(
        body(value)
            .min_w(46.0)
            .text_align(TextAlign::Right)
            .dyn_style(move |s| {
                s.text_color(if critical {
                    colors().error
                } else if warning {
                    colors().secondary
                } else {
                    colors().on_surface
                })
            }),
    );
    let content = div().flex().flex_col().gap(2.0_f32).child(row);
    if reset.is_some() || remaining.is_some() {
        content.child(hint(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(4.0_f32)
                .access(move |a| a.label(countdown(reset)))
                .child(sym(icons::SCHEDULE, 12.0, |c| c.on_surface_variant))
                .child(
                    txt(TextStyle::LABEL_SMALL, move || match reset {
                        Some(at) if at > minute_now() => duration(at - service::now()),
                        Some(_) => tr("Due", "待更新"),
                        None => "—".to_owned(),
                    })
                    .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                ),
            move || countdown(reset),
        ))
    } else {
        content
    }
}

fn entry_details(entry: &Entry) -> Div {
    let mut details = div().flex().flex_col().gap(6.0_f32);
    if !entry.plan.is_empty() {
        details = details.child(muted(format!("{}: {}", tr("Plan", "套餐"), entry.plan)));
    }
    if let Some(at) = entry.fetched_at {
        details = details.child(muted(format!(
            "{}: {}",
            tr("Account fetched", "账户获取时间"),
            service::date(at)
        )));
    }
    for metric in &entry.metrics {
        if let Some(at) = metric.reset_at {
            details = details.child(muted(format!(
                "{} · {}",
                window_name(metric),
                service::date(at)
            )));
        }
    }
    if let Some(available) = entry.available {
        details = details.child(muted(format!(
            "{}: {available}",
            tr("Personal reset credits", "个人重置次数")
        )));
    }
    for credit in &entry.credits {
        let expiry = credit
            .expires_at
            .map(service::date)
            .unwrap_or_else(|| tr("Unknown expiry", "到期时间未知"));
        details = details.child(muted(format!(
            "{} · {} {expiry}",
            credit.title,
            tr("Expires", "到期")
        )));
    }
    for block in &entry.blocks {
        details = details.child(muted(block.clone()));
    }
    details
}

fn account(entry: &Entry, expanded: Signal<Vec<String>>) -> Div {
    let id = entry.id.clone();
    let expanded_id = id.clone();
    let label_id = id.clone();
    let details_entry = entry.clone();
    let name = if entry.name.is_empty() {
        entry.id.clone()
    } else {
        entry.name.clone()
    };
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(6.0_f32)
        .child(TextStyle::TITLE_SMALL.apply(div().flex_1().min_w(0.0).child(name.clone())));
    if entry.failed || entry.stale {
        header = header.child(
            body(if entry.failed {
                tr("Unavailable", "不可用")
            } else {
                tr("Cached", "缓存")
            })
            .dyn_style(|s| s.text_color(colors().error)),
        );
    }
    header = header.child(
        icon_action(
            icons::EXPAND_MORE,
            || tr("Account details", "账户详情"),
            IconButtonStyle::standard(),
        )
        .dyn_style(move |s| {
            s.rotate(if expanded.with(|ids| ids.contains(&label_id)) {
                180.0_f32
            } else {
                0.0_f32
            })
        })
        .access(move |a| {
            a.label(format!("{} · {}", name, tr("Details", "详情")))
                .expanded(expanded.with(|ids| ids.contains(&expanded_id)))
        })
        .on_click(move |_| {
            expanded.update(|ids| {
                if ids.contains(&id) {
                    ids.retain(|candidate| candidate != &id);
                } else {
                    ids.push(id.clone());
                }
            })
        }),
    );
    let mut rows = div()
        .flex()
        .flex_col()
        .gap(8.0_f32)
        .py(8.0_f32)
        .child(header);
    if entry.failed {
        rows = rows.child(muted(tr(
            "This account could not be read. Check its CLI configuration and refresh.",
            "无法读取此账户。请检查 CLI 配置后刷新。",
        )));
    } else {
        for metric in &entry.metrics {
            rows = rows.child(metric_row(metric));
        }
        if entry.metrics.is_empty() {
            if entry.blocks.is_empty() {
                rows = rows.child(muted(tr("No quota windows reported", "未报告额度周期")));
            } else {
                for block in &entry.blocks {
                    rows = rows.child(body(block.clone()));
                }
            }
        }
    }
    let id = entry.id.clone();
    rows.child(show(
        move || expanded.with(|ids| ids.contains(&id)),
        move || entry_details(&details_entry),
    ))
}

fn usage_content(expanded: Signal<Vec<String>>) -> Div {
    let job = service::state().usage.get();
    let config = settings().with(|s| s.ai_usage.clone());
    let mut content = div().flex().flex_col().gap(8.0_f32);
    if !config.enabled {
        return content.child(muted(tr(
            "AI usage is off. Enable it in Preferences.",
            "AI 用量已关闭，可在偏好设置中启用。",
        )));
    }
    if let Some(error) = &job.error {
        content =
            content.child(body(error_text(error)).dyn_style(|s| s.text_color(colors().error)));
        if job.data.is_some() {
            content = content.child(muted(tr(
                "Showing cached readings, not live availability.",
                "显示的是缓存读数，并非实时可用额度。",
            )));
        }
    }
    if let Some(report) = &job.data {
        if config.provider != "auto" && report.select(&config.provider).is_none() {
            content = content.child(
                body(format!(
                    "{}: {}. {}",
                    tr("Pinned account missing", "固定账户缺失"),
                    config.provider,
                    tr(
                        "Choose an available account in Preferences.",
                        "请在偏好设置中选择可用账户。"
                    )
                ))
                .dyn_style(|s| s.text_color(colors().error)),
            );
        }
        if !report.entries.is_empty() {
            for (index, entry) in report.entries.iter().enumerate() {
                if index > 0 {
                    content = content.child(rule());
                }
                content = content.child(account(entry, expanded));
            }
            return content;
        }
    }
    content.child(body(if job.busy { tr("Reading your accounts…", "正在读取账户…") } else { tr("No usage data yet", "暂无用量数据") }))
        .child(muted(tr("Configure your providers in ai-usagebar, then run ai-usagebar usage --json and refresh here.", "请在 ai-usagebar 中配置服务商，运行 ai-usagebar usage --json，再在此处刷新。")))
        .child(link(tr("Set up ai-usagebar", "配置 ai-usagebar"), SETUP.to_owned()))
}

fn event_row(event: &ResetEvent, scheduled: bool, expanded: Signal<bool>) -> Div {
    let at = if scheduled {
        event.scheduled_for
    } else {
        event.announced_at
    };
    let title = if scheduled {
        match at {
            Some(at) if at > minute_now() => format!(
                "{} · {}",
                tr("Public reset in", "公开重置还需"),
                duration(at - service::now())
            ),
            Some(_) => tr(
                "Public reset · awaiting confirmation",
                "公开重置 · 等待确认",
            ),
            None => tr("Public reset · time unconfirmed", "公开重置 · 时间未确认"),
        }
    } else {
        format!(
            "{} · {}",
            tr("Latest public report", "最近公开报告"),
            at.map(service::date)
                .unwrap_or_else(|| tr("Time unknown", "时间未知"))
        )
    };
    let event = event.clone();
    div().flex().flex_col().items_start().gap(4.0_f32)
        .child(disclosure(title, expanded))
        .child(show(move || expanded.get(), move || {
            let kind = if event.kind == "banked" { tr("Banked reset", "储存额度重置") } else { tr("Regular reset", "常规额度重置") };
            let mut detail = div().flex().flex_col().gap(4.0_f32)
                .child(muted(format!("{kind} · {}", at.map(service::date).unwrap_or_else(|| tr("Time unknown", "时间未知")))))
                .child(muted(event.text.clone()));
            if scheduled {
                detail = detail.child(muted(tr("This is a public announcement, not your account's reset. Elapsed time does not confirm execution.",
                    "这是公开公告，并非个人额度重置；时间经过不代表已执行。")));
            }
            detail.child(link(tr("View source", "查看来源"), event.url.clone().unwrap_or_else(|| RESETS.to_owned())))
        }))
}

fn public_summary() -> String {
    let now = minute_now();
    service::state().resets.with(|job| {
        let mut parts = vec![tr("Public Codex resets", "Codex 公开重置")];
        if let Some(data) = &job.data {
            if job.error.is_some() {
                parts.push(tr("cached", "缓存"));
            }
            if let Some(event) = &data.scheduled {
                parts.push(match event.scheduled_for {
                    Some(at) if at > now => format!("{} {}", tr("in", "还需"), duration(at - now)),
                    Some(_) => tr("awaiting confirmation", "等待确认"),
                    None => tr("time unconfirmed", "时间未确认"),
                });
            }
            let watch = data
                .watch
                .as_ref()
                .filter(|w| w.expires_at.is_some_and(|at| at > now));
            if watch.is_some() {
                parts.push(tr("AI forecast", "AI 预测"));
            } else if data.scheduled.is_none() {
                parts.push(tr("no announcement", "暂无公告"));
            }
        } else {
            parts.push(if job.busy {
                tr("checking", "查询中")
            } else {
                tr("unavailable", "不可用")
            });
        }
        parts.join(" · ")
    })
}

fn public_resets(disclosures: [Signal<bool>; 3]) -> Div {
    let now = minute_now();
    let job = service::state().resets.get();
    let [announcement_open, forecast_open, details] = disclosures;
    let mut content = div().flex().flex_col().gap(4.0_f32).child(rule());
    if let Some(error) = &job.error {
        content =
            content.child(body(error_text(error)).dyn_style(|s| s.text_color(colors().error)));
        if job.data.is_some() {
            content = content.child(muted(tr("Cached public information", "缓存的公开信息")));
        }
    }
    if let Some(data) = &job.data {
        if let Some(event) = &data.scheduled {
            content = content.child(event_row(event, true, announcement_open));
        }
        let active_watch = data
            .watch
            .as_ref()
            .filter(|watch| watch.expires_at.is_some_and(|at| at > now));
        if let Some(watch) = active_watch {
            let chance = watch
                .chance
                .map(|chance| format!(" · {chance:.0}%"))
                .unwrap_or_default();
            let title = format!(
                "{}{chance}",
                tr("AI forecast · not official", "AI 预测 · 非官方")
            );
            let watch = watch.clone();
            let expanded = forecast_open;
            content = content.child(disclosure(title, expanded)).child(show(
                move || expanded.get(),
                move || {
                    let mut detail = div()
                        .flex()
                        .flex_col()
                        .gap(4.0_f32)
                        .child(muted(watch.text.clone()));
                    detail = detail.child(muted(tr(
                        "Not an official commitment from OpenAI.",
                        "不代表 OpenAI 的官方承诺。",
                    )));
                    if !watch.window.is_empty() {
                        detail = detail.child(muted(watch.window.clone()));
                    }
                    if !watch.level.is_empty() {
                        detail = detail.child(muted(format!(
                            "{}: {}",
                            tr("Signal", "信号"),
                            watch.level
                        )));
                    }
                    if let Some(at) = watch.observed_at {
                        detail = detail.child(muted(format!(
                            "{}: {}",
                            tr("Observed", "观察时间"),
                            service::date(at)
                        )));
                    }
                    if let Some(at) = watch.expires_at {
                        detail = detail.child(muted(format!(
                            "{}: {}",
                            tr("Expires", "到期"),
                            service::date(at)
                        )));
                    }
                    detail.child(link(
                        tr("View forecast source", "查看预测来源"),
                        watch.url.clone().unwrap_or_else(|| RESETS.to_owned()),
                    ))
                },
            ));
        }
        if data.scheduled.is_none() && active_watch.is_none() {
            if let Some(event) = &data.latest {
                content = content.child(event_row(event, false, announcement_open));
            } else {
                content = content.child(muted(tr(
                    "No public reset scheduled",
                    "暂无已公告的公开重置",
                )));
            }
        }
        let data = data.clone();
        let checked = job.updated_at;
        content = content
            .child(disclosure(
                tr("Codex Resets · source", "Codex Resets · 数据源"),
                details,
            ))
            .child(show(
                move || details.get(),
                move || {
                    let mut info = div().flex().flex_col().gap(4.0_f32);
                    if let Some(at) = checked {
                        info = info.child(muted(format!(
                            "{}: {}",
                            tr("Checked", "检查时间"),
                            service::date(at)
                        )));
                    }
                    if let Some(at) = data.generated_at {
                        info = info.child(muted(format!(
                            "{}: {}",
                            tr("Source generated", "数据生成时间"),
                            service::date(at)
                        )));
                    }
                    if let Some(latest) = &data.latest {
                        if let Some(at) = latest.announced_at {
                            info = info.child(muted(format!(
                                "{}: {}",
                                tr("Latest report", "最近报告"),
                                service::date(at)
                            )));
                        }
                    }
                    if let Some(total) = data.total {
                        info = info.child(muted(format!(
                            "{}: {total}",
                            tr("Reported resets", "已报告的重置次数")
                        )));
                    }
                    if let Some(days) = data.average_days {
                        info = info.child(muted(format!(
                            "{}: {days:.1}",
                            tr("Average interval (days)", "平均间隔（天）")
                        )));
                    }
                    info = info.child(link(
                        tr("Data from Codex Resets", "数据来自 Codex Resets"),
                        RESETS.to_owned(),
                    ));
                    info
                },
            ));
    } else {
        content = content.child(muted(if job.busy {
            tr("Checking public announcements…", "正在检查公开公告…")
        } else {
            tr(
                "Public reset information is unavailable",
                "公开重置信息不可用",
            )
        }));
    }
    content
}

fn public_strip(open: Signal<bool>) -> Div {
    let now = minute_now();
    let state = service::state();
    let row = styled_button("", ButtonStyle::text().size(ButtonSize::Small))
        .justify_start()
        .gap(8.0_f32)
        .px(4.0_f32)
        .access(move |a| a.label(public_summary()).expanded(open.get()))
        .on_click(move |_| open.update(|value| *value = !*value))
        .child(sym(icons::PUBLIC, 16.0, |c| c.on_surface_variant))
        .child(static_txt(TextStyle::LABEL_MEDIUM, "Codex Resets"));
    let row = state.resets.with(|job| {
        let mut row = row;
        if let Some(data) = &job.data {
            if let Some(event) = &data.scheduled {
                row = row
                    .child(sym(icons::EVENT, 14.0, |c| c.primary))
                    .child(body(match event.scheduled_for {
                        Some(at) if at > now => duration(at - now),
                        Some(_) => tr("Pending", "待确认"),
                        None => "—".to_owned(),
                    }));
            }
            if data
                .watch
                .as_ref()
                .is_some_and(|w| w.expires_at.is_some_and(|at| at > now))
            {
                row = row.child(hint(
                    sym(icons::AUTO_AWESOME, 16.0, |c| c.secondary),
                    || tr("AI forecast · not official", "AI 预测 · 非官方"),
                ));
            }
        }
        if job.error.is_some() {
            row = row.child(sym(icons::WARNING, 14.0, |c| c.error));
        } else if job.busy {
            row = row.child(sym(icons::HOURGLASS_EMPTY, 14.0, |c| c.on_surface_variant));
        }
        row
    });
    hint(
        row.child(div().flex_1()).child(
            sym(icons::EXPAND_MORE, 16.0, |c| c.on_surface_variant)
                .dyn_style(move |s| s.rotate(if open.get() { 180.0_f32 } else { 0.0_f32 })),
        ),
        public_summary,
    )
}

pub(crate) fn panel() -> Div {
    let expanded = signal(Vec::<String>::new());
    let prefs = signal(false);
    let public_open = signal(false);
    let public_details = [signal(false), signal(false), signal(false)];
    // The public API has no output argument; the smallest live output bounds the panel safely.
    let size = outputs()
        .into_iter()
        .fold((420.0_f32, 680.0_f32), |(w, h), output| {
            (
                w.min((output.logical_size.width as f32 - 40.0).max(240.0)),
                h.min((output.logical_size.height as f32 - 120.0).max(160.0)),
            )
        });
    let state = service::state();
    div()
        .w(size.0)
        .max_h(size.1)
        .p(16.0_f32)
        .rounded(30.0_f32)
        .shadows(elevation(3))
        .flex()
        .flex_col()
        .gap(12.0_f32)
        .dyn_style(|s| {
            s.bg(colors().surface_container)
                .text_color(colors().on_surface)
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .flex_none()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(static_txt(
                            TextStyle::TITLE_LARGE,
                            tr("AI usage", "AI 用量"),
                        ))
                        .child(muted(tr("Remaining quota", "剩余可用额度"))),
                )
                .child(
                    icon_action(
                        icons::REFRESH,
                        refresh_hint,
                        IconButtonStyle::standard().disabled_when(move || {
                            !settings().with(|s| s.ai_usage.enabled)
                                || state.usage.with(|j| j.busy)
                                || state.resets.with(|j| j.busy)
                        }),
                    )
                    .on_click(|_| service::refresh()),
                )
                .child(
                    icon_action(
                        icons::SETTINGS,
                        || tr("Preferences", "偏好设置"),
                        IconButtonStyle::standard(),
                    )
                    .access(move |a| a.label(tr("Preferences", "偏好设置")).expanded(prefs.get()))
                    .on_click(move |_| prefs.update(|value| *value = !*value)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(12.0_f32)
                .min_h(0.0)
                .pr(12.0_f32)
                .max_h((size.1 - 100.0).max(60.0))
                .overflow_y_scroll()
                .child(show(
                    move || prefs.get(),
                    || {
                        div()
                            .flex()
                            .flex_col()
                            .gap(12.0_f32)
                            .child(preferences())
                            .child(rule())
                    },
                ))
                .child(dyn_view(move || usage_content(expanded)))
                .child(show(
                    move || state.usage.with(|job| job.error.is_some()),
                    move || {
                        txt(TextStyle::LABEL_SMALL, move || {
                            state.usage.with(|job| {
                                job.next_at
                                    .filter(|at| *at > minute_now())
                                    .map(|at| {
                                        format!(
                                            "{} {}",
                                            tr("Retry in", "重试还需"),
                                            duration(at - service::now())
                                        )
                                    })
                                    .unwrap_or_default()
                            })
                        })
                        .dyn_style(|s| s.text_color(colors().on_surface_variant))
                    },
                ))
                .child(show(
                    || settings().with(|s| s.ai_usage.enabled && s.ai_usage.resets_enabled),
                    move || {
                        div()
                            .flex()
                            .flex_col()
                            .gap(4.0_f32)
                            .child(rule())
                            .child(dyn_view(move || public_strip(public_open)))
                            .child(show(
                                move || public_open.get(),
                                move || {
                                    div().child(dyn_view(move || public_resets(public_details)))
                                },
                            ))
                    },
                )),
        )
}

fn setting_switch(label: String, binding: Binding<bool>) -> Div {
    let accessible = label.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(10.0_f32)
        .child(TextStyle::BODY_MEDIUM.apply(div().flex_1().min_w(0.0).child(label)))
        .child(switch(binding).access(move |a| a.label(accessible.clone())))
}

fn config_switch(
    label: String,
    get: fn(&AiUsageConfig) -> bool,
    set: fn(&mut AiUsageConfig, bool),
) -> Div {
    setting_switch(
        label,
        Binding::new(
            move || settings().with(|s| get(&s.ai_usage)),
            move |on| {
                settings().update(|s| set(&mut s.ai_usage, on));
            },
        ),
    )
}

fn cadence(public: bool) -> Div {
    let label = if public {
        tr("Public updates", "公开信息更新")
    } else {
        tr("Usage updates", "用量更新")
    };
    let accessible = label.clone();
    let minimum = if public { 5.0 } else { 1.0 };
    let binding = Binding::new(
        move || {
            settings().with(|s| {
                if public {
                    s.ai_usage.resets_refresh_minutes as f32
                } else {
                    s.ai_usage.refresh_minutes as f32
                }
            })
        },
        move |value: f32| {
            settings().update(|s| {
                let value = value.round().clamp(minimum, 120.0) as u64;
                if public {
                    s.ai_usage.resets_refresh_minutes = value;
                } else {
                    s.ai_usage.refresh_minutes = value;
                }
            })
        },
    );
    div()
        .flex()
        .flex_col()
        .gap(4.0_f32)
        .child(txt(TextStyle::BODY_MEDIUM, move || {
            let minutes = settings().with(|s| {
                if public {
                    s.ai_usage.resets_refresh_minutes
                } else {
                    s.ai_usage.refresh_minutes
                }
            });
            format!("{label} · {minutes} {}", tr("min", "分钟"))
        }))
        .child(
            styled_slider(
                binding,
                SliderStyle::new()
                    .range(minimum..=120.0)
                    .steps((120.0 - minimum - 1.0) as u32)
                    .track(4.0)
                    .handle_height(24.0)
                    .no_stops()
                    .label(|value| format!("{value:.0} {}", tr("min", "分钟"))),
            )
            .access(move |a| a.label(accessible.clone())),
        )
}

fn provider_choices() -> Div {
    let job = service::state().usage.get();
    let selected = settings().with(|s| s.ai_usage.provider.clone());
    let mut options = vec![("auto".to_owned(), tr("Automatic", "自动"))];
    if let Some(report) = &job.data {
        for entry in &report.entries {
            if !options.iter().any(|(id, _)| id == &entry.id) {
                options.push((
                    entry.id.clone(),
                    if entry.name.is_empty() {
                        entry.id.clone()
                    } else {
                        entry.name.clone()
                    },
                ));
            }
        }
    }
    if !options.iter().any(|(id, _)| id == &selected) {
        options.push((
            selected.clone(),
            format!("{} · {selected}", tr("Missing", "缺失")),
        ));
    }
    let mut choices = div().flex().flex_col().items_start().gap(6.0_f32);
    for (id, name) in options {
        let current = id.clone();
        choices = choices.child(filter_chip(
            name,
            Binding::new(
                move || settings().with(|s| s.ai_usage.provider == current),
                move |on| {
                    if on {
                        settings().update(|s| s.ai_usage.provider = id.clone());
                    }
                },
            ),
        ));
    }
    choices
}

pub(crate) fn preferences() -> Div {
    div().flex().flex_col().gap(12.0_f32)
        .child(static_txt(TextStyle::TITLE_MEDIUM, tr("AI usage", "AI 用量")))
        .child(config_switch(tr("Enable AI usage", "启用 AI 用量"), |c| c.enabled, |c, on| c.enabled = on))
        .child(show(|| settings().with(|s| s.ai_usage.enabled), || {
            div().flex().flex_col().gap(12.0_f32)
                .child(static_txt(TextStyle::BODY_MEDIUM, tr("Account shown in the bar", "状态栏显示的账户")))
                .child(muted(tr("The overview always shows every account and quota window.", "总览始终显示所有账户及额度周期。")))
                .child(dyn_view(provider_choices))
                .child(cadence(false))
                .child(config_switch(tr("Countdown in the bar", "状态栏显示倒计时"), |c| c.show_countdown, |c, on| c.show_countdown = on))
                .child(config_switch(tr("Public Codex reset information", "Codex 公开重置信息"), |c| c.resets_enabled, |c, on| c.resets_enabled = on))
                .child(show(|| settings().with(|s| s.ai_usage.resets_enabled), || {
                    div().flex().flex_col().gap(12.0_f32).child(cadence(true))
                        .child(config_switch(tr("Announcement indicator in the bar", "状态栏显示公告标记"), |c| c.show_reset_indicator, |c, on| c.show_reset_indicator = on))
                        .child(muted(tr("Uses codex-resets.com. No account data is sent to this public source.", "使用 codex-resets.com，不会向此公开数据源发送账户数据。")))
                }))
        }))
}
