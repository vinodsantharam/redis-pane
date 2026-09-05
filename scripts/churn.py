#!/usr/bin/env python3
"""Keep a dev keyspace moving: delete, mutate, expire and create keys at random.

This is the other half of the fixtures: it exists so liveness can be watched
rather than reasoned about. Open a key in redis-pane, run this against it, and
the Viewer should follow the server without a refresh (ADR-0006); leave the keys
pane open and rows should appear, change and vanish under the cursor.

  python3 scripts/churn.py                          # 5 ops/s, until ^C
  python3 scripts/churn.py --rate 40 --duration 60  # a stress burst
  python3 scripts/churn.py --focus user:1234        # hammer one key
  python3 scripts/churn.py --no-delete --no-create  # mutations only
"""

import argparse
import random
import signal
import string
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from resp import RedisError, add_target_args, connect  # noqa: E402

TTL_MIN_SECONDS = 10
TTL_MAX_SECONDS = 120

WORDS = "alpha beta gamma delta epsilon zeta eta theta orion vega rigel atlas hermes juno".split()

running = True


def stop(*_):
    global running
    running = False


def blob(rng, n=16):
    return "".join(rng.choice(string.ascii_letters + string.digits) for _ in range(n))


def mutate(r, rng, key):
    """Type-appropriate in-place change. Returns a one-line description, or None
    if the key vanished between the pick and the write."""
    try:
        kind = r.call("TYPE", key)
    except RedisError:
        return None
    if kind == "none":
        return None
    if kind == "string":
        if rng.random() < 0.3:
            try:
                return "INCRBY %s -> %s" % (key, r.call("INCRBY", key, rng.randint(-50, 50)))
            except RedisError:
                pass
        r.call("SET", key, blob(rng, rng.randint(8, 400)))
        return "SET %s" % key
    if kind == "hash":
        field = "field:%s" % rng.choice(WORDS)
        if rng.random() < 0.25:
            r.call("HDEL", key, field)
            return "HDEL %s %s" % (key, field)
        r.call("HSET", key, field, blob(rng, 24))
        return "HSET %s %s" % (key, field)
    if kind == "list":
        if rng.random() < 0.4:
            r.call("LPOP", key)
            return "LPOP %s" % key
        r.call("RPUSH", key, "%s-%d" % (rng.choice(WORDS), rng.randint(0, 9999)))
        return "RPUSH %s" % key
    if kind == "set":
        member = "%s-%d" % (rng.choice(WORDS), rng.randint(0, 999))
        if rng.random() < 0.3:
            r.call("SREM", key, member)
            return "SREM %s" % key
        r.call("SADD", key, member)
        return "SADD %s" % key
    if kind == "zset":
        member = "player:%d" % rng.randint(1, 100_000)
        if rng.random() < 0.25:
            r.call("ZREM", key, member)
            return "ZREM %s" % key
        r.call("ZADD", key, round(rng.uniform(0, 10_000), 3), member)
        return "ZADD %s" % key
    if kind == "stream":
        r.call("XADD", key, "*", "event", rng.choice(["login", "purchase", "refund"]),
               "user", rng.randint(1, 100_000), "amount", round(rng.uniform(0, 500), 2))
        return "XADD %s" % key
    return None


def create(r, rng, prefix):
    ns, kind = rng.choice([
        ("session", "string"), ("cache:page", "string"), ("user", "hash"),
        ("queue", "list"), ("tag", "set"), ("leaderboard", "zset"), ("events", "stream"),
    ])
    key = "%s%s:%s" % (prefix, ns, blob(rng, 10))
    if kind == "string":
        r.call("SET", key, blob(rng, rng.randint(16, 512)))
    elif kind == "hash":
        r.call("HSET", key, "created", int(time.time()), "who", rng.choice(WORDS))
    elif kind == "list":
        r.call("RPUSH", key, *[rng.choice(WORDS) for _ in range(rng.randint(1, 20))])
    elif kind == "set":
        r.call("SADD", key, *{rng.choice(WORDS) for _ in range(rng.randint(1, 8))})
    elif kind == "zset":
        r.call("ZADD", key, round(rng.uniform(0, 100), 2), "player:%d" % rng.randint(1, 9999))
    else:
        r.call("XADD", key, "*", "event", "created")
    if rng.random() < 0.5:
        r.call("EXPIRE", key, rng.randint(TTL_MIN_SECONDS, TTL_MAX_SECONDS))
    return "CREATE %s (%s)" % (key, kind)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    add_target_args(p)
    p.add_argument("--rate", type=float, default=5.0, help="operations per second (default 5)")
    p.add_argument("--duration", type=float, default=0, help="seconds to run; 0 means until ^C")
    p.add_argument("--focus", action="append", default=[],
                   help="key to mutate preferentially; repeatable")
    p.add_argument("--focus-share", type=float, default=0.5,
                   help="share of ops aimed at --focus keys (default 0.5)")
    p.add_argument("--prefix", default="", help="prefix for keys this script creates")
    p.add_argument("--seed", type=int, default=None)
    p.add_argument("--no-delete", action="store_true")
    p.add_argument("--no-create", action="store_true")
    p.add_argument("--no-expire", action="store_true", help="never touch TTLs")
    p.add_argument("-q", "--quiet", action="store_true", help="counters only, no per-op log")
    args = p.parse_args()

    rng = random.Random(args.seed)
    r = connect(args)
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)

    weights = [("mutate", 5.0)]
    if not args.no_delete:
        weights.append(("delete", 2.0))
    if not args.no_create:
        weights.append(("create", 2.0))
    if not args.no_expire:
        weights.append(("expire", 1.0))
    kinds = [k for k, _ in weights]
    probs = [w for _, w in weights]

    counts = dict.fromkeys(kinds, 0)
    counts["skipped"] = 0
    interval = 1.0 / args.rate if args.rate > 0 else 0
    deadline = time.time() + args.duration if args.duration else None
    started = time.time()
    print("churning %s:%d at %.1f op/s%s — ^C to stop"
          % (args.host, args.port, args.rate, " (focus: %s)" % ", ".join(args.focus) if args.focus else ""))

    while running and (deadline is None or time.time() < deadline):
        op = rng.choices(kinds, probs)[0]
        key = None
        if args.focus and rng.random() < args.focus_share:
            key, op = rng.choice(args.focus), "mutate"
        elif op != "create":
            key = r.call("RANDOMKEY")
            key = key.decode(errors="replace") if isinstance(key, bytes) else key

        try:
            if op == "create":
                line = create(r, rng, args.prefix)
            elif key is None:
                line = None
            elif op == "delete":
                line = "DEL %s" % key if r.call("DEL", key) else None
            elif op == "expire":
                ttl = rng.randint(TTL_MIN_SECONDS, TTL_MAX_SECONDS)
                line = "EXPIRE %s %ds" % (key, ttl) if r.call("EXPIRE", key, ttl) else None
            else:
                line = mutate(r, rng, key)
        except RedisError as e:
            print("  ! %s" % e, file=sys.stderr)
            line = None

        counts[op if line else "skipped"] += 1
        if line and not args.quiet:
            print("  %s" % line)
        if interval:
            time.sleep(interval)

    print("\nstopped after %.1fs: %s" % (
        time.time() - started, ", ".join("%s=%d" % kv for kv in sorted(counts.items()))))
    r.close()


if __name__ == "__main__":
    main()
