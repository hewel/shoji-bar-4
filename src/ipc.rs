//! Commands from outside, such as the compositor's key bindings, over a socket in
//! `$XDG_RUNTIME_DIR`:
//!
//! ```text
//! shoji-bar-4 msg toggle launcher [OUTPUT]
//! shoji-bar-4 msg toggle overview [OUTPUT]
//! shoji-bar-4 msg toggle quick-settings [OUTPUT]
//! shoji-bar-4 msg toggle calendar [OUTPUT]
//! shoji-bar-4 msg toggle clipboard [OUTPUT]
//! shoji-bar-4 msg toggle media [OUTPUT]
//! shoji-bar-4 msg close
//! shoji-bar-4 msg lock
//! shoji-bar-4 msg theme dark|light|auto|toggle
//! shoji-bar-4 msg quit
//! shoji-bar-4 msg status
//! shoji-bar-4 msg idle status
//! shoji-bar-4 msg idle configure '{"pre_action_fade_seconds":2,"behaviors":[]}'
//! shoji-bar-4 msg caffeine on|off|toggle
//! shoji-bar-4 msg suspend
//! ```
//!
//! Without an output, prefer Niri's focused workspace, then the focused window, then the first output.
//! An implicit toggle closes an already-open panel of the same kind before resolving output.

use crate::panels::{self, Panel};
use crate::settings::{ThemeMode, settings};
use anti_webui::prelude::*;
use anti_webui::shell::niri;
use anti_webui::shell::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

fn socket() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    dir.join(format!("shoji-bar-4.{display}.sock"))
}

/// Sends a command to the running shell and prints what it says.
pub fn send(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(socket())?;
    writeln!(stream, "{}", args.join(" "))?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    let answer = answer.trim();
    match answer.strip_prefix("ok") {
        Some(said) if said.is_empty() || said.starts_with(' ') => {
            if !said.is_empty() {
                println!("{}", said.trim_start());
            }
            Ok(())
        }
        _ => Err(answer.to_owned().into()),
    }
}

/// Listens for commands. Call once, from the root.
pub fn serve() {
    let path = socket();
    // Someone else listening means another shell is running; a file with no one behind it is left
    // over from one that died.
    if UnixStream::connect(&path).is_ok() {
        eprintln!("shoji-bar-4: another one is running at {}", path.display());
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("shoji-bar-4: no commands from outside: {e}");
            return;
        }
    };
    let to_ui = ui_channel(
        move |(line, answer): (String, std::sync::mpsc::Sender<String>)| {
            let _ = answer.send(match run(&line, focused_output) {
                Ok(said) if said.is_empty() => "ok".into(),
                Ok(said) => format!("ok {said}"),
                Err(e) => e,
            });
        },
    );
    let _ = std::thread::Builder::new()
        .name("shoji-bar-4 ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let (tx, rx) = std::sync::mpsc::channel();
                to_ui.send((line.trim().to_owned(), tx));
                let answer = rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap_or_else(|_| "no answer".into());
                let _ = writeln!(&stream, "{answer}");
            }
        });
}

/// What could keep the shell busy, on one line, for `scripts/cpu-report.sh`.
fn status() -> String {
    let panel = panels::current().map_or("none".into(), |(output, panel)| {
        format!("{panel:?}@{output}")
    });
    let playing = media()
        .current()
        .map_or("none".into(), |p| format!("{:?}", p.status));
    let notifications = crate::notify::server().map_or("off".into(), |s| {
        format!("{}/{}", s.popups().len(), s.notifications().len())
    });
    format!(
        "panel={panel} ticking={} media={playing} popups/notifications={notifications} \
         windows={} workspaces={} tray={} dark={}",
        crate::ui::ticking(),
        toplevels().all().len(),
        workspaces().all().len(),
        tray().items().len(),
        crate::theme::is_dark(),
    )
}

/// The compositor's focused workspace output, then the active window's output, else the first.
pub fn focused_output() -> Option<String> {
    resolve_focused_output(
        niri::workspaces(),
        || {
            toplevels()
                .active()
                .and_then(|t| t.outputs.first().cloned())
        },
        || outputs().into_iter().next().map(|o| o.name),
    )
}

fn resolve_focused_output(
    workspaces: Vec<niri::Workspace>,
    window_output: impl FnOnce() -> Option<String>,
    first_output: impl FnOnce() -> Option<String>,
) -> Option<String> {
    // Workspace focus survives a keyboard-interactive layer and its closing animation;
    // toplevel focus does not. Only use the fallback when that compositor fact is absent.
    workspaces
        .into_iter()
        .find(|workspace| workspace.is_focused)
        .and_then(|workspace| workspace.output)
        .or_else(window_output)
        .or_else(first_output)
}

fn run(line: &str, resolve_output: impl FnOnce() -> Option<String>) -> Result<String, String> {
    if let Some(json) = line.strip_prefix("idle configure ") {
        let value = serde_json::from_str(json).map_err(|e| format!("invalid idle JSON: {e}"))?;
        crate::idle::apply_config(&value)?;
        return Ok(String::new());
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["toggle" | "open", name, rest @ ..] => {
            let panel = Panel::from_name(name).ok_or_else(|| format!("no panel called {name}"))?;
            if !panel.enabled() {
                return Err(format!("{name} is disabled in Settings"));
            }
            // A panel can take keyboard focus away from the toplevel that chose its output.
            // An implicit toggle closes that panel before consulting the changed focus.
            let open_now = panels::current().is_some_and(|(output, open_panel)| {
                open_panel == panel
                    && rest
                        .first()
                        .is_none_or(|requested| *requested == output.as_str())
            });
            if words[0] == "toggle" && open_now {
                panels::close();
            } else {
                let output = rest
                    .first()
                    .map(|s| s.to_string())
                    .or_else(resolve_output)
                    .ok_or("no output")?;
                panels::open(&output, panel);
            }
            Ok(String::new())
        }
        // There is no unlock: only the password does that.
        ["lock"] => {
            crate::lock::lock();
            Ok(String::new())
        }
        ["suspend"] => {
            crate::idle::request_suspend();
            Ok(String::new())
        }
        ["idle", "status"] => Ok(crate::idle::status_text().replace('\n', " · ")),
        ["idle", "config"] => Ok(settings().with(|s| s.idle.to_json().to_string())),
        ["idle", "cancel"] => {
            crate::idle::cancel_warning();
            Ok(String::new())
        }
        ["caffeine", mode @ ("on" | "off" | "toggle")] => {
            let enabled = match *mode {
                "on" => true,
                "off" => false,
                _ => !crate::idle::caffeine(),
            };
            crate::idle::set_caffeine(enabled);
            Ok(String::new())
        }
        ["close"] => {
            panels::close();
            Ok(String::new())
        }
        ["theme", mode] => {
            let mode = match *mode {
                "dark" => ThemeMode::Dark,
                "light" => ThemeMode::Light,
                "auto" => ThemeMode::Auto,
                "toggle" if crate::theme::is_dark() => ThemeMode::Light,
                "toggle" => ThemeMode::Dark,
                other => return Err(format!("no theme called {other}")),
            };
            settings().update(|s| s.mode = mode);
            Ok(String::new())
        }
        ["status"] => Ok(status()),
        ["quit"] => {
            let _ = std::fs::remove_file(socket());
            quit();
            Ok(String::new())
        }
        _ => Err(format!("unknown command: {line}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_implicit_toggle_closes_even_when_focus_moves_or_disappears() {
        panels::close();
        run("toggle overview", || Some("DP-2".into())).unwrap();
        assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Overview)));

        // A keyboard-interactive panel can hide the formerly focused toplevel.
        // The next lookup would then fall back to a different output.
        run("toggle overview", || Some("DP-1".into())).unwrap();
        assert_eq!(panels::current(), None);

        run("toggle overview", || Some("DP-2".into())).unwrap();
        run("toggle overview", || None).unwrap();
        assert_eq!(panels::current(), None);
    }

    #[test]
    fn explicit_target_and_open_keep_their_output_selection() {
        panels::close();
        run("toggle overview DP-2", || Some("DP-1".into())).unwrap();
        assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Overview)));
        run("toggle overview DP-1", || Some("DP-2".into())).unwrap();
        assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Overview)));
        run("toggle overview DP-1", || Some("DP-2".into())).unwrap();
        assert_eq!(panels::current(), None);

        run("open overview DP-1", || None).unwrap();
        run("open overview", || Some("DP-2".into())).unwrap();
        assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Overview)));

        run("open calendar DP-1", || None).unwrap();
        run("toggle overview", || Some("DP-2".into())).unwrap();
        assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Overview)));
        panels::close();
    }

    #[test]
    fn rapid_toggles_follow_workspace_focus_while_window_focus_is_absent() {
        let mut workspaces = vec![
            niri::Workspace {
                id: 1,
                output: Some("DP-1".into()),
                is_active: true,
                ..Default::default()
            },
            niri::Workspace {
                id: 2,
                output: Some("DP-2".into()),
                is_active: true,
                is_focused: true,
                ..Default::default()
            },
        ];
        panels::close();
        for press in 0..8 {
            run("toggle overview", || {
                resolve_focused_output(
                    workspaces.clone(),
                    || (press == 0).then(|| "DP-2".into()),
                    || Some("DP-1".into()),
                )
            })
            .unwrap();
            assert_eq!(
                panels::current(),
                if press % 2 == 0 {
                    Some(("DP-2".into(), Panel::Overview))
                } else {
                    None
                },
            );
        }

        // A real workspace-focus change wins, even if the toplevel source is stale.
        workspaces[0].is_focused = true;
        workspaces[1].is_focused = false;
        run("toggle overview", || {
            resolve_focused_output(workspaces, || Some("DP-2".into()), || Some("DP-2".into()))
        })
        .unwrap();
        assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Overview)));
        panels::close();
    }

    #[test]
    fn opening_without_workspace_focus_retains_window_and_output_fallbacks() {
        panels::close();
        run("toggle overview", || {
            resolve_focused_output(Vec::new(), || Some("DP-2".into()), || Some("DP-1".into()))
        })
        .unwrap();
        assert_eq!(panels::current(), Some(("DP-2".into(), Panel::Overview)));
        panels::close();
        run("toggle overview", || {
            resolve_focused_output(Vec::new(), || None, || Some("DP-1".into()))
        })
        .unwrap();
        assert_eq!(panels::current(), Some(("DP-1".into(), Panel::Overview)));
        panels::close();
        assert_eq!(
            run("toggle overview", || resolve_focused_output(
                Vec::new(),
                || None,
                || None
            )),
            Err("no output".into()),
        );
        assert_eq!(panels::current(), None);
    }
}
