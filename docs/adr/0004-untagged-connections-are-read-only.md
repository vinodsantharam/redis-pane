# Untagged remote connections start read-only

An Ad-hoc Connection has no Profile, so nothing supplies its Environment. When the target is
loopback, `::1`, or a unix socket it is inferred as `local` with full write access. Everything
else becomes a fourth Environment, `unknown`: distinct chrome, Read-only Mode on at startup,
lifted with a single keypress.

## Considered Options

Treating untagged as `local` was rejected because `--url redis://cache-prod-01:6379` is exactly
what gets typed during an incident, and it would arrive with neutral chrome and full write
access. Treating untagged as `prod` was rejected in the other direction: it makes the ordinary
localhost launch hostile in order to protect the rarest case.

## Consequences

`unknown` is a real Environment with its own colour, not a null state, and every place that
switches on Environment must handle four cases rather than three.

Read-only being one keypress away is what keeps this from being paternalistic — the decision
relies on that escape hatch staying cheap.
