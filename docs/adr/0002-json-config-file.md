# JSON config file, with secrets by reference

Profiles live in a single JSON file at `$XDG_CONFIG_HOME/redis-pane/config.json`, falling back
to `~/.config/redis-pane/config.json`. Credentials are expected as references — `passwordEnv`
naming an environment variable, or `passwordCommand` giving a command to run at connect time. A
literal `password` field is still honoured, but the file is refused outright if it is group- or
world-readable, and Profiles using one are badged in the UI.

## Considered Options

TOML was the obvious pick — no implicit typing, real comments, and a `[profiles.staging]` table
syntax that suits this shape exactly. YAML was the familiar pick. JSON was chosen for
universality and zero parsing ambiguity, accepting that it has no comments.

The lost comments are replaced by a `note` field on each Profile, which the app renders next to
the Connection. This is strictly better than a comment for the purpose people would use one —
"this is the prod box, be careful" is worth seeing while connected, not only while editing.

The permission check follows SSH's StrictModes precedent for private keys. Refusing to load,
rather than warning, is deliberate: a warning in a TUI scrolls away, and this file predictably
ends up inside a symlinked dotfiles repo.

## Consequences

Changing format later is a migration for every user who has hand-written this file, so the shape
should be treated as close to frozen once released.
