#!/usr/bin/env python3
"""Fill a dev Redis with a keyspace worth browsing.

Every Redis type the Viewers care about is represented, key names are
namespaced so the filter has something to bite on, sizes vary by two orders of
magnitude so the memory column is not a flat line, and a share of the keys
carry a TTL between 10s and 2m so the countdown visibly moves while you watch.

  python3 scripts/fixtures.py                 # 5000 keys, 40% with a TTL
  python3 scripts/fixtures.py -n 200000       # a keyspace big enough to scan
  python3 scripts/fixtures.py --ttl-fraction 1 --flush
"""

import argparse
import random
import string
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from resp import add_target_args, connect  # noqa: E402

TTL_MIN_SECONDS = 10
TTL_MAX_SECONDS = 120

WORDS = (
    "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda sigma orion "
    "vega rigel atlas hermes juno pluto vesta ceres pallas europa titan rhea"
).split()

NAMESPACES = [
    ("session", "string"),
    ("cache:page", "string"),
    ("cache:user", "hash"),
    ("user", "hash"),
    ("queue", "list"),
    ("feed", "list"),
    ("tag", "set"),
    ("online", "set"),
    ("leaderboard", "zset"),
    ("rank:daily", "zset"),
    ("events", "stream"),
    ("audit", "stream"),
    ("counter", "string"),
    ("flag", "string"),
    ("lock", "string"),
]


def blob(rng, n):
    return "".join(rng.choice(string.ascii_letters + string.digits) for _ in range(n))


def sentence(rng, n=6):
    return " ".join(rng.choice(WORDS) for _ in range(n))


def size_for(rng):
    """Log-ish size distribution: mostly small, a few fat keys."""
    roll = rng.random()
    if roll < 0.70:
        return rng.randint(1, 12)
    if roll < 0.95:
        return rng.randint(12, 120)
    return rng.randint(120, 1500)


def commands_for(rng, key, kind):
    n = size_for(rng)
    if kind == "string":
        if key.startswith("counter:"):
            return [("SET", key, rng.randint(0, 10_000_000))]
        if key.startswith("flag:"):
            return [("SET", key, rng.choice(["true", "false", "enabled", "off"]))]
        if key.startswith("lock:"):
            return [("SET", key, "held-by-worker-%d" % rng.randint(1, 32))]
        return [("SET", key, blob(rng, n * rng.choice([8, 32, 200])))]
    if kind == "hash":
        fields = []
        for i in range(min(n, 200)):
            fields += ["field:%s:%d" % (rng.choice(WORDS), i), sentence(rng, 3)]
        return [("HSET", key, *fields)]
    if kind == "list":
        return [("RPUSH", key, *[sentence(rng) for _ in range(min(n, 500))])]
    if kind == "set":
        return [("SADD", key, *{"%s-%d" % (rng.choice(WORDS), rng.randint(0, 9999)) for _ in range(min(n, 500))})]
    if kind == "zset":
        members = []
        for i in range(min(n, 500)):
            members += [round(rng.uniform(0, 10_000), 3), "player:%d" % rng.randint(1, 100_000)]
        return [("ZADD", key, *members)]
    if kind == "stream":
        out = []
        for _ in range(min(n, 100)):
            out.append(
                (
                    "XADD", key, "*",
                    "event", rng.choice(["login", "logout", "purchase", "refund", "view"]),
                    "user", rng.randint(1, 100_000),
                    "amount", round(rng.uniform(0, 500), 2),
                )
            )
        return out
    raise ValueError(kind)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    add_target_args(p)
    p.add_argument("-n", "--count", type=int, default=5000, help="keys to create (default 5000)")
    p.add_argument("--ttl-fraction", type=float, default=0.4,
                   help="share of keys given a %d-%ds TTL (default 0.4)" % (TTL_MIN_SECONDS, TTL_MAX_SECONDS))
    p.add_argument("--prefix", default="", help="prepend to every key name")
    p.add_argument("--flush", action="store_true", help="FLUSHDB first")
    p.add_argument("--seed", type=int, default=None, help="seed the RNG for a reproducible keyspace")
    p.add_argument("--batch", type=int, default=500, help="commands per pipeline flush")
    args = p.parse_args()

    rng = random.Random(args.seed)
    r = connect(args)

    if args.flush:
        r.call("FLUSHDB")
        print("flushed db %d" % args.db)

    pending, made, expiring = [], 0, 0
    started = time.time()

    def flush():
        nonlocal pending
        for reply in r.pipeline(pending):
            if isinstance(reply, Exception):
                print("  ! %s" % reply, file=sys.stderr)
        pending = []

    for i in range(args.count):
        ns, kind = NAMESPACES[i % len(NAMESPACES)]
        key = "%s%s:%s" % (args.prefix, ns, blob(rng, 10) if rng.random() < 0.5 else rng.randint(1, 999_999))
        pending += commands_for(rng, key, kind)
        if rng.random() < args.ttl_fraction:
            pending.append(("EXPIRE", key, rng.randint(TTL_MIN_SECONDS, TTL_MAX_SECONDS)))
            expiring += 1
        made += 1
        if len(pending) >= args.batch:
            flush()
    flush()

    print(
        "created %d keys (%d with a %d-%ds TTL) in %.1fs; DBSIZE now %s"
        % (made, expiring, TTL_MIN_SECONDS, TTL_MAX_SECONDS, time.time() - started, r.call("DBSIZE"))
    )
    r.close()


if __name__ == "__main__":
    main()
