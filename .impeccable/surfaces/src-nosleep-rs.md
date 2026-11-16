---
version: 1
slug: "src-nosleep-rs"
primary_target: "src/nosleep.rs"
related_targets: ["src/nosleep_service.rs","src/bar.rs","src/idle_policy.rs"]
---

# Nosleep

Mode: Operate. Extend the existing Shoji bar and its Material 3 Expressive panel system without inventing a separate visual world.

## Direction contract

THESIS: Surface manual sleep inhibition (`nosleep.service`) and automatic Herdr agent protection (`nosleep-agents.service`) as one calm, scannable bar emblem with an on-demand anchored status card—never polling when idle and never leaving automatic locks unguarded while agents work.

OWN-WORLD: Inherit dynamic Material roles, Roboto Flex type scale, native `switch` and `styled_icon_button`, capsule bar islands (`32px` top / `40px` vertical), `30px` rounded panels on `surface_container` with `surface_container_high` cards, and zero idle animation ticks.

STORY: Glance at the bar to distinguish idle, manual block, working-agent guard, and service warnings; left-click to toggle manual protection, right-click to inspect active Herdr agents or flip the switch, middle-click to jump to Idle settings.

FIRST VIEWPORT: Always-visible compact emblem in the trailing island group (`Nosleep → AI Usage → Media → Tray → Status`); secondary badges expand smoothly only when both protections are active or a warning exists. Detailed status, saved power profile context, working agent rows, and errors live inside the anchored `Nosleep` panel and bilingual tooltip.

FORM: Scoped extension of the incumbent native interface using `anti-webui` and `anti-webui-md3e`. Horizontal and vertical bar placements share one reactive `Snapshot` from `nosleep_service`. Chinese and English copy follow process locale.

SIGNATURE: `inotify` + `org.freedesktop.systemd1` D-Bus event streams feed both the bar UI and Shoji's built-in `IdleCoordinator`, keeping idle CPU at 0% while automatically deferring idle lock/suspend until working agents finish.
