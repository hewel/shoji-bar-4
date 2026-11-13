# shoji-bar-4

A Material 3 Expressive desktop shell for [ShojiWM](https://github.com/bea4dev/ShojiWM), written in Rust on [AntiWebUI](https://github.com/bea4dev/AntiWebUI).

![shoji-bar-4 on ShojiWM](./ss.png)

- **Top or vertical side bar** of floating islands: launcher, workspaces, the active window, a rolling clock, now playing, tray and status icons.
- **Dock** that hides at the bottom edge and rises when the pointer reaches it.
- **Quick settings**, **calendar**, **launcher**, **clipboard history**, **media player**, **notifications**, **OSD** and a **lock screen**.
- **Dynamic color** from a seed or the wallpaper, moved to on a spring everywhere at once, window frames included.
- Springs and shape morphs throughout, and no frames drawn while nothing moves (0% CPU when idle).

## Getting started

You need ShojiWM, a Rust toolchain, and the usual desktop services: NetworkManager, BlueZ, PipeWire, UPower, power-profiles-daemon and logind. Clipboard capture is built in and requires the compositor's `ext-data-control-v1` or `wlr-data-control` protocol; an unlocked Secret Service keyring enables encrypted history across restarts. `zenity` lets you pick a user picture.

```sh
git clone https://github.com/bea4dev/shoji-bar-4 ~/.config/shoji-bar-4
cd ~/.config/shoji-bar-4
./scripts/install.sh
```

The script builds with `Cargo.lock` and installs to `~/.local/bin/shoji-bar-4`. Run it again after updating the checkout to upgrade; use `--prefix /path/to/prefix` for a different installation directory. It does not install system packages, edit your configuration, or restart the running shell.

ShojiWM's default config starts `~/.config/shoji-bar-4/target/release/shoji-bar-4` with the session, binds its keys (`Super` for the launcher, `Super+N` quick settings, `Super+C` calendar, `Super+V` clipboard, `Super+L` lock) and draws the window frames in the shell's colors. To use the installed binary, point its shell command and keybindings at `~/.local/bin/shoji-bar-4`; see [Using it with ShojiWM](docs/usage.md#using-it-with-shojiwm).

## Documentation

- [Features](docs/features.md): everything the shell does, part by part.
- [Usage](docs/usage.md): running it, ShojiWM integration, commands and settings.
- [Development](docs/development.md): building against a local AntiWebUI, offscreen previews and CPU reports.
- [Glossary](GLOSSARY.md): the bar, islands, panels, placements and shell surfaces' shared language.
