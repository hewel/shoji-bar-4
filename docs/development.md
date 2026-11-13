# Development

## AntiWebUI

AntiWebUI comes from GitHub's `main`, and `Cargo.lock` pins the commit used. To move to a newer AntiWebUI:

```sh
cargo update -p anti-webui -p anti-webui-md3e
```

To work on a local AntiWebUI checkout at the same time, patch it in from an uncommitted `.cargo/config.toml` (it is in `.gitignore`):

```toml
[patch."https://github.com/bea4dev/AntiWebUI.git"]
anti-webui = { path = "/path/to/AntiWebUI/src/anti-webui" }
anti-webui-md3e = { path = "/path/to/AntiWebUI/src/anti-webui-md3e" }
```

The `dynamic_linking` feature links AntiWebUI dynamically, for faster rebuilds while developing.

## Offscreen previews

Any part of the shell can be drawn to a PNG without a compositor surface, with a simulated pointer and clicks:

```sh
shoji-bar-4 preview dock out.png --pointer 470,110
shoji-bar-4 preview quick-settings out.png --width 460 --height 900 --scale 1 --light
shoji-bar-4 preview workspaces out.png --ms 0 --frames 40 --every 30   # out-0.png … out-40.png
shoji-bar-4 preview quick-settings out.png --click 1548,80 --click-frames 1 --ms 0 --frames 8 --every 40
shoji-bar-4 preview bar left.png --bar-position left
shoji-bar-4 preview quick-settings right.png --bar-position right --scale 1
```

- **Parts**: `bar`, `dock`, `workspaces`, `title`, `quick-settings`, `calendar`, `launcher`, `clipboard`, `media`, `power`, `overview`, `lock` and `notifications`.
  - In `lock`, the password is `shoji`, and PAM is not used.
  - `title` is the window island with a made-up window whose title goes between a long one and a short one every `--switch` ms (80 by default), from 1.2 s for a second and a half, and then stays on the one `--end` names (`long` or `short`). With `--windows`, the two titles belong to two windows that take the keyboard in turn. With `--spinner`, the title is a terminal's with a spinner in front, as Claude Code's is, which at the end changes to another title.
- **Settings**: `preview` doesn't save them. `--bar-position top|left|right` selects the bar edge (top by default), including for panel and component previews. `--output NAME` selects the output instead of the first one.
- **Real services**: the previews act on them.
  - A click on a network, Bluetooth or audio row really changes it.
  - `clipboard` previews use an isolated in-memory history: new copies arrive live, Enter changes the real clipboard, and Delete/pinning only affect the preview's history. They never open the persistent archive or access its encryption key.
  - Window and workspace clicks in `overview` really change Niri's focus.
- **`--click X,Y`**: a click.
- **`--right-click X,Y`**: a right click.
- **`--drag X1,Y1:X2,Y2`**: after the clicks, presses at the first point, moves to the second and lets go.
- **`--key K`**: after the clicks, presses a key (`ArrowDown`, `ArrowUp`, `Enter`, `Delete`, `Escape`), or types K if it is a character. It can be given more than once.
- **`--click-frames N`**: how many frames to run after a click (20 by default). Make it small to catch an animation a click starts from its beginning.

## Idle and lock lifecycle tests

`src/idle_coordinator.rs` owns sequencing across inactivity, lock acknowledgement, sleep and recovery. `src/idle_runtime.rs` supplies two adapters behind that seam: the AntiWebUI/Wayland/logind desktop implementation and the scenario implementation in `src/idle_runtime/tests.rs`. Tests drive the shared runtime through input events and assert platform observations; they do not inspect the internal rule policy. This covers activity or media playback during lock waits, fresh countdowns after playback, independent media/Caffeine/application inhibition, explicit Sleep during playback, refusal, stale deadlines and sleep-delay release without touching the session.

`src/display_power.rs` owns ordered screen-power requests, cancellation and shutdown restoration. Its tests use a worker-controlled adapter, plus a socket-backed compositor for the Niri protocol and saturated-backlog cases. `src/lock.rs` owns sleep holds, PAM answer generations and unlock suppression; tests exercise that lifecycle without opening compositor surfaces.

```sh
cargo test --locked --bin shoji-bar-4 idle
cargo test --locked --bin shoji-bar-4 lock::tests
```

## Layer lifecycle tests

`src/shell_layer.rs` owns shell-surface creation, output binding, placement-driven reconfiguration and closing. Production uses its Wayland adapter; unit tests use the recording adapter through the same `open_bar` and `install_panels` interface. This covers multi-output assignment, unrelated-setting isolation, in-place placement updates and panel input changes without a compositor.

Bars reserve their fixed thickness from the first surface commit, and update that reservation before changing edges. The reservation regression exercises the production Wayland adapter's opening options and queued layer changes with AntiWebUI's real exclusive-zone calculation. It checks top/left/right startup and repeated edge changes with both delayed and immediate configure timing, including valid stretch anchors. It does not map surfaces into a live compositor or assert Niri's viewport recovery.

```sh
cargo test --locked --bin shoji-bar-4 shell_layer::tests
```

## Overview tests

`src/overview/viewport.rs` owns workspace projection, panel-local browsing positions, range clamping and clipped drawing/hit geometry. `src/overview.rs` owns rendering, pixel scaling and event propagation; unchanged workspace positions do not invalidate their canvases. The tests in `src/overview/tests.rs` exercise the same projection and browsing interface as the panel, including window changes after scrolling, and retain a real `Ui` test for wheel propagation and card rebuilds.

`src/overview/layouts.rs` owns the layout cache, event decoding and opening-snapshot reconciliation. The panel passes complete responses and the shared tracker's live windows; projection only reads typed layouts. Sequence tests feed the cache through the production interface and verify visible geometry after delayed snapshots, layout changes, window closure and full refreshes. Niri's existing owner-scoped subscription still manages cleanup.

```sh
cargo test --locked overview::tests
```

## Native clipboard checks

`src/clipboard_service/payload.rs` owns MIME, encoding, safe aliases and history admission rules. Transport, history and presentation use that shared interpretation; transport's larger ownership limit and cheap completeness checks remain distinct from full history validation. The panel retains immutable image metadata and bounded text previews across history reordering; full-content search text is indexed lazily and released with the retained entries.

`src/clipboard_service/transport.rs` owns the Wayland connection, selection ownership and bounded nonblocking transfers. Its socket-backed ext/wlr tests include a source offering only `text/plain; charset=utf-8` and verify capture and original-byte copying. `history.rs` owns deduplication, pinning, retention and authenticated storage; tests use temporary directories and injected keys, not the user's keyring. The writer lock is released explicitly when history closes, even if a concurrent child still holds a duplicate descriptor.

`clipboard_service.rs` runs the same session dispatch with native or controlled copy I/O. Its tests drive capture, pinning, removal, missing entries, failures and mailbox shutdown. `src/clipboard/copy.rs` owns invocation-scoped pending requests, feedback and close-on-success; the session only publishes receipts. Lifecycle tests use the production request/result path to cover coalesced dismissal/reopening, output switches, delayed owner disposal and batched stale/current acknowledgements. A Panel invocation ends on logical dismissal, not when its closing animation finishes.

```sh
cargo test --locked --bin shoji-bar-4 clipboard
```

## CPU reports

When the shell keeps using CPU:

```sh
scripts/cpu-report.sh 10               # look for 10 seconds now and write a report
scripts/cpu-report.sh watch 8 30       # stay running; write a report when CPU is over 8% twice in a row
scripts/cpu-report.sh --sudo watch     # also record perf's time per function (only perf runs under sudo)
```

Reports are written to `~/.local/state/shoji-bar-4/reports/`. They cover:

- **CPU and wakeups per thread**: a UI thread waking 60 to 120 times a second is drawing frames all the time.
- **`msg status`**: whether a clock keeps ticking (`ticking`), and whether the UI thread answers quickly.
- **perf**: the functions taking the time. Release builds keep function names for this.
- **D-Bus**: whether a flood of signals is coming in.

The report ends with what these suggest.

## What was added to AntiWebUI for this shell

All of it went in as general features, and is in AntiWebUI's `main`.

- **Layer background**: `WindowOptions::background`; layers are transparent by default.
- **md3e dynamic color**, ported from material-color-utilities:
  - `ColorScheme::from_seed` / `from_seed_with`, with 9 variants.
  - `seed_colors`, which takes colors from an image.
  - `ColorScheme::lerp` and `set_color_scheme_animated`.
- **md3e shapes**: `MaterialShape` (34 shapes), `shape` / `styled_shape` (which morph on a spring), and a public `Ink` (state layers and ripples).
- **md3e components**:
  - `DatePickerStyle::no_header()`.
  - `wavy_progress_eased`: a wavy progress bar whose line moves to a new value at a speed you choose, for seek bars.
  - Menu submenus: `MenuItem::submenu`.
- **style**: `overflow_x_scroll()`.
- **Image memory**:
  - Large photos are decoded one at a time and scaled down while still RGB.
  - What stays in glibc's heap is returned to the OS.
  - `images::decode_bitmap` takes the pixels without showing the image.
- **shell**: `Battery`'s `energy()`, `energy_full()`, `health()` and `charge_cycles()`.
