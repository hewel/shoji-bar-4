# Usage

## Building and running

```sh
cargo build --release
./target/release/shoji-bar-4
```

Run it inside a ShojiWM session. Normally ShojiWM starts it (see below).

## Using it with ShojiWM

ShojiWM's default config already does everything here. For a config of your own, start the shell and bind its panels to keys in `~/.config/shojiwm/src/index.tsx`:

```ts
const SHOJI_BAR_4 = "~/.config/shoji-bar-4/target/release/shoji-bar-4";
COMPOSITOR.process.once("shell", {
  command: `exec ${SHOJI_BAR_4}`,
  runPolicy: "once-per-session",
});

function toggleLauncher() {
  const monitor = HYBRID_WINDOW_MANAGER.getCurrentMonitorName();
  COMPOSITOR.process.spawn({ command: `${SHOJI_BAR_4} msg toggle launcher "${monitor}"` });
}
COMPOSITOR.key.bind("launcher", "Super+A", toggleLauncher);
```

The window frames can follow the shell's colors:

- The shell stays connected to ShojiWM's IPC socket (`$XDG_RUNTIME_DIR/shojiwm-$WAYLAND_DISPLAY.sock`).
- It sends its color scheme there as `theme.set` (`src/wm.rs`): on start, on reconnecting (including after a config reload), and whenever the scheme changes.
- A config that doesn't handle `theme.set` ignores it.

The layer namespaces are `shoji-bar-4-bar`, `-dock`, `-quick-settings`, `-calendar`, `-launcher`, `-overview`, `-clipboard`, `-media`, `-notifications`, `-osd`, `-power`, `-wallpaper` and `-idle` (the warning fade). Use them to give layers ShojiWM effects such as blur.

## Commands

The running shell takes commands over its socket (`$XDG_RUNTIME_DIR/shoji-bar-4.$WAYLAND_DISPLAY.sock`):

```sh
shoji-bar-4 msg toggle launcher [OUTPUT]       # output selection described below
shoji-bar-4 msg toggle overview [OUTPUT]        # Niri workspace/window overview
shoji-bar-4 msg toggle quick-settings [OUTPUT]
shoji-bar-4 msg toggle calendar [OUTPUT]
shoji-bar-4 msg toggle clipboard [OUTPUT]
shoji-bar-4 msg toggle media [OUTPUT]
shoji-bar-4 msg close
shoji-bar-4 msg lock                           # only the password unlocks
shoji-bar-4 msg suspend                        # lock acknowledgement, then suspend
shoji-bar-4 msg idle status                    # policy/inhibitor status and integration errors
shoji-bar-4 msg idle config                    # current policy as JSON
shoji-bar-4 msg idle cancel                    # cancel pending actions and reset countdowns
shoji-bar-4 msg caffeine on|off|toggle          # temporary automatic-idle inhibition
shoji-bar-4 msg theme dark|light|auto|toggle
shoji-bar-4 msg status                         # open panels, running clocks and so on
shoji-bar-4 msg quit
```

Without `OUTPUT`, `toggle` closes an already-open panel of the same kind, even if keyboard focus has moved or no window is focused. When opening, Niri's focused workspace determines the output, including on empty workspaces and while a panel is closing. If that information is unavailable, the focused window's output is used, then the first output. An explicit `OUTPUT` keeps the toggle local to that output; `open` always opens on the selected output rather than closing the panel.

## Settings

Settings are kept in `$XDG_STATE_HOME/shoji-bar-4/settings.json` (usually `~/.local/state/…`). Changes made in the quick settings are saved there, and you can also edit the file by hand.

| Key | Meaning |
|---|---|
| `mode` | `light` / `dark` / `auto` (light from 7:00 to 19:00) |
| `seed` | The seed color (`#rrggbb`) |
| `seed_from_wallpaper` | Take the seed color from the wallpaper |
| `palette` | `tonal-spot` / `vibrant` / `expressive` / `fidelity` / `neutral` / `monochrome` |
| `wallpaper` | The wallpaper's path (`null` draws none) |
| `pinned` | Desktop IDs of the apps pinned to the dock |
| `bar_position` | `top` / `left` / `right`; changing it in Settings moves the bar immediately |
| `clock_24h` / `clock_seconds` | How the clock is shown |
| `idle` | Idle policy; all automatic actions disabled by default (see below) |

Wallpapers to choose from are read from `~/Pictures/wallpapers`.

## Configuring idle actions

Open **Quick Settings → Settings → Idle & sleep**. Each action has a switch and a timer summary; its edit button opens the timer, with **More options** for action type, name, locked-screen timing and commands. **Advanced** contains warning duration, custom-action creation and service status. Press **Apply** after editing. The `idle` object is persisted with the other settings. Invalid policy input is rejected as a whole; an invalid policy loaded from disk falls back to disabled defaults.

`idle configure` replaces the entire policy, not just one rule. For example, this enables only a ten-minute lock with a two-second warning:

```sh
shoji-bar-4 msg idle configure '{"pre_action_fade_seconds":2,"behaviors":[{"name":"lock","enabled":true,"timeout_seconds":600,"action":"lock"}]}'
```

| Field | Meaning |
|---|---|
| `pre_action_fade_seconds` | Additional warning time after an idle timeout, 0–60 seconds; 0 acts immediately |
| `behaviors` | Up to 64 rules; an empty array disables all automatic actions |
| `name` | Unique, nonempty rule name |
| `enabled` | Whether this rule runs; defaults to false |
| `timeout_seconds` | Normal inactivity timeout, 0–86400; 0 disables this clock |
| `locked_timeout_seconds` | Timeout while locked, 0–86400; 0 uses the normal timeout |
| `action` | `lock`, `screen_off`, `suspend`, `lock_and_suspend` or `command`; required for enabled rules |
| `lock_before_suspend` | For `suspend`, wait for locking first; defaults to true. `lock_and_suspend` always locks |
| `command` | Required for `command`; executed as your user through `/bin/sh -c` |
| `resume_command` | Optional command after activity/wake or policy cancellation, only if the action ran |

Screen power requires `NIRI_SOCKET` and a compatible compositor. Custom commands are not sandboxed; review them before enabling. Cancellation prevents queued, not-yet-admitted commands from starting but does not terminate a command already admitted. Caffeine does not bypass locking for explicit or external sleep requests. Integration failures appear in `idle status` and the Idle page; if the session/system bus restarts or another screensaver service releases its name, restart Shoji to re-establish that integration.

Any MPRIS player reporting Playing pauses Shoji's automatic idle actions, including lock, screen-off and suspend. Countdown timers restart when the last playing player pauses, stops or exits; Caffeine and application inhibitors remain independent. This does not block manual or external sleep, and players that do not expose MPRIS still need an application inhibitor or the Caffeine switch.

Safe offscreen previews (no idle actions):

```sh
shoji-bar-4 preview idle /tmp/idle.png --width 440 --height 700
shoji-bar-4 preview idle-warning /tmp/idle-warning.png --ms 2200
```
