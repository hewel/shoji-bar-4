use super::*;
use anti_webui::shell::niri::Workspace;

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

fn tiled(column: usize, row: usize, width: f32, height: f32) -> Layout {
    Layout {
        column,
        row,
        width,
        height,
        view_position: None,
    }
}

fn floating(x: f32, y: f32, width: f32, height: f32) -> Layout {
    Layout {
        column: 0,
        row: 0,
        width,
        height,
        view_position: Some((x, y)),
    }
}

fn geometry(
    viewport: &WorkspaceViewport,
    browsing: &BrowseState,
) -> Vec<(u64, f32, f32, f32, f32)> {
    viewport
        .visible_windows(browsing.position(viewport))
        .map(|tile| (tile.window.id, tile.x, tile.y, tile.width, tile.height))
        .collect()
}

#[test]
fn wheel_browsing_is_local_bounded_and_survives_card_rebuilds() {
    use anti_webui::core::{
        Ui,
        event::{Modifiers, ScrollPhase, ScrollSource, WindowEvent},
        geometry::{PointF, Scale, SizeI},
    };
    use std::cell::Cell;

    let slots = Cell::new(None);
    let bubbled = Rc::new(Cell::new(0));
    let outer_events = bubbled.clone();
    let layouts = HashMap::from([
        (1, tiled(1, 1, 1600.0, 900.0)),
        (2, tiled(2, 1, 1600.0, 900.0)),
    ]);
    let project = move |windows: &[Window]| {
        viewport::project(
            &[workspace(1, "DP-1"), workspace(2, "DP-1")],
            windows,
            &layouts,
            "DP-1",
            (1600.0, 900.0),
        )
    };
    let render_project = project.clone();
    let mut ui = Ui::new(|| {
        let browsing = signal(BrowseState::default());
        let windows = signal(vec![window(1, 1, false), window(2, 1, false)]);
        slots.set(Some((browsing, windows)));
        div()
            .w(500.0)
            .h(300.0)
            .on_scroll(move |_| outer_events.set(outer_events.get() + 1))
            .child(dyn_view(move || {
                let mut row = div().flex().flex_row().gap(8.0_f32);
                for viewport in windows.with(|windows| render_project(windows)) {
                    row = row.child(workspace_canvas(viewport, 0.125, browsing));
                }
                row
            }))
    });
    ui.handle_event(WindowEvent::Resized {
        size: SizeI::new(500, 300),
        scale: Scale::from_f64(1.0),
    });
    ui.update();
    let (browsing, windows) = slots.get().unwrap();
    let visible = || {
        let viewports = windows.with(|windows| project(windows));
        browsing.with(|state| {
            (
                geometry(&viewports[0], state),
                geometry(&viewports[1], state),
            )
        })
    };
    let scroll = |ui: &mut Ui, x: f32, y: f32, dx: f32, dy: f32| {
        ui.handle_event(WindowEvent::Scroll {
            position: PointF::new(x, y),
            delta_x: dx,
            delta_y: dy,
            modifiers: Modifiers::default(),
            source: ScrollSource::Unknown,
            phase: ScrollPhase::Single,
            time: 0,
        });
        ui.update();
    };
    scroll(&mut ui, 50.0, 50.0, 0.0, 20.0);
    let browsed = visible();
    assert_eq!(
        browsed.0,
        [(1, 0.0, 0.0, 1440.0, 900.0), (2, 1446.0, 0.0, 154.0, 900.0)]
    );
    assert!(browsed.1.is_empty());
    windows.update(|windows| windows[0].title = "Updated".into());
    ui.update();
    assert_eq!(visible(), browsed);
    scroll(&mut ui, 50.0, 50.0, 20.0, 0.0);
    assert_eq!(
        visible().0,
        [(1, 0.0, 0.0, 1280.0, 900.0), (2, 1286.0, 0.0, 314.0, 900.0)]
    );
    scroll(&mut ui, 50.0, 50.0, 0.0, 10000.0);
    assert_eq!(visible().0, [(2, 0.0, 0.0, 1600.0, 900.0)]);
    scroll(&mut ui, 50.0, 50.0, 0.0, -10000.0);
    assert_eq!(visible().0, [(1, 0.0, 0.0, 1600.0, 900.0)]);
    scroll(&mut ui, 250.0, 50.0, 0.0, 20.0);
    assert_eq!(bubbled.get(), 0);
    scroll(&mut ui, 450.0, 250.0, 0.0, 20.0);
    assert_eq!(bubbled.get(), 1);
}

#[test]
fn workspaces_are_ordered_by_index_not_niri_event_order() {
    let result = viewport::project(
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
            .map(|viewport| viewport.workspace.idx)
            .collect::<Vec<_>>(),
        [1, 3, 4]
    );
}

#[test]
fn viewport_scale_and_panel_size_do_not_depend_on_offscreen_windows() {
    let workspaces = [workspace(1, "DP-1"), workspace(2, "DP-1")];
    let pair = viewport::project(&workspaces, &[], &HashMap::new(), "DP-1", (1600.0, 900.0));
    let before = panel_geometry(&pair, 1120.0);
    let populated = viewport::project(
        &workspaces,
        &[window(1, 1, true)],
        &HashMap::from([(1, floating(12000.0, 0.0, 8000.0, 4000.0))]),
        "DP-1",
        (1600.0, 900.0),
    );
    assert_eq!(panel_geometry(&populated, 1120.0), before);
    assert_eq!(
        card_width(&pair[0], before.scale),
        card_width(&populated[0], before.scale)
    );
    let narrow = panel_geometry(&populated, 280.0);
    assert!(narrow.width <= 280.0);
    assert!(narrow.scale > 0.0 && narrow.scale < before.scale);
}

#[test]
fn viewport_clips_partial_windows_and_excludes_outside_hit_targets() {
    let layouts = HashMap::from([
        (1, floating(-50.0, -50.0, 400.0, 300.0)),
        (2, floating(1500.0, 800.0, 400.0, 300.0)),
        (3, floating(-400.0, 0.0, 400.0, 300.0)),
        (4, floating(1600.0, 0.0, 400.0, 300.0)),
        (5, floating(0.0, -300.0, 400.0, 300.0)),
        (6, floating(0.0, 900.0, 400.0, 300.0)),
    ]);
    let windows: Vec<_> = (1..=6).map(|id| window(id, 1, true)).collect();
    let viewports = viewport::project(
        &[workspace(1, "DP-1")],
        &windows,
        &layouts,
        "DP-1",
        (1600.0, 900.0),
    );
    assert_eq!(
        geometry(&viewports[0], &BrowseState::default()),
        [
            (1, 0.0, 0.0, 350.0, 250.0),
            (2, 1500.0, 800.0, 100.0, 100.0),
        ]
    );
}

#[test]
fn projection_keeps_columns_floating_positions_and_active_windows() {
    let layouts = HashMap::from([
        (1, tiled(1, 1, 800.0, 900.0)),
        (2, tiled(1, 2, 800.0, 900.0)),
        (3, tiled(2, 1, 1000.0, 900.0)),
        (4, floating(120.0, 80.0, 400.0, 300.0)),
    ]);
    let windows = [
        window(1, 1, false),
        window(2, 1, false),
        window(3, 1, false),
        window(4, 1, true),
        window(5, 2, false),
    ];
    let viewports = viewport::project(
        &[workspace(1, "DP-1"), workspace(2, "DP-2")],
        &windows,
        &layouts,
        "DP-1",
        (1600.0, 900.0),
    );
    assert_eq!(viewports.len(), 1);
    let viewport = &viewports[0];
    let browsing = BrowseState::default();
    assert_eq!(viewport.window_count, 4);
    assert_eq!(
        geometry(viewport, &browsing),
        [
            (1, 0.0, 0.0, 594.0, 900.0),
            (3, 600.0, 0.0, 1000.0, 900.0),
            (4, 120.0, 80.0, 400.0, 300.0),
        ]
    );
    assert_eq!(
        viewport
            .visible_windows(browsing.position(viewport))
            .filter(|tile| tile.active)
            .map(|tile| tile.window.id)
            .collect::<Vec<_>>(),
        [3]
    );
}

#[test]
fn browsing_tracks_changed_bounds_without_losing_panel_local_position() {
    let layouts = HashMap::from([
        (1, tiled(1, 1, 1600.0, 900.0)),
        (2, tiled(2, 1, 1600.0, 900.0)),
        (3, floating(-120.0, 80.0, 400.0, 300.0)),
    ]);
    let windows = [window(1, 1, false), window(2, 1, false), window(3, 1, true)];
    let workspaces = [workspace(1, "DP-1")];
    let original = viewport::project(&workspaces, &windows, &layouts, "DP-1", (1600.0, 900.0));
    let mut browsing = BrowseState::default();
    assert!(browsing.scroll(&original[0], -10000.0));
    assert_eq!(
        geometry(&original[0], &browsing),
        [(1, 120.0, 0.0, 1480.0, 900.0), (3, 0.0, 80.0, 400.0, 300.0)]
    );
    assert!(!browsing.scroll(&original[0], -1.0));
    browsing.scroll(&original[0], 10000.0);
    assert_eq!(
        geometry(&original[0], &browsing),
        [(2, 0.0, 0.0, 1600.0, 900.0)]
    );
    let reduced = viewport::project(
        &workspaces,
        &[windows[0].clone(), windows[2].clone()],
        &layouts,
        "DP-1",
        (1600.0, 900.0),
    );
    assert_eq!(
        geometry(&reduced[0], &browsing),
        [(1, 0.0, 0.0, 1600.0, 900.0), (3, 0.0, 80.0, 280.0, 300.0)]
    );
    // Clamping a rebuilt view does not overwrite the user's saved browsing position.
    assert_eq!(
        geometry(&original[0], &browsing),
        [(2, 0.0, 0.0, 1600.0, 900.0)]
    );
    assert_eq!(
        geometry(&original[0], &BrowseState::default()),
        [(1, 0.0, 0.0, 1600.0, 900.0), (3, 0.0, 80.0, 280.0, 300.0)]
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
            "id": 7, "is_floating": true,
            "layout": {"pos_in_scrolling_layout": null, "tile_size": [400, 300], "tile_pos_in_workspace_view": [120, 80]}
        }}}),
    );
    assert_eq!(layouts[&7].view_position, Some((120.0, 80.0)));
    assert_eq!((layouts[&7].width, layouts[&7].height), (400.0, 300.0));
}

#[test]
fn initial_response_does_not_replace_newer_events_or_closed_windows() {
    let old = tiled(1, 1, 800.0, 900.0);
    let new = Layout { column: 3, ..old };
    let mut current = HashMap::from([(1, new), (2, old)]);
    merge_initial_layouts(
        &mut current,
        HashMap::from([(1, old), (3, old)]),
        &HashSet::from([1, 3]),
    );
    assert_eq!(current, HashMap::from([(1, new), (3, old)]));
}
