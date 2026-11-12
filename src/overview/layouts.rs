//! The layout details absent from the shared Niri window tracker.

use anti_webui::shell::niri::Window;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layout {
    pub column: usize,
    pub row: usize,
    pub width: f32,
    pub height: f32,
    pub view_position: Option<(f32, f32)>,
}

#[derive(Clone, Default)]
pub(super) struct LayoutCache {
    entries: HashMap<u64, Layout>,
}

impl LayoutCache {
    pub fn get(&self, id: u64) -> Option<Layout> {
        self.entries.get(&id).copied()
    }

    /// Merge the opening request against events already received and the tracker's live windows.
    pub fn accept_snapshot(&mut self, response: &Value, live_windows: &[Window]) {
        let Some(windows) = response.get("Windows").and_then(Value::as_array) else {
            return;
        };
        let initial = layouts_of(windows);
        let live: HashSet<_> = live_windows.iter().map(|window| window.id).collect();
        self.entries.retain(|id, _| live.contains(id));
        for (id, layout) in initial {
            if live.contains(&id) {
                // An event received after the request is newer than the response.
                self.entries.entry(id).or_insert(layout);
            }
        }
    }

    pub fn apply_event(&mut self, event: &Value) {
        if let Some(windows) = event
            .get("WindowsChanged")
            .and_then(|e| e.get("windows"))
            .and_then(Value::as_array)
        {
            self.entries = layouts_of(windows);
        } else if let Some(window) = event
            .get("WindowOpenedOrChanged")
            .and_then(|e| e.get("window"))
        {
            let Some(id) = window.get("id").and_then(Value::as_u64) else {
                return;
            };
            match layout_of(window) {
                Some(layout) => self.entries.insert(id, layout),
                None => self.entries.remove(&id),
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
                    self.entries.insert(id, layout);
                }
            }
        } else if let Some(id) = event
            .get("WindowClosed")
            .and_then(|e| e.get("id"))
            .and_then(Value::as_u64)
        {
            self.entries.remove(&id);
        }
    }
}

fn layouts_of(windows: &[Value]) -> HashMap<u64, Layout> {
    windows
        .iter()
        .filter_map(|window| Some((window.get("id")?.as_u64()?, layout_of(window)?)))
        .collect()
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
