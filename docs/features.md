# Features

## Bar

Floating islands along the top, left or right. **Settings → Desktop → Bar position** moves the bar immediately.

- **Launcher button**: its Material Shape morphs (Cookie4 → Cookie9 → Circle).
- **Workspaces**: a marker that stretches as it moves, its leading edge on a fast spring and its trailing edge on a slow one. The mouse wheel switches workspaces too. When workspaces come and go, their places open and close and the island follows along the bar.
- **Active window**: on the top bar, its icon and title. A new window, or a new title, comes up from below as the last one goes up, and the island follows its width on a spring, however fast they change. A title wider than the island for the moment fades out at its edge. A few characters changing in one place (a spinner like Claude Code's, a counter, a clock) change where they are, without the title coming up again. A vertical bar shows the application icon only.
- **Clock**: the digits roll as they change, and hovering changes Roboto Flex's weight. A vertical bar stacks hours, minutes and optional seconds, with AM/PM for 12-hour time; the date stays on the top bar.
- **Now playing** (a dancing equalizer): a click opens the player under the top bar or beside a vertical bar, a middle click pauses or plays. The vertical island omits the title.
- **AI usage**: a compact remaining-percentage island; a click opens all accounts, a right click refreshes. Vertical bars stack the label and percentage.
- **Tray**, network, Bluetooth, volume and battery. Tray menus open beside a vertical bar. Tray icons are resolved from absolute files, application-provided icon directories or the desktop icon theme, with supplied pixmaps and title initials as fallbacks.

## Dock

The icon under the pointer lifts a little and shows its name.

- It stays hidden below the bottom of the screen. Moving the pointer to the bottom edge brings it up, and it goes down a moment after the pointer leaves. Windows can use the whole height of the screen.
- Pinned apps come first, then a divider, then the other open apps.
- An app that is starting bounces until its window appears.
- Dots underneath show how many windows an app has and which one is focused.
- Right-click menu: new window, desktop actions, pin, close.

## Quick settings

- **Tiles**: round off and change color when on.
- **Sliders** for volume and brightness.
- **Power menu**: Lock, Sleep, Log out, Restart, Shut down.
  - Log out (`shoji_wm --quit`), Restart and Shut down ask first, in a dialog in the middle of the screen over a blurred veil like the launcher's.
  - The dialog's icon shape spins in, and turns into a sun while the pointer is on the action button.
  - Cancel, Escape or a click outside closes it without doing anything.
- **Battery card**:
  - A filling battery picture and rolling numbers.
  - The time until empty or full, and the clock time it will be.
  - A state pill (charging, on battery, full, not charging), power draw, health and charge cycles.
  - Light flows through the battery while it charges.
- **Notifications**: always shown, with "No notifications" when there are none.
- **Settings page**, behind the gear (or the user picture):
  - **User picture**: a copy of the one you pick is kept in `$XDG_STATE_HOME/shoji-bar-4/`. Without one, `~/.face.icon`, `~/.face` or AccountsService's picture is used. Picking one needs zenity.
  - **Style and color**: wallpaper, seed color, scheme variant, and light, auto or dark.
  - **Desktop**: bar position and independent **Launcher** and **Dock** switches, both on by default and applied immediately on every output.
    - Disabling Launcher hides its bar and dock buttons and closes it if open. IPC requests to open or toggle it return an error while disabled.
    - Disabling Dock hides it and removes its bottom-edge trigger and menu input. Re-enabling it restores the dock; pinned apps are kept.
    - The switches persist as `launcher_enabled` and `dock_enabled` in `$XDG_STATE_HOME/shoji-bar-4/settings.json`. Older settings files keep both features enabled.
  - **Clock**: 24-hour time and seconds.
  - **Idle & sleep**: action switches and timeout summaries first; edit an action to change its timer. **More options** holds names, action types, locked-screen timing and resume commands. **Advanced** holds the warning duration, custom actions and service status. Edits take effect only after **Apply**.
  - **AI usage**: enable the collector, choose the bar's account, adjust refresh intervals and toggle countdowns or public reset information.

## Calendar

- A large clock that rolls down to the seconds.
- The month, from Monday, six weeks of it, with the days of the months around it faint. Today stands on a cookie that turns under the pointer.
  - ‹ and ›, or the mouse wheel, go through the months, which slide over; the month's name goes back to this one.
- Wavy rings for CPU, memory and temperature.

## AI usage

The native **AI usage** panel adapts `noctalia-ai-usage` without a Noctalia runtime. It uses `ai-usagebar usage --json` (schema 1) for account data and the optional [Codex Resets](https://codex-resets.com) public API.

- All configured accounts remain visible, regardless of the account selected for the bar. Each quota window has a label, a slim remaining bar, a percentage and its reset countdown; plan, exact times, personal reset credits and credit balances are disclosed on demand.
- The bar shows remaining quota as a compact ring around the AI symbol; exact percentages stay in its tooltip and the panel. A full ring means full quota, an empty ring with a blocked symbol means exhausted, and a question mark means no percentage is available. Initial collection uses an hourglass, and errors retain a separate warning. Refresh and preferences are icon buttons; account chevrons reveal details. Clock symbols accompany compact reset durations, while calendar and forecast symbols summarize the public feed. Hover or focus for labels; the refresh tooltip includes the last update time. Error and cached-state warnings remain explicit text.
- Panel percentages and the bar ring mean **remaining**, not used: 74% used becomes 26% remaining. In the panel, exhausted is 0%; unknown is “—”. A non-percentage metric retains its reported value.
- Automatic account selection prefers the report's primary account, then Codex, then an available account. A pinned account that disappears stays missing rather than silently changing identity.
- One collector is shared by every output and panel. Opening a panel does not request fresh data. Failed requests keep the previous snapshot with a visible warning; manual refresh respects the same retry backoff.
- Public announcements and AI forecasts are separate from personal quota resets. Passing an announced time means awaiting confirmation, not that a reset executed. Expired forecasts disappear; predictions are not official commitments. Source links open through `xdg-open`.
- Chinese and English text follows the process locale. Colors, typography, controls and placement follow the shell's existing Material theme.
- No login files are read by the shell, no raw CLI diagnostics are displayed, and no account information is sent to Codex Resets. Authentication remains the CLI's responsibility.

## Workspace overview

Open it from the bar's **Overview** icon or with `shoji-bar-4 msg toggle overview [OUTPUT]`. The bar icon appears when Niri is available and uses the accent color while the overview is open.

- Each output shows its workspaces as equal-sized, abstract screen viewports, in index order. A compact two-column layout becomes one column when space is limited; taller lists scroll.
- All windows on an output share one scale. Windows are clipped at the viewport edges; fully offscreen windows are not drawn or clickable. Empty workspaces retain an empty screen at the same scale. The heading counts all windows assigned to the workspace, including those outside the viewport.
- Scroll over a workspace viewport to browse its windows horizontally (vertical wheel or horizontal touchpad input). Each workspace remembers its own browsing offset while the panel is open; this does not change Niri's workspace or focus. Viewport scrolling stops at the content edges without scrolling the surrounding list; scroll outside a viewport to browse the workspace list.
- Tiled positions and the viewport offset are still estimated from column/row indices and the active window because Niri's IPC does not expose their current viewport coordinates. This is not an exact reconstruction of scrolling or stacking. Floating windows use their compositor-reported positions and sizes.
- Application icons and titles identify windows. Active windows and the active workspace use tinted backgrounds and accent outlines, without text badges. Urgent windows and workspaces retain error-colored outlines. Workspace cards have room around their previews; viewport corner radii are derived from the card radius, padding and border thickness to keep the nested curves concentric.
- Click a window to focus it, or a workspace heading to show that workspace; the overview closes. Escape, the close button or a click outside also closes it.
- Requires niri's IPC. It uses AntiWebUI directly, without Noctalia, Luau, `grim` or screenshots.

## Launcher

- It opens in the middle of the screen, its items coming in one after another.
- The grid rearranges itself (FLIP) as you search.
- Arrow keys, Enter and Escape work.

## Player

It opens under the top bar's now-playing island, or beside it on a vertical bar.

- A card on a base in the theme's colors, like the other panels. The album art fills the card, darkened so the text stays readable. When the track changes, the art and the title change over.
- Covers can come from local file URLs or HTTP/HTTPS URLs, including redirects. Network covers load in the background (up to 10 MiB, with a 10-second timeout); unavailable covers keep the music-note placeholder. A late download cannot replace a newer track's cover.
- **Wavy seek bar**: a click seeks there, and a drag seeks to where you let go. A handle appears at the end while the pointer is on it.
- Elapsed time and length, and previous, play or pause (a large button that rounds off while playing), and next.

## Clipboard history

Native Wayland clipboard capture runs for the whole shell session, independently of whether the panel is open. The panel updates immediately when new content arrives, with pinned entries first and recent history below.

- **Entries**: text shows an icon (text, link or path) and its first two lines; images show a thumbnail and their size.
- **Search** narrows the list; `png` or `image` finds images.
- **Choosing**: ↑↓ (and PageUp, PageDown) move a highlight that stretches as it goes, and the list springs to keep it in view.
- **Copying**: Enter or a click serves the original bytes and compatible MIME types directly through Wayland; only the panel invocation that requested the copy closes after success. Dismissing or reopening the panel prevents an old completion from closing the new invocation or replacing its error. Errors remain visible.
- **Pinning**: the pin button keeps an entry at the front and protects it from automatic eviction. Unpinning returns it to recent history.
- **Forgetting**: Delete (Shift+Delete while searching) or × forgets an entry; its row closes and the ones below move up.
- **Opening**: the rows drop in one after another. New captures preserve the selected entry while you browse.
- **Retention**: up to 100 entries and 256 MiB, including pins; individual text entries are limited to 4 MiB and images to 32 MiB. Repeated content is deduplicated. Blank text, invalid images and entries advertising `x-kde-passwordManagerHint` are excluded.
- **Text formats**: UTF-8 plain text accepts both `text/plain;charset=utf-8` and `text/plain; charset=utf-8`. Legacy `STRING` is displayed and searched as Latin-1; copying preserves its original bytes and never relabels non-ASCII Latin-1 as UTF-8.
- **Persistence**: history is authenticated and encrypted under `$XDG_STATE_HOME/shoji-bar-4/clipboard` (normally `~/.local/state/shoji-bar-4/clipboard`), with its key in Secret Service. A locked, missing or unavailable keyring leaves history in memory only, with a visible status; there is no plaintext fallback. Restart the shell after unlocking the keyring to enable persistence.
- **Images**: thumbnails decode from shared memory, without plaintext image-cache files.
- **Content ownership**: the shell can re-offer the last supported, complete, non-sensitive selection when its source application exits. A newer selection always supersedes that backup.
- **Requirement**: the compositor must support `ext-data-control-v1` (preferred) or `wlr-data-control`. No `cliphist`, `wl-paste` watcher or `wl-copy` process is needed. Existing cliphist records are neither imported nor deleted.

## Lock screen

It covers every screen with `ext-session-lock`.

- **Look**: a veil over the wallpaper, a large rolling clock and the date, the user picture and name, and the password field. Everything comes in one after another.
- **Typing**: each character adds a Material shape (cookie, clover, flower, ...) that bounces into place. While Enter checks the password, the shapes turn into a loading indicator.
- **Wrong password**: the field shakes and turns red, and says why. When faillock locks the account for a while, PAM's own message is shown.
- **Right password**: everything grows as it fades, the veil lifts, and the desktop with the same wallpaper is back.
- **Keys**: Escape or Ctrl+U clears the field, and Backspace removes one character.
- **Checking**: the password goes to PAM (the `login` service, `pam_authenticate` only) on a thread of its own.
- **When it locks**: on `shoji-bar-4 msg lock`, on logind's Lock (`loginctl lock-session`, or Lock in the quick settings power menu), and before sleep. Nothing outside the lock screen can unlock it.
- **If the shell dies while locked**: ShojiWM stays locked. Start another locker (`swaylock`, ...) in the same session from a TTY; it takes over the lock and can unlock it.
- **faillock**: with Arch's default settings, three wrong passwords in 15 minutes lock the account for 10 minutes, even against the right one.

## Idle & sleep

- Automatic actions start **disabled**. The presets are lock after 10 minutes, turn screens off after 11 minutes, and lock then suspend after 15 minutes; enable only those you want.
- Each named rule has an inactivity timeout, an optional locked-screen timeout, and one action: **Lock**, **Screen off**, **Suspend**, **Lock and suspend**, or a trusted custom shell command. Screen off currently uses Niri-compatible IPC; use custom commands on other compositors.
- A configurable fade (two seconds by default) follows the timeout before acting. It covers every output without intercepting input; activity cancels it. Waking, changing the policy or releasing inhibition starts fresh countdowns.
- Activity restores screens turned off by Shoji and runs a completed rule's optional resume command once. Commands already admitted for execution are not killed by later cancellation.
- **Caffeine**, a tile in Quick Settings, pauses local automatic actions and requests a logind idle/sleep inhibitor. Click the tile to toggle it; its On/Off label and active styling follow the other tiles. It is temporary, not persisted. **Idle & sleep → Advanced → Service status** reports when only local inhibition is available.
- **Media playback** automatically pauses Shoji's idle actions while any MPRIS player reports Playing, even without an application inhibitor. Pausing, stopping or closing the last playing player restarts full countdowns unless Caffeine or another inhibitor is still active. This also pauses automatic locking and screen-off; manual Sleep and external sleep requests remain unchanged. Players without MPRIS must supply an inhibitor or use Caffeine.
- Compositor idle inhibitors are respected through `ext-idle-notify`. Applications can also use the `org.freedesktop.ScreenSaver` inhibition API; cookies belong to the calling connection and disappear when it disconnects. Shoji does not replace another owner of that bus name.
- Manual **Sleep** and lock-required idle actions wait for the compositor to acknowledge the lock. Authentication cannot tear down that lock during the pending sleep transition; authenticate again after waking. Plain **Suspend** can explicitly opt out of locking.
- External sleep requests use a logind delay inhibitor and release it only after lock acknowledgement. This is not a veto: logind may proceed at its configured maximum delay if locking fails. Missing protocol or D-Bus capabilities are reported in status; do not run a second idle daemon with competing policies.

## Notifications

They work when the shell can become the notification daemon.

- Cards slide in at the top right on a spring.
- The history stays in the quick settings.
- Application icons stay in the small icon slot. Theme names in `image-path` also select an icon, not a body attachment.
- In-memory pictures and local absolute paths or `file://` URLs appear below the text, fitted without cropping or stretching (including SVG). Missing or invalid pictures leave no empty attachment area.
- Replacing a notification refreshes its text, actions and picture in both popups and history, including changed files at the same path.
- Do not disturb.

## OSD

When the volume or the brightness changes, a pill shows above the dock.

## Theme

- The scheme is made from a seed color with Material You's HCT. The seed can also come from the wallpaper.
- Variants: Tonal, Vibrant, Expressive, Fidelity, Neutral and Mono.
- A new scheme is moved to on a spring, everywhere at once.

## Wallpaper

A new wallpaper crossfades in, settling back from a slight zoom.
