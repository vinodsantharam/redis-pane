# The app never writes the config file; session state lives elsewhere

`config.json` is read-only from the application's perspective. There is no "save this connection
as a Profile" action; Profiles are created by editing the file. Session state that the app *does*
persist — last Connection, pane sizes, active filter, scroll position — is written to a separate
state file under `$XDG_STATE_HOME/redis-pane/`, which is never hand-edited.

## Considered Options

Offering an in-app save was the expected design, and would have given a smooth on-ramp from an
Ad-hoc Connection to a Profile. It was rejected to keep the UI light, and because a tool that
rewrites a hand-maintained JSON file will eventually reformat or clobber something a user cared
about. The split means the app can persist freely without ever touching a file the user owns.

## Consequences

Discoverability of the config file has to be carried entirely by other means, since the app
never demonstrates the file by writing to it: first run prints the exact path with a
ready-to-paste example, `--config-path` reports it, and parse failures must name the file with a
line and column rather than reporting "invalid config".
