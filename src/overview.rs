//! A workspace overview for niri: uniformly scaled, abstract workspace viewports.
//!
//! The geometry comes from niri's event stream. The UI owns no compositor state of its own beyond
//! the layout details that AntiWebUI's general window model does not retain.

use crate::appinfo::app_id_icon;
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui::shell::niri::{self, Window};
use anti_webui::shell::prelude::*;
use anti_webui_md3e::icons;
use serde_json::json;
use std::rc::Rc;

mod layouts;
use layouts::LayoutCache;

mod viewport;
use viewport::{BrowseState, VisibleWindow, WorkspaceViewport};

const CARD_PAD: f32 = 18.0;
const GRID_GAP: f32 = 16.0;
const PANEL_PAD: f32 = 18.0;
const SCROLLBAR: f32 = 6.0;
const TARGET_VIEWPORT_WIDTH: f32 = 280.0;
const CARD_BORDER: f32 = 1.5;
const CARD_RADIUS: f32 = 24.0;
// Match the nested curves at the card's padded edges.
const VIEWPORT_RADIUS: f32 = CARD_RADIUS - CARD_PAD - CARD_BORDER;

pub fn available() -> bool {
    niri::available()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelGeometry {
    scale: f32,
    width: f32,
    columns: usize,
}

fn card_width(viewport: &WorkspaceViewport, scale: f32) -> f32 {
    viewport.viewport_width * scale + (CARD_PAD + CARD_BORDER) * 2.0
}

fn panel_geometry(viewports: &[WorkspaceViewport], max_width: f32) -> PanelGeometry {
    if viewports.is_empty() {
        return PanelGeometry {
            scale: 0.0,
            width: max_width.min(360.0),
            columns: 1,
        };
    }
    let viewport_width = viewports
        .iter()
        .map(|viewport| viewport.viewport_width)
        .fold(1.0, f32::max);
    let content_limit = max_width - PANEL_PAD * 2.0 - SCROLLBAR;
    let scale =
        TARGET_VIEWPORT_WIDTH.min(content_limit - (CARD_PAD + CARD_BORDER) * 2.0) / viewport_width;
    let width = viewport_width * scale + (CARD_PAD + CARD_BORDER) * 2.0;
    let columns = ((content_limit + GRID_GAP) / (width + GRID_GAP))
        .floor()
        .max(1.0)
        .min(2.0)
        .min(viewports.len() as f32);
    PanelGeometry {
        scale,
        width: columns * width + (columns - 1.0) * GRID_GAP + PANEL_PAD * 2.0 + SCROLLBAR,
        columns: columns as usize,
    }
}

pub fn panel(output: Output) -> Div {
    let output_name = output.name.clone();
    let size = (
        output.logical_size.width.max(1) as f32,
        output.logical_size.height.max(1) as f32,
    );
    let layouts = signal(LayoutCache::default());
    niri::on_event(move |event| {
        layouts.update(|cache| cache.apply_event(event));
    });
    // The shared tracker may have consumed the initial WindowsChanged before this panel opened.
    niri::request(json!("Windows")).then(move |result| {
        if let Ok(value) = result {
            layouts.update(|cache| cache.accept_snapshot(&value, &niri::windows()));
        }
    });
    let viewports = memo(move || {
        layouts.with(|cache| {
            viewport::project(
                &niri::workspaces(),
                &niri::windows(),
                cache,
                &output_name,
                size,
            )
        })
    });
    // Browsing belongs to this panel, not the compositor. Keep it across live card rebuilds.
    let browsing = signal(BrowseState::default());
    let max_panel_w = (size.0 - 48.0).max(280.0).min(1120.0);
    let panel_h = (size.1 - 48.0).max(320.0).min(780.0);
    let geometry = memo(move || viewports.with(|viewports| panel_geometry(viewports, max_panel_w)));
    div()
        .max_h(panel_h)
        .flex_none()
        .flex()
        .flex_col()
        .gap(16.0_f32)
        .p(PANEL_PAD)
        .rounded(30.0_f32)
        .shadows(elevation(3))
        .dyn_style(move |s| {
            let c = colors();
            s.w(geometry.get().width)
                .bg(c.surface_container)
                .text_color(c.on_surface)
        })
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(12.0_f32)
                .child(sym(icons::WORKSPACES, 24.0, |c| c.primary))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(static_txt(TextStyle::TITLE_LARGE, "Workspace overview"))
                        .child(
                            static_txt(TextStyle::BODY_SMALL, output.name)
                                .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                        ),
                )
                .child(
                    icon_btn(icons::CLOSE, 36.0, |c| c.on_surface_variant)
                        .on_click(|_| crate::panels::close()),
                ),
        )
        .child(show(available, move || {
            div()
                .flex_1()
                .min_h(0.0)
                .overflow_y_scroll()
                .child(dyn_view(move || {
                    let geometry = geometry.get();
                    let mut workspaces = viewports.get().into_iter().peekable();
                    let mut rows = div().flex().flex_col().gap(GRID_GAP).pr(SCROLLBAR);
                    while workspaces.peek().is_some() {
                        let mut row = div().flex().flex_row().gap(GRID_GAP);
                        for viewport in workspaces.by_ref().take(geometry.columns) {
                            row = row.child(workspace_card(viewport, geometry.scale, browsing));
                        }
                        rows = rows.child(row);
                    }
                    rows
                }))
        }))
        .child(show(
            || !available(),
            || {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(8.0_f32)
                    .child(sym(icons::WORKSPACES, 40.0, |c| c.on_surface_variant))
                    .child(static_txt(TextStyle::TITLE_MEDIUM, "Niri is not available"))
                    .child(
                        static_txt(TextStyle::BODY_MEDIUM, "Start the shell in a niri session.")
                            .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                    )
            },
        ))
}

fn workspace_card(viewport: WorkspaceViewport, scale: f32, browsing: Signal<BrowseState>) -> Div {
    let width = card_width(&viewport, scale);
    let workspace = viewport.workspace.clone();
    let title = workspace
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .map(|name| format!("{} · {name}", workspace.idx))
        .unwrap_or_else(|| format!("Workspace {}", workspace.idx));
    let active = workspace.is_active;
    let urgent = workspace.is_urgent;
    let count = format!(
        "{} window{}",
        viewport.window_count,
        if viewport.window_count == 1 { "" } else { "s" }
    );
    let canvas = workspace_canvas(viewport, scale, browsing);
    div()
        .w(width)
        .flex_none()
        .flex()
        .flex_col()
        .gap(10.0_f32)
        .p(CARD_PAD)
        .rounded(CARD_RADIUS)
        .border(CARD_BORDER)
        .dyn_style(move |s| {
            let c = colors();
            s.bg(if active {
                c.secondary_container.with_alpha(0.38)
            } else {
                c.surface_container_high
            })
            .border_color(if urgent {
                c.error
            } else {
                c.primary.with_alpha(if active { 1.0 } else { 0.0 })
            })
        })
        .child(pressable(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(8.0_f32)
                .child(static_txt(TextStyle::TITLE_SMALL, title).flex_1())
                .child(
                    static_txt(TextStyle::LABEL_SMALL, count)
                        .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                )
                .on_click(move |_| focus_workspace(workspace.id)),
            |c| c.on_surface,
        ))
        .child(canvas)
}

fn workspace_canvas(viewport: WorkspaceViewport, scale: f32, browsing: Signal<BrowseState>) -> Div {
    let size = (viewport.viewport_width, viewport.viewport_height);
    let viewport = Rc::new(viewport);
    let scrolling = Rc::clone(&viewport);
    let position = memo({
        let viewport = Rc::clone(&viewport);
        move || browsing.with(|state| state.position(&viewport))
    });
    div()
        .relative()
        .w(size.0 * scale)
        .h(size.1 * scale)
        .rounded(VIEWPORT_RADIUS)
        .overflow_hidden()
        .dyn_style(|s| s.bg(colors().surface_container_lowest))
        .on_scroll(move |event| {
            let delta = if event.delta_x.abs() > event.delta_y.abs() {
                event.delta_x
            } else {
                event.delta_y
            };
            if browsing.update_untracked(|state| state.scroll(&scrolling, delta / scale)) {
                browsing.notify();
            }
            // Do not scroll the surrounding workspace list, even at a viewport's edge.
            event.stop_propagation();
        })
        .child(dyn_view(move || {
            let mut content = div().size_full().relative();
            for tile in viewport.visible_windows(position.get()) {
                content = content.child(window_tile(tile, scale));
            }
            if viewport.window_count == 0 {
                content = content.child(
                    div()
                        .absolute()
                        .inset(0.0)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            static_txt(TextStyle::BODY_SMALL, "Empty workspace")
                                .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                        ),
                );
            }
            content
        }))
}

fn window_tile(tile: VisibleWindow<'_>, scale: f32) -> Div {
    let id = tile.window.id;
    let urgent = tile.window.is_urgent;
    let active = tile.active;
    let w = tile.width * scale;
    let h = tile.height * scale;
    let title = tile.window.title.clone();
    let mut content = div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(4.0_f32)
        .p(4.0_f32);
    if w >= 28.0 && h >= 28.0 {
        content = content.child(window_icon(tile.window, 22.0));
    }
    if w >= 100.0 && h >= 54.0 {
        content = content.child(
            static_txt(TextStyle::LABEL_SMALL, title)
                .max_w(w - 8.0)
                .overflow_hidden(),
        );
    }
    pressable(
        div()
            .absolute()
            .left(tile.x * scale)
            .top(tile.y * scale)
            .w(w)
            .h(h)
            .rounded(VIEWPORT_RADIUS)
            .overflow_hidden()
            .border(if active || urgent {
                CARD_BORDER
            } else {
                0.0_f32
            })
            .child(content)
            .dyn_style(move |s| {
                let c = colors();
                s.bg(if active {
                    c.primary_container
                } else if urgent {
                    c.error_container
                } else {
                    c.secondary_container
                })
                .border_color(if urgent { c.error } else { c.primary })
                .text_color(if active {
                    c.on_primary_container
                } else if urgent {
                    c.on_error_container
                } else {
                    c.on_secondary_container
                })
            })
            .on_click(move |_| focus_window(id)),
        |c| c.on_surface,
    )
}

fn window_icon(window: &Window, size: f32) -> View {
    if let Some(path) = app_id_icon(&window.app_id, 64) {
        img(path)
            .size(size)
            .object_fit(ObjectFit::Contain)
            .into_view()
    } else {
        let initial = window
            .app_id
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into());
        static_txt(TextStyle::TITLE_MEDIUM, initial).into_view()
    }
}

fn focus_workspace(id: u64) {
    drop(niri::action(json!({
        "FocusWorkspace": { "reference": { "Id": id } }
    })));
    crate::panels::close();
}

fn focus_window(id: u64) {
    drop(niri::action(json!({ "FocusWindow": { "id": id } })));
    crate::panels::close();
}

#[cfg(test)]
mod tests;
