//! A workspace overview for niri: each output's workspaces as live ribbons of their windows.
//!
//! The geometry comes from niri's event stream. The UI owns no compositor state of its own beyond
//! the layout details that AntiWebUI's general window model does not retain.

use crate::appinfo::app_id_icon;
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui::shell::niri::{self, Window, Workspace};
use anti_webui::shell::prelude::*;
use anti_webui_md3e::icons;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

const GAP: f32 = 6.0;
const CARD_PAD: f32 = 14.0;
const GRID_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    column: usize,
    row: usize,
    width: f32,
    height: f32,
    view_position: Option<(f32, f32)>,
}

#[derive(Clone, Debug, PartialEq)]
struct Tile {
    window: Window,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Ribbon {
    workspace: Workspace,
    tiles: Vec<Tile>,
    window_count: usize,
    width: f32,
    height: f32,
    viewport_x: f32,
    viewport_width: f32,
    viewport_height: f32,
}

fn layout_of(window: &Value) -> Option<Layout> {
    let layout = window.get("layout").unwrap_or(window);
    let position = layout
        .get("pos_in_scrolling_layout")
        .and_then(Value::as_array);
    let view_position = layout
        .get("tile_pos_in_workspace_view")
        .and_then(Value::as_array);
    let size = layout.get("tile_size")?.as_array()?;
    let (column, row) = match position {
        Some(position) => (
            position.first()?.as_u64()? as usize,
            position.get(1)?.as_u64()? as usize,
        ),
        None => (0, 0),
    };
    let view_position = match view_position {
        Some(position) => Some((
            position.first()?.as_f64()? as f32,
            position.get(1)?.as_f64()? as f32,
        )),
        None => None,
    };
    let layout = Layout {
        column,
        row,
        width: size.first()?.as_f64()? as f32,
        height: size.get(1)?.as_f64()? as f32,
        view_position,
    };
    (layout.width > 0.0
        && layout.height > 0.0
        && ((column > 0 && row > 0) || view_position.is_some()))
    .then_some(layout)
}

fn layouts_of(windows: &[Value]) -> HashMap<u64, Layout> {
    windows
        .iter()
        .filter_map(|window| Some((window.get("id")?.as_u64()?, layout_of(window)?)))
        .collect()
}

fn merge_initial_layouts(
    current: &mut HashMap<u64, Layout>,
    initial: HashMap<u64, Layout>,
    live: &HashSet<u64>,
) {
    current.retain(|id, _| live.contains(id));
    for (id, layout) in initial {
        if live.contains(&id) {
            // An event received after the request is newer than the response.
            current.entry(id).or_insert(layout);
        }
    }
}

fn apply_layout_event(layouts: &mut HashMap<u64, Layout>, event: &Value) {
    if let Some(windows) = event
        .get("WindowsChanged")
        .and_then(|e| e.get("windows"))
        .and_then(Value::as_array)
    {
        *layouts = layouts_of(windows);
    } else if let Some(window) = event
        .get("WindowOpenedOrChanged")
        .and_then(|e| e.get("window"))
    {
        let Some(id) = window.get("id").and_then(Value::as_u64) else {
            return;
        };
        match layout_of(window) {
            Some(layout) => layouts.insert(id, layout),
            None => layouts.remove(&id),
        };
    } else if let Some(changes) = event
        .get("WindowLayoutsChanged")
        .and_then(|e| e.get("changes"))
        .and_then(Value::as_array)
    {
        for change in changes {
            let Some(pair) = change.as_array() else {
                continue;
            };
            let Some(id) = pair.first().and_then(Value::as_u64) else {
                continue;
            };
            if let Some(layout) = pair.get(1).and_then(layout_of) {
                layouts.insert(id, layout);
            }
        }
    } else if let Some(id) = event
        .get("WindowClosed")
        .and_then(|e| e.get("id"))
        .and_then(Value::as_u64)
    {
        layouts.remove(&id);
    }
}

fn ribbons(
    workspaces: &[Workspace],
    windows: &[Window],
    layouts: &HashMap<u64, Layout>,
    output: &str,
    output_size: (f32, f32),
) -> Vec<Ribbon> {
    let (screen_w, screen_h) = output_size;
    let mut result = Vec::new();
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.output.as_deref() == Some(output))
    {
        let assigned: Vec<&Window> = windows
            .iter()
            .filter(|window| window.workspace_id == Some(workspace.id))
            .collect();
        let window_count = assigned.len();
        let mut columns = BTreeMap::<usize, Vec<(Window, Layout)>>::new();
        for window in assigned.into_iter().filter(|window| !window.is_floating) {
            let Some(layout) = layouts.get(&window.id).copied() else {
                continue;
            };
            columns
                .entry(layout.column)
                .or_default()
                .push((window.clone(), layout));
        }

        let mut tiles = Vec::new();
        let mut x = 0.0;
        let mut ribbon_h: f32 = 0.0;
        for entries in columns.values_mut() {
            entries.sort_by_key(|(_, layout)| layout.row);
            let column_w = entries
                .iter()
                .map(|(_, layout)| layout.width)
                .fold(0.0, f32::max);
            let mut y = 0.0;
            for (window, layout) in entries {
                tiles.push(Tile {
                    window: window.clone(),
                    x,
                    y,
                    width: layout.width,
                    height: layout.height,
                    visible: false,
                });
                y += layout.height + GAP;
            }
            ribbon_h = ribbon_h.max((y - GAP).max(0.0));
            x += column_w + GAP;
        }
        let ribbon_w = if tiles.is_empty() {
            screen_w
        } else {
            (x - GAP).max(screen_w)
        };
        let ribbon_h = if tiles.is_empty() {
            screen_h
        } else {
            ribbon_h.max(screen_h)
        };
        let focus = tiles.iter().position(|tile| {
            tile.window.is_focused || Some(tile.window.id) == workspace.active_window_id
        });
        let viewport_x = focus
            .map(|i| tiles[i].x + tiles[i].width / 2.0 - screen_w / 2.0)
            .unwrap_or(0.0)
            .clamp(0.0, (ribbon_w - screen_w).max(0.0));
        for window in windows
            .iter()
            .filter(|window| window.workspace_id == Some(workspace.id) && window.is_floating)
        {
            let Some(layout) = layouts.get(&window.id) else {
                continue;
            };
            // Niri reports floating positions in the current workspace view. Put them into the
            // same ribbon coordinates as the reconstructed tiled columns.
            let Some((x, y)) = layout.view_position else {
                continue;
            };
            tiles.push(Tile {
                window: window.clone(),
                x: viewport_x + x,
                y,
                width: layout.width,
                height: layout.height,
                visible: false,
            });
        }
        for tile in &mut tiles {
            tile.visible = tile.x + tile.width > viewport_x && tile.x < viewport_x + screen_w;
        }
        result.push(Ribbon {
            workspace: workspace.clone(),
            tiles,
            window_count,
            width: ribbon_w,
            height: ribbon_h,
            viewport_x,
            viewport_width: screen_w,
            viewport_height: screen_h,
        });
    }
    result.sort_by_key(|ribbon| ribbon.workspace.idx);
    result
}

pub fn available() -> bool {
    niri::available()
}

pub fn summary() -> String {
    let workspaces = niri::workspaces().len();
    let windows = niri::windows().len();
    format!(
        "{windows} {} · {workspaces} {}",
        if windows == 1 { "window" } else { "windows" },
        if workspaces == 1 {
            "workspace"
        } else {
            "workspaces"
        }
    )
}

pub fn panel(output: Output) -> Div {
    let output_name = output.name.clone();
    let size = (
        output.logical_size.width.max(1) as f32,
        output.logical_size.height.max(1) as f32,
    );
    let layouts = signal(HashMap::new());
    niri::on_event(move |event| {
        layouts.update(|layouts| apply_layout_event(layouts, event));
    });
    // The shared tracker may have consumed the initial WindowsChanged before this panel opened.
    niri::request(json!("Windows")).then(move |result| {
        if let Ok(value) = result
            && let Some(windows) = value.get("Windows").and_then(Value::as_array)
        {
            let initial = layouts_of(windows);
            let live = niri::windows().into_iter().map(|w| w.id).collect();
            layouts.update(|current| merge_initial_layouts(current, initial, &live));
        }
    });
    let ribbons = memo(move || {
        ribbons(
            &niri::workspaces(),
            &niri::windows(),
            &layouts.get(),
            &output_name,
            size,
        )
    });
    let panel_w = (size.0 - 48.0).max(280.0).min(1120.0);
    let panel_h = (size.1 - 48.0).max(320.0).min(780.0);
    let columns = if panel_w >= 960.0 { 2.0 } else { 1.0 };
    let card_w = (panel_w - 42.0 - (columns - 1.0) * GRID_GAP) / columns;
    let workspace_ids = memo({
        let ribbons = ribbons.clone();
        move || {
            ribbons
                .get()
                .iter()
                .map(|ribbon| ribbon.workspace.id)
                .collect::<Vec<_>>()
        }
    });
    div()
        .w(panel_w)
        .max_h(panel_h)
        .flex()
        .flex_col()
        .gap(16.0_f32)
        .p(18.0_f32)
        .rounded(30.0_f32)
        .shadows(elevation(3))
        .dyn_style(|s| {
            let c = colors();
            s.bg(c.surface_container).text_color(c.on_surface)
        })
        .child(
            div()
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
            div().flex_1().min_h(0.0).overflow_y_scroll().child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .content_start()
                    .gap(GRID_GAP)
                    .pr(6.0_f32)
                    .child(
                        for_each(
                            move || workspace_ids.get(),
                            |id| *id,
                            move |id| {
                                let ribbon = memo({
                                    let ribbons = ribbons.clone();
                                    move || {
                                        ribbons
                                            .get()
                                            .into_iter()
                                            .find(|ribbon| ribbon.workspace.id == id)
                                    }
                                });
                                dyn_view(move || match ribbon.get() {
                                    Some(ribbon) => workspace_card(ribbon, card_w).into_view(),
                                    None => div().into_view(),
                                })
                            },
                        )
                        .moves(emphasized(240)),
                    ),
            )
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

fn workspace_card(ribbon: Ribbon, width: f32) -> Div {
    let workspace = ribbon.workspace.clone();
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
        ribbon.window_count,
        if ribbon.window_count == 1 { "" } else { "s" }
    );
    let canvas_w = width - CARD_PAD * 2.0;
    let canvas_h =
        (canvas_w * ribbon.viewport_height / ribbon.viewport_width.max(1.0)).clamp(112.0, 184.0);
    let scale = (canvas_w / ribbon.width.max(1.0)).min(canvas_h / ribbon.height.max(1.0));
    let offset_x = (canvas_w - ribbon.width * scale) / 2.0;
    let offset_y = (canvas_h - ribbon.height * scale) / 2.0;
    let mut canvas = div()
        .relative()
        .w(canvas_w)
        .h(canvas_h)
        .rounded(16.0_f32)
        .overflow_hidden()
        .dyn_style(|s| s.bg(colors().surface_container_lowest));
    for tile in ribbon.tiles.clone() {
        canvas = canvas.child(window_tile(tile, scale, offset_x, offset_y));
    }
    canvas = canvas.child(
        div()
            .absolute()
            .ml(offset_x + ribbon.viewport_x * scale)
            .mt(offset_y)
            .w(ribbon.viewport_width * scale)
            .h((ribbon.viewport_height * scale).min(canvas_h))
            .rounded(12.0_f32)
            .border(1.5_f32)
            .pointer_events_none()
            .dyn_style(|s| s.border_color(colors().primary.with_alpha(0.75))),
    );
    if ribbon.tiles.is_empty() {
        canvas = canvas.child(
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
    div()
        .w(width)
        .flex_none()
        .flex()
        .flex_col()
        .gap(10.0_f32)
        .p(CARD_PAD)
        .rounded(24.0_f32)
        .border(if active { 1.5_f32 } else { 0.0_f32 })
        .dyn_style(move |s| {
            let c = colors();
            s.bg(if active {
                c.secondary_container.with_alpha(0.38)
            } else {
                c.surface_container_high
            })
            .border_color(if urgent { c.error } else { c.primary })
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

fn window_tile(tile: Tile, scale: f32, offset_x: f32, offset_y: f32) -> Div {
    let id = tile.window.id;
    let focused = tile.window.is_focused;
    let urgent = tile.window.is_urgent;
    let visible = tile.visible;
    let w = (tile.width * scale - 1.0).max(2.0);
    let h = (tile.height * scale - 1.0).max(2.0);
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
        content = content.child(window_icon(&tile.window, 22.0));
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
            .ml(offset_x + tile.x * scale)
            .mt(offset_y + tile.y * scale)
            .w(w)
            .h(h)
            .rounded(10.0_f32)
            .border(if focused || urgent { 1.5_f32 } else { 0.0_f32 })
            .child(content)
            .dyn_style(move |s| {
                let c = colors();
                s.bg(if focused {
                    c.primary_container
                } else if urgent {
                    c.error_container
                } else if visible {
                    c.secondary_container
                } else {
                    c.surface_container_high
                })
                .border_color(if urgent { c.error } else { c.primary })
                .opacity(if visible || focused { 1.0_f32 } else { 0.5_f32 })
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
mod tests {
    use super::*;

    fn workspace(id: u64, output: &str) -> Workspace {
        Workspace {
            id,
            idx: id as u32,
            output: Some(output.to_owned()),
            is_active: id == 1,
            active_window_id: Some(3),
            ..Default::default()
        }
    }

    fn window(id: u64, workspace: u64, floating: bool) -> Window {
        Window {
            id,
            workspace_id: Some(workspace),
            is_floating: floating,
            ..Default::default()
        }
    }

    #[test]
    fn workspaces_are_ordered_by_index_not_niri_event_order() {
        let result = ribbons(
            &[
                workspace(4, "DP-1"),
                workspace(1, "DP-1"),
                workspace(3, "DP-1"),
            ],
            &[],
            &HashMap::new(),
            "DP-1",
            (1600.0, 900.0),
        );
        assert_eq!(
            result
                .iter()
                .map(|ribbon| ribbon.workspace.idx)
                .collect::<Vec<_>>(),
            [1, 3, 4]
        );
    }

    #[test]
    fn ribbons_keep_niri_columns_and_their_real_proportions() {
        let layouts = HashMap::from([
            (
                1,
                Layout {
                    column: 1,
                    row: 1,
                    width: 800.0,
                    height: 900.0,
                    view_position: None,
                },
            ),
            (
                2,
                Layout {
                    column: 1,
                    row: 2,
                    width: 800.0,
                    height: 900.0,
                    view_position: None,
                },
            ),
            (
                3,
                Layout {
                    column: 2,
                    row: 1,
                    width: 1000.0,
                    height: 900.0,
                    view_position: None,
                },
            ),
            (
                4,
                Layout {
                    column: 0,
                    row: 0,
                    width: 400.0,
                    height: 300.0,
                    view_position: Some((120.0, 80.0)),
                },
            ),
        ]);
        let windows = vec![
            window(1, 1, false),
            window(2, 1, false),
            window(3, 1, false),
            window(4, 1, true),
            window(5, 2, false),
        ];
        let result = ribbons(
            &[workspace(1, "DP-1"), workspace(2, "DP-2")],
            &windows,
            &layouts,
            "DP-1",
            (1600.0, 900.0),
        );
        assert_eq!(result.len(), 1);
        let ribbon = &result[0];
        assert_eq!(ribbon.window_count, 4);
        assert_eq!(
            ribbon
                .tiles
                .iter()
                .map(|t| (t.window.id, t.x, t.y, t.width))
                .collect::<Vec<_>>(),
            [
                (1, 0.0, 0.0, 800.0),
                (2, 0.0, 906.0, 800.0),
                (3, 806.0, 0.0, 1000.0),
                (4, 326.0, 80.0, 400.0),
            ]
        );
        assert_eq!((ribbon.width, ribbon.height), (1806.0, 1806.0));
        assert_eq!(ribbon.viewport_x, 206.0);
        assert_eq!(
            ribbon.tiles.iter().map(|t| t.visible).collect::<Vec<_>>(),
            [true, true, true, true]
        );
    }

    #[test]
    fn layout_events_replace_initial_state_and_follow_window_changes() {
        let initial = json!({"WindowsChanged": {"windows": [
            {"id": 1, "layout": {"pos_in_scrolling_layout": [1, 1], "tile_size": [800, 900]}},
            {"id": 2, "layout": {"pos_in_scrolling_layout": [2, 1], "tile_size": [800, 900]}}
        ]}});
        let mut layouts = HashMap::new();
        apply_layout_event(&mut layouts, &initial);
        assert_eq!(layouts.len(), 2);
        apply_layout_event(
            &mut layouts,
            &json!({"WindowLayoutsChanged": {"changes": [
                [1, {"pos_in_scrolling_layout": [3, 2], "tile_size": [640, 480]}]
            ]}}),
        );
        assert_eq!(layouts[&1].column, 3);
        assert_eq!(layouts[&1].height, 480.0);
        apply_layout_event(
            &mut layouts,
            &json!({"WindowOpenedOrChanged": {"window": {"id": 2, "is_floating": true}}}),
        );
        apply_layout_event(&mut layouts, &json!({"WindowClosed": {"id": 1}}));
        assert!(layouts.is_empty());
    }

    #[test]
    fn floating_layout_events_keep_compositor_positions() {
        let mut layouts = HashMap::new();
        apply_layout_event(
            &mut layouts,
            &json!({"WindowOpenedOrChanged": {"window": {
                "id": 7,
                "is_floating": true,
                "layout": {
                    "pos_in_scrolling_layout": null,
                    "tile_size": [400, 300],
                    "tile_pos_in_workspace_view": [120, 80]
                }
            }}}),
        );
        assert_eq!(layouts[&7].view_position, Some((120.0, 80.0)));
        assert_eq!((layouts[&7].width, layouts[&7].height), (400.0, 300.0));
    }

    #[test]
    fn initial_response_does_not_replace_newer_events_or_closed_windows() {
        let old = Layout {
            column: 1,
            row: 1,
            width: 800.0,
            height: 900.0,
            view_position: None,
        };
        let new = Layout { column: 3, ..old };
        let mut current = HashMap::from([(1, new), (2, old)]);
        merge_initial_layouts(
            &mut current,
            HashMap::from([(1, old), (3, old)]),
            &HashSet::from([1, 3]),
        );
        assert_eq!(current, HashMap::from([(1, new), (3, old)]));
    }
}
