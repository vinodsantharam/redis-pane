# navigation-demo.tape — records a short terminal demo of the tree
# Left/Right navigation and the value-pane cursor with VHS
# (https://github.com/charmbracelet/vhs — `brew install vhs`).
#
# Shows: collapsing/expanding a tree group and jumping to its parent with
# Left/Right (standard treeview convention, not the old Enter-toggle), then
# opening a key and moving a real cursor through its value with Enter, plain
# movement keys, and Esc back to the key list — all without a mouse.
#
# Render with:
#   vhs scripts/navigation-demo.tape
# which produces navigation-demo.gif in the repo root.
#
# Unlike demo.tape, this one targets your live local Redis directly — it
# does NOT flush or reseed anything. It expects the dev container from
# `./scripts/redis-up.sh` already running with real data in it, including a
# list key named `queue:132011` (any list with a few dozen elements works;
# change the filter text below if yours has a different name).
#
# Prereqs (run once, before recording):
#   ./scripts/redis-up.sh status   # confirm it's up — do NOT flush it
#   cargo build --release -p redis-pane
#   export PATH="$PWD/target/release:$PATH"
#
# The recording targets --url redis://127.0.0.1:6379 explicitly (not plain
# `redis-pane`) for the same reason demo.tape does: connection resolution is
# flags -> default Profile -> environment -> 127.0.0.1:6379 (CLAUDE.md), and
# a bare `redis-pane` would prefer a configured default Profile over this.
#
# Keybindings below match the tree Left/Right redesign and the value-cursor
# Enter/Esc design as of this writing. Keybindings are data (CLAUDE.md) and
# user-overridable, so if a step doesn't do what its comment says, check `?`
# in a live session and adjust the tape rather than assuming it's stale.

Require redis-pane

Output navigation-demo.gif

Set Shell "bash"
Set FontSize 18
Set Width 1200
Set Height 720
Set Theme "Dracula"
Set Padding 20
Set TypingSpeed 50ms

Type "redis-pane --url redis://127.0.0.1:6379"
Enter
Sleep 1500ms

# Tree mode is the default view. Filter down to one group, wide enough to
# have real children to fold.
Type "/"
Sleep 300ms
Type "queue"
Sleep 800ms
Enter
Sleep 800ms

# Collapse the group — Left, never a toggle, matching the standard treeview
# convention (VS Code, macOS/Windows outline views, WAI-ARIA).
Left
Sleep 1200ms

# Expand it again.
Right
Sleep 1000ms

# Right on an already-expanded group steps into its first child rather than
# collapsing it — Left is the only key that ever collapses.
Right
Sleep 800ms

# Left on a key row (no children of its own) jumps straight to its parent
# group, the same move a second Left on an already-collapsed group makes.
Left
Sleep 1200ms

# Narrow the filter down to the one key this demo cares about.
Type "/"
Sleep 300ms
Backspace 5
Type "132011"
Sleep 800ms
Enter
Sleep 800ms

# Step into the single remaining child, then open it.
Right
Sleep 500ms
Right
Sleep 1500ms

# A real cursor inside the value — Enter activates it.
Enter
Sleep 1000ms

Down
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 800ms

# A page at a time — VHS has no Home/End key, so PageDown twice stands in
# for "jump toward the end" in this recording.
PageDown
Sleep 1000ms
PageDown
Sleep 1500ms

# Back to the key list — Esc exits the cursor without closing the value.
Escape
Sleep 1200ms

Type "q"
Sleep 500ms
