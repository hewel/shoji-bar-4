# Usage

## Installing and running

From the checkout, build and install for the current user:

```sh
./scripts/install.sh
~/.local/bin/shoji-bar-4
```

Run the binary inside a ShojiWM session, only if a shell is not already running. Installation does not stop or restart the existing shell, change compositor configuration, or install system packages. It requires Cargo, a Rust toolchain and the project's native build dependencies.

The script uses `cargo install --locked --force` on this checkout, builds a release, and installs the executable to `~/.local/bin`. It can be invoked from any working directory. Run it again after updating the source to upgrade. Add `~/.local/bin` to your shell's `PATH` for terminal commands.

To use a different prefix (relative paths are relative to the invoking directory):

```sh
./scripts/install.sh --prefix "$HOME/apps/shoji"
```

This installs to `$HOME/apps/shoji/bin/shoji-bar-4`. The script never invokes sudo; the prefix must be writable by the invoking user. To remove a default installation, use `cargo uninstall --root "$HOME/.local" shoji-bar-4`; saved settings remain untouched.

For development without installation:

```sh
cargo build --locked --release
./target/release/shoji-bar-4
```

## Using it with ShojiWM

ShojiWM's default config starts the executable in `~/.config/shoji-bar-4/target/release/`. To use an installed binary, update its shell command and keybindings to the installed path. For a config of your own, start the shell and bind its panels to keys in `~/.config/shojiwm/src/index.tsx`:

```ts
const SHOJI_BAR_4 = "~/.local/bin/shoji-bar-4";
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

The layer namespaces are `shoji-bar-4-bar`, `-dock`, `-quick-settings`, `-calendar`, `-launcher`, `-overview`, `-clipboard`, `-media`, `-ai-usage`, `-nosleep`, `-notifications`, `-osd`, `-power`, `-wallpaper` and `-idle` (the warning fade). Use them to give layers ShojiWM effects such as blur.

## Commands

The running shell takes commands over its socket (`$XDG_RUNTIME_DIR/shoji-bar-4.$WAYLAND_DISPLAY.sock`):

```sh
shoji-bar-4 msg toggle launcher [OUTPUT]       # output selection described below
shoji-bar-4 msg toggle overview [OUTPUT]        # Niri workspace/window overview
shoji-bar-4 msg toggle quick-settings [OUTPUT]
shoji-bar-4 msg toggle calendar [OUTPUT]
shoji-bar-4 msg toggle clipboard [OUTPUT]
shoji-bar-4 msg toggle media [OUTPUT]
shoji-bar-4 msg toggle ai-usage [OUTPUT]         # all configured AI accounts
shoji-bar-4 msg ai-usage refresh                # respects retry backoff
shoji-bar-4 msg toggle nosleep [OUTPUT]          # manual & agent sleep-guard status
shoji-bar-4 msg nosleep on|off|toggle|refresh|status
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
| `gtk_theme_enabled` | Opt into reversible GTK 3/4 palette linking; default `false` |
| `zen_theme_enabled` | Opt into reversible Zen Browser chrome palette linking; default `false` |
| `pinned` | Desktop IDs of the apps pinned to the dock |
| `bar_position` | `top` / `left` / `right`; changing it in Settings moves the bar immediately |
| `nosleep_enabled` | Show the Nosleep island and panel (`true` by default) |
| `clock_24h` / `clock_seconds` | How the clock is shown |
| `idle` | Idle policy; all automatic actions disabled by default (see below) |
| `ai_usage` | AI usage collector and bar options (see below) |

Wallpapers to choose from are read from `~/Pictures/wallpapers`.

## Matugen and application colors

Install [Matugen](https://github.com/InioX/matugen) 4.2 or newer and make it available on the shell's `PATH`. **Settings → Style & color** controls the wallpaper/seed, existing six variants, and light/auto/dark mode. **Retry theme** regenerates the current selection, including an image whose contents changed at the same path.

The shell runs Matugen with its own temporary configuration and bundled templates. It does not read or modify your Matugen configuration, execute your hooks, or ask Matugen to set the wallpaper. The last valid palette is cached at `$XDG_STATE_HOME/shoji-bar-4/theme/last-good.json`. A missing CLI, invalid image or generation error leaves that palette in use and shows an error. Without a usable cache, the shell starts with built-in colors. The effective mode stays with the applied colors if a requested mode fails.

Under **Application colors**, enable each target explicitly:

- **GTK 3 / 4** requires the `adw-gtk-theme` package (Arch: `sudo pacman -S adw-gtk-theme`), the `gsettings` command, and `libgio-2.0.so.0`. GTK 3 uses `adw-gtk3` / `adw-gtk3-dark`; GTK 4 removes the managed Colloid injection and uses the native Adwaita base. Fonts, icon theme and cursor settings are preserved. Separate generated `shoji-colors.css` files are imported by `$XDG_CONFIG_HOME/gtk-3.0/gtk.css` and `gtk-4.0/gtk.css`. Only the final desktop theme/mode preference is set—there is no flip-to-light-and-back reload trick. Application-specific hardcoded colors and sandboxed applications are not guaranteed to follow; no Flatpak permissions are changed.
- **Zen Browser** uses the install-default profile in `$XDG_CONFIG_HOME/zen/profiles.ini`, falling back to `~/.zen/profiles.ini`. Ambiguous defaults are reported rather than applying to every profile. Its `chrome/userChrome.css` imports `chrome/shoji-colors.css`, and a managed `user.js` directive enables user stylesheets. The palette and mode override workspace colors without deleting workspace settings. Websites, internal pages, `userContent.css` and Zen Mods' `zen-themes.css` are untouched.

The shell updates as soon as a palette is accepted. GTK and Zen receive generated files, but running applications may not reload them. Restart them yourself when convenient; the shell never closes them. Preview mode never writes the palette cache or changes external application configuration, even when saved linking settings are enabled.

### Turning linking off and resolving conflicts

Turning a switch off restores the configuration still owned by the shell. Existing files and symbolic links are recorded before mutation; link targets are never overwritten. Unrelated additions to supported text configuration are retained. Later edits to a managed field or generated file cause a visible conflict instead of being overwritten.

Recovery records are kept under `$XDG_STATE_HOME/shoji-bar-4/theme/`. Do not delete them while an integration is active or a recovery error is reported: they contain the originals needed for restoration. Resolve the reported conflicting managed field/file, then retry. Symlinked parent directories are rejected conservatively rather than followed.

Removing a Zen `user.js` directive alone cannot undo a value already persisted by the browser. When necessary, disabling therefore leaves a tracked directive that restores the previous value at the next Zen start. The shell never writes `prefs.js`, including while Zen is running. If Zen's default profile changes, disable the old integration before enabling the new default.


## Configuring AI usage

Install and configure [ai-usagebar](https://github.com/akitaonrails/ai-usagebar), then confirm `ai-usagebar usage --json` succeeds. The executable must be on the shell process's `PATH`. The panel does not manage credentials; a missing CLI produces setup guidance rather than invented readings.

Open **Quick Settings → Settings → AI usage**, or the gear in the AI panel, to change the account and polling options. The AI island's ring shows remaining quota; hover or open the panel for exact values. An empty ring with a blocked symbol means exhausted, a question mark means no percentage is available, and an hourglass means initial collection. Left-click for all accounts; right-click to refresh. Account chevrons expand details; the header's refresh icon includes the last update time in its tooltip. Clock icons indicate personal reset countdowns; the Codex Resets footer uses calendar and forecast symbols for public information. Escape or an outside click dismisses the panel. Disabling AI usage hides its island, closes its panel and stops further collection.

The persisted `ai_usage` object accepts:

| Field | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Show the island and collect usage |
| `provider` | `"auto"` | Exact report account ID, including named accounts such as `openai@work`, or automatic selection |
| `refresh_minutes` | `5` | Usage polling interval, clamped to 1–120 minutes |
| `resets_enabled` | `true` | Fetch public Codex reset information; disabling hides it and ignores pending results |
| `resets_refresh_minutes` | `10` | Public polling interval, clamped to 5–120 minutes |
| `show_countdown` | `true` | Show the selected window's countdown on the horizontal bar |
| `show_reset_indicator` | `true` | Show a public-announcement indicator on the bar |

Failures use exponential backoff up to one hour; HTTP 429 waits at least 15 minutes. Neither reopening nor manually refreshing bypasses backoff. Countdowns update locally. Settings are saved by the shell; edit the JSON only while the shell is stopped to avoid overwriting live changes.

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

Any MPRIS player reporting Playing pauses Shoji's automatic idle actions, including lock, screen-off and suspend. Countdown timers restart when the last playing player pauses, stops or exits; Caffeine and application inhibitors remain independent. This does not block manual or external sleep, and players that do not expose MPRIS still need an application inhibitor or the Caffeine tile.

Safe offscreen previews (no idle actions):

```sh
shoji-bar-4 preview idle /tmp/idle.png --width 440 --height 700
shoji-bar-4 preview idle-warning /tmp/idle-warning.png --ms 2200
```
