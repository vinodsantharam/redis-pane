# Dev scripts

A disposable Redis and something to put in it, so the TUI can be developed
against a keyspace that moves. Nothing here is part of the build or the test
suites (ADR-0011's integration suite uses `testcontainers` instead); these are
for driving the app by hand.

Python 3 only — no virtualenv, no pip. `resp.py` is a ~100-line RESP2 client
that the two tools share.

```bash
./scripts/redis-up.sh                        # start redis:8.4-alpine on :6379
python3 scripts/fixtures.py --flush          # 5000 keys, all types, 40% with TTLs
cargo run -p redis-pane                      # browse it
python3 scripts/churn.py                     # in a second terminal: keep it moving
./scripts/redis-up.sh down                   # throw it away
```

## `redis-up.sh`

`up` (default) / `down` / `restart` / `cli` / `logs` / `status`. Persistence is
off — the data is disposable, and an unexpected background save is a `-MISCONF`
in the middle of a demo. Override `REDIS_PANE_PORT`, `REDIS_PANE_CONTAINER` or
`REDIS_PANE_IMAGE` in the environment; the image pin can drop to `redis:6.2` to
test the server floor (ADR-0007).

## `fixtures.py`

Writes strings, hashes, lists, sets, sorted sets and streams across ~15
namespaces, with sizes spread over two orders of magnitude so the memory column
is not a flat line. `--ttl-fraction` (default 0.4) of the keys get a TTL
between **10s and 2m**, which is short enough that the countdown visibly moves
and keys really do disappear while you watch.

```bash
python3 scripts/fixtures.py -n 200000            # enough to watch SCAN stream
python3 scripts/fixtures.py --ttl-fraction 1     # everything expires
python3 scripts/fixtures.py --seed 7 --flush     # reproducible keyspace
```

## `churn.py`

Deletes, mutates, expires and creates keys at random, type-appropriately, at a
given rate. This is how liveness gets watched rather than reasoned about: open
a key in redis-pane and the Viewer should follow the server with no refresh
(ADR-0006), and the keys pane should gain and lose rows under the cursor.

```bash
python3 scripts/churn.py --rate 40 --duration 60 # a burst
python3 scripts/churn.py --focus user:1234       # hammer the key you have open
python3 scripts/churn.py --no-delete --no-create # mutations only
```

Both tools take `--host/--port/--username/--password/--db`.
