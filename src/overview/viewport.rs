//! Workspace projection and panel-local browsing in output logical coordinates.

use super::layouts::{Layout, LayoutCache};
use anti_webui::shell::niri::{Window, Workspace};
use std::collections::{BTreeMap, HashMap};

const GAP: f32 = 6.0;

#[derive(Clone, Debug, PartialEq)]
struct Tile {
    window: Window,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorkspaceViewport {
    pub workspace: Workspace,
    pub window_count: usize,
    pub viewport_width: f32,
    pub viewport_height: f32,
    tiles: Vec<Tile>,
    initial_x: f32,
    min_x: f32,
    max_x: f32,
}

/// Clipped drawing and hit geometry; the source window is borrowed, not copied.
pub(super) struct VisibleWindow<'a> {
    pub window: &'a Window,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub active: bool,
}

/// Owned by the panel so browsing survives projection rebuilds, but not reopening.
#[derive(Default)]
pub(super) struct BrowseState {
    offsets: HashMap<u64, f32>,
}

/// A resolved view origin, comparable so unchanged workspaces need not redraw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ViewPosition(f32);

impl BrowseState {
    pub fn position(&self, viewport: &WorkspaceViewport) -> ViewPosition {
        ViewPosition(
            self.offsets
                .get(&viewport.workspace.id)
                .copied()
                .unwrap_or(viewport.initial_x)
                .clamp(viewport.min_x, viewport.max_x),
        )
    }

    /// Returns whether the effective browsing position changed.
    pub fn scroll(&mut self, viewport: &WorkspaceViewport, delta: f32) -> bool {
        let current = self.position(viewport).0;
        let next = (current + delta).clamp(viewport.min_x, viewport.max_x);
        if next == current {
            return false;
        }
        self.offsets.insert(viewport.workspace.id, next);
        true
    }
}

impl WorkspaceViewport {
    pub fn visible_windows<'a>(
        &'a self,
        position: ViewPosition,
    ) -> impl Iterator<Item = VisibleWindow<'a>> + 'a {
        self.tiles.iter().filter_map(move |tile| {
            let x = tile.x - position.0;
            let right = (x + tile.width).min(self.viewport_width);
            let bottom = (tile.y + tile.height).min(self.viewport_height);
            let left = x.max(0.0);
            let top = tile.y.max(0.0);
            (right > left && bottom > top).then_some(VisibleWindow {
                window: &tile.window,
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
                active: tile.window.is_focused
                    || Some(tile.window.id) == self.workspace.active_window_id,
            })
        })
    }
}

pub(super) fn project(
    workspaces: &[Workspace],
    windows: &[Window],
    layouts: &LayoutCache,
    output: &str,
    output_size: (f32, f32),
) -> Vec<WorkspaceViewport> {
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
        let mut columns = BTreeMap::<usize, Vec<(&Window, Layout)>>::new();
        for &window in assigned.iter().filter(|window| !window.is_floating) {
            let Some(layout) = layouts.get(window.id) else {
                continue;
            };
            columns
                .entry(layout.column)
                .or_default()
                .push((window, layout));
        }

        let mut tiles = Vec::new();
        let mut x = 0.0;
        for entries in columns.values_mut() {
            entries.sort_by_key(|(_, layout)| layout.row);
            let column_w = entries
                .iter()
                .map(|(_, layout)| layout.width)
                .fold(0.0, f32::max);
            let mut y = 0.0;
            for &(window, layout) in entries.iter() {
                tiles.push(Tile {
                    window: window.clone(),
                    x,
                    y,
                    width: layout.width,
                    height: layout.height,
                });
                y += layout.height + GAP;
            }
            x += column_w + GAP;
        }
        let content_width = if tiles.is_empty() {
            screen_w
        } else {
            (x - GAP).max(screen_w)
        };
        let focus = tiles.iter().position(|tile| {
            tile.window.is_focused || Some(tile.window.id) == workspace.active_window_id
        });
        let initial_x = focus
            .map(|i| tiles[i].x + tiles[i].width / 2.0 - screen_w / 2.0)
            .unwrap_or(0.0)
            .clamp(0.0, (content_width - screen_w).max(0.0));
        for window in assigned.into_iter().filter(|window| window.is_floating) {
            let Some(layout) = layouts.get(window.id) else {
                continue;
            };
            // Floating positions are relative to the estimated initial workspace view.
            let Some((x, y)) = layout.view_position else {
                continue;
            };
            tiles.push(Tile {
                window: window.clone(),
                x: initial_x + x,
                y,
                width: layout.width,
                height: layout.height,
            });
        }
        let min_x = tiles.iter().map(|tile| tile.x).fold(0.0, f32::min);
        let max_x = (tiles
            .iter()
            .map(|tile| tile.x + tile.width)
            .fold(screen_w, f32::max)
            - screen_w)
            .max(min_x);
        result.push(WorkspaceViewport {
            workspace: workspace.clone(),
            window_count,
            viewport_width: screen_w,
            viewport_height: screen_h,
            tiles,
            initial_x,
            min_x,
            max_x,
        });
    }
    result.sort_by_key(|viewport| viewport.workspace.idx);
    result
}
