# Shoji Bar

The desktop shell's shared language for its bar, transient panels and shell-owned surfaces.

## Language

**Bar**:
The row or column of floating islands along an output's top, left or right edge.
_Avoid_: Panel

**Island**:
A capsule-shaped group of related controls or status within the bar.

**Bar position**:
The user-selected output edge occupied by the bar: top, left or right.

**Bar placement**:
The geometric arrangement implied by a bar position: its main axis, thickness, clearance and the side on which adjacent panels open.

**Panel**:
A transient shell surface such as Quick Settings, Calendar, Media, Launcher, Clipboard or Power.

**Panel invocation**:
One logical opening of a panel on an output, ending when it is dismissed or replaced. Its closing animation may outlive it.

**Clipboard payload**:
The original bytes of a clipboard selection together with their advertised format and encoding.

**Workspace viewport**:
The browsable region in Overview that represents one workspace's windows at the proportions of the output's screen. It is neither the entire workspace nor the surrounding card with its heading.

**Anchor point**:
The centre of the island that invoked a panel, expressed in that output's logical coordinates.

**Shell layer**:
A shell-owned layer-shell surface occupied by a bar, panel, notification popup or similar shell UI.

**Idle coordinator**:
The module that sequences inactivity, lock acknowledgement, sleep and recovery. It owns ordering across those events, not their platform transport.

**Sleep hold**:
A lock-screen lifetime that suppresses authentication-driven unlocking while system sleep is pending. Releasing it permits a fresh authentication; it never unlocks by itself.
