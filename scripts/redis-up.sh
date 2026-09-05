#!/usr/bin/env bash
# Spawn (or tear down) a throwaway Redis for developing redis-pane.
#
# The image is pinned to a Redis 8 line because ADR-0006's tracking behaviour is
# verified against 8.4.0; the server floor is RESP3 / Redis 6.0 (ADR-0007), so a
# lower pin is a legitimate thing to test against, but not the default.
set -euo pipefail

NAME="${REDIS_PANE_CONTAINER:-redis-pane-dev}"
PORT="${REDIS_PANE_PORT:-6379}"
IMAGE="${REDIS_PANE_IMAGE:-redis:8.4-alpine}"

usage() {
  cat <<'USAGE'
usage: scripts/redis-up.sh [up|down|restart|cli|logs|status]

  up       start the container (idempotent) and wait for PONG
  down     stop and remove it, discarding the keyspace
  restart  down, then up
  cli      open redis-cli -3 inside the container
  logs     follow the server log
  status   print container state and DBSIZE

env:
  REDIS_PANE_CONTAINER  container name  (default redis-pane-dev)
  REDIS_PANE_PORT       host port       (default 6379)
  REDIS_PANE_IMAGE      image           (default redis:8.4-alpine)
USAGE
}

running() { [ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)" = true ]; }
exists()  { docker inspect "$NAME" >/dev/null 2>&1; }

up() {
  if running; then
    echo "$NAME already running on port $PORT"
  else
    exists && docker rm -f "$NAME" >/dev/null
    # No persistence: this data is disposable, and an unexpected RDB save is a
    # -MISCONF waiting to happen in the middle of a demo.
    docker run -d --name "$NAME" -p "$PORT:6379" "$IMAGE" \
      redis-server --save "" --appendonly no --notify-keyspace-events KEA >/dev/null
    echo "started $NAME ($IMAGE) on port $PORT"
  fi
  printf 'waiting for redis'
  for _ in $(seq 1 50); do
    if docker exec "$NAME" redis-cli ping 2>/dev/null | grep -q PONG; then
      echo " … ready"
      docker exec "$NAME" redis-cli INFO server | grep -i '^redis_version' | tr -d '\r'
      echo "next: python3 scripts/fixtures.py --port $PORT"
      return 0
    fi
    printf .
    sleep 0.2
  done
  echo; echo "redis did not answer PING in time; see: scripts/redis-up.sh logs" >&2
  exit 1
}

case "${1:-up}" in
  up)      up ;;
  down)    docker rm -f "$NAME" >/dev/null 2>&1 && echo "removed $NAME" || echo "$NAME not present" ;;
  restart) "$0" down; "$0" up ;;
  cli)     shift; exec docker exec -it "$NAME" redis-cli -3 "$@" ;;
  logs)    exec docker logs -f "$NAME" ;;
  status)
    if running; then
      echo "$NAME: running on port $PORT"
      docker exec "$NAME" redis-cli DBSIZE
    else
      echo "$NAME: not running"
    fi ;;
  -h|--help|help) usage ;;
  *) usage; exit 2 ;;
esac
