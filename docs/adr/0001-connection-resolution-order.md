# Connection resolution order, and never prompting

A Connection's target is resolved in strict precedence: explicit flags (`--url`, `--host`,
`--profile`) beat the config file's default Profile, which beats the environment, which falls
back to `127.0.0.1:6379`. Within the environment, `REDIS_URL` is used wholesale if set;
otherwise the target is assembled from `REDIS_HOST` / `REDIS_PORT` / `REDIS_USER` /
`REDIS_PASSWORD`. The two forms are never merged at the component level. Where sources conflict,
the app resolves deterministically and never prompts — instead the title bar permanently
displays both the resolved target and its Source.

## Considered Options

An earlier draft prompted with a picker whenever a default Profile and `REDIS_URL` disagreed, on
the grounds that silently choosing between two plausible servers is the exact hazard the
Environment model exists to prevent. It was rejected for two reasons: it puts friction on the
hot path of a tool whose headline metric is under one second to a browsable keyspace, and a
prompt that fires routinely trains people to dismiss it unread — which makes it worthless as a
safety mechanism at the moment it matters. `kubectl`, `psql`, and `aws` all resolve a default
deterministically and offer a flag to override; we follow that.

Component-level merging of `REDIS_URL` with the discrete variables was rejected outright. It
produces an unexplainable target when a stale export lingers in a shell.

## Consequences

**The displayed target is redacted; the dialled URL is not.** A `rediss://user:pass@host` URL
would otherwise put a password in the title bar for the whole session — through every
screen-share, screenshot and pasted diagnostic. `Resolution` therefore carries both: `target` for
display with the password replaced by `•••`, and `dial_url` for connecting. Reconstructing either
from the other would mean dialling a redacted URL or showing a secret.

Making the Source visible at all times is not decoration — it is the entire mitigation for
resolving silently. It cannot be dropped for visual tidiness without reopening this decision.

The first-run picker survives, but only for the case where nothing at all is configured.
