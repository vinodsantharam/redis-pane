# demo.tape — records a short terminal demo of redis-pane with VHS
# (https://github.com/charmbracelet/vhs — `brew install vhs`).
#
# Shows: tree/flat toggle, filtering, opening a key, and the key updating on
# its own with no keypress or refresh (the CLIENT TRACKING liveness path,
# ADR-0006) — the one thing screenshots can't show and this app is built on.
#
# Render with:
#   vhs scripts/demo.tape
# which produces demo.gif in the repo root.
#
# Prereqs (run once, before recording — see scripts/README.md):
#   ./scripts/redis-up.sh
#   python3 scripts/fixtures.py --flush --seed 7
#   docker exec redis-pane-dev redis-cli HSET demo:live status idle hits 0
#   cargo build --release -p redis-pane
#   export PATH="$PWD/target/release:$PATH"
#
# Afterwards: ./scripts/redis-up.sh down
#
# The recording targets --url redis://localhost:6379 explicitly (not plain
# `redis-pane`) because connection resolution is flags -> default Profile ->
# environment -> 127.0.0.1:6379 (CLAUDE.md) — if you have a default Profile
# configured in ~/.config/redis-pane/config.json, a bare `redis-pane` will
# try that instead of the disposable Redis this tape just seeded.
#
# Keybindings below match ALPHA.md's "What to try" section as of this
# writing. Keybindings are data (CLAUDE.md) and user-overridable, so if a
# step doesn't do what its comment says, check `?` in a live session and
# adjust the tape rather than assuming the recording is stale.

Require redis-pane
Require python3
Require docker

Output demo.gif

Set Shell "bash"
Set FontSize 18
Set Width 1200
Set Height 720
Set Theme "Dracula"
Set Padding 20
Set TypingSpeed 50ms

# Start a background writer so `demo:live` visibly changes later, without
# ever showing this setup command in the recording.
Hide
Type "python3 scripts/churn.py --focus demo:live --focus-share 1 --rate 3 --no-delete --no-create > /tmp/redis-pane-demo-churn.log 2>&1 & disown"
Enter
Sleep 500ms
Type "clear"
Enter
Show

Type "redis-pane --url redis://localhost:6379"
Enter
Sleep 2s

# Tree is the default view — flip to flat, which is what filtering below
# assumes (a flat list needs no group-expansion to reach a leaf).
Type "t"
Sleep 1200ms

# Filter down to the one key this demo cares about. Esc clears the filter
# (ALPHA.md), so open the match with the filter still active — do not Esc
# first.
Type "/"
Sleep 300ms
Type "demo"
Sleep 1500ms
Enter
Sleep 500ms

# Open it and let the liveness path play out on screen.
Right
Sleep 4s

# Copy a ready-to-run redis-cli command for it.
Type "yc"
Sleep 1200ms

# Back out to the list, then quit.
Escape
Sleep 500ms
Type "q"
Sleep 500ms
