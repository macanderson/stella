#!/usr/bin/env bash
#
# Smoke: run the packaged stella-serve image far enough to prove it actually
# serves. See #635.
#
# packaging/docker/Dockerfile.serve shipped on main having never been built.
# It asserts four runtime preconditions and nothing checked any of them:
#
#   1. the binary starts under an unprivileged uid (USER 10001),
#   2. it binds 8080 inside the container,
#   3. the bearer-token gate admits a good token and refuses everything else,
#   4. `stella-serve healthcheck` — the image's HEALTHCHECK, which carries no
#      curl or wget — can reach the server it is probing.
#
# Each is plausible; "plausible" is how an image reaches a user broken. This
# script exercises all four against a tag you have already built, so the same
# assertions run in CI (.github/workflows/docker-serve.yml) and on a laptop:
#
#   docker build -f packaging/docker/Dockerfile.serve -t stella-serve:ci .
#   ./scripts/smoke-serve-image.sh stella-serve:ci
#
# The token is generated per run and passed by environment, never baked into
# the image. Container logs are dumped on every exit path — a failure here is
# usually a startup error the container printed and took to its grave.
set -euo pipefail

image="${1:-stella-serve:ci}"
# Host-side port only; the container side is fixed at 8080 on purpose, because
# "does it serve on 8080" is one of the things under test.
host_port="${STELLA_SERVE_SMOKE_PORT:-18080}"
base="http://127.0.0.1:${host_port}"
token="$(openssl rand -hex 32)"

cid=""
cleanup() {
  status=$?
  if [ -n "$cid" ]; then
    echo "--- container logs ---"
    docker logs "$cid" 2>&1 || true
    docker rm --force "$cid" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT

fail() {
  echo "smoke-serve-image: $*" >&2
  exit 1
}

echo "smoke-serve-image: running $image on 127.0.0.1:${host_port}"
cid="$(docker run --detach \
  --env STELLA_SERVE_TOKEN="$token" \
  --publish "127.0.0.1:${host_port}:8080" \
  "$image")"

# Readiness, bounded. The loop exits early if the container is gone: a config
# error (missing token, unparseable bind) exits non-zero in milliseconds, and
# waiting the full timeout for a corpse only buries the log that explains it.
ready=""
for _ in $(seq 1 60); do
  if [ "$(docker inspect --format '{{.State.Running}}' "$cid")" != "true" ]; then
    fail "the container exited before it served anything"
  fi
  if [ "$(curl --silent --output /dev/null --write-out '%{http_code}' "${base}/healthz" || true)" = "200" ]; then
    ready=1
    break
  fi
  sleep 1
done
[ -n "$ready" ] || fail "no 200 from ${base}/healthz within 60s"

# 1. It serves on 8080 — the request above crossed a host:8080 port mapping, so
#    the listener is on 8080 inside the container, not merely "somewhere".
health="$(curl --silent --show-error "${base}/healthz")"
case "$health" in
*'"status":"ok"'*) echo "ok: /healthz on 8080 -> $health" ;;
*) fail "/healthz answered 200 with an unexpected body: $health" ;;
esac

# 2. An unauthenticated write is refused. Sent to a real endpoint, not a
#    made-up path, so a 401 proves the token gate and not a 404 in disguise.
status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --request POST "${base}/v1/turns" \
  --header 'content-type: application/json' \
  --data '{}')"
[ "$status" = "401" ] || fail "unauthenticated POST /v1/turns returned $status, want 401"
echo "ok: unauthenticated POST /v1/turns -> 401"

# 3. An authenticated write succeeds. A turn is the smallest real unit of work
#    the wire protocol has; it parks waiting for the host to answer its first
#    reverse request, which is exactly what a host-driven sidecar should do.
turn="$(curl --silent --show-error --write-out '\n%{http_code}' \
  --request POST "${base}/v1/turns" \
  --header "authorization: Bearer ${token}" \
  --header 'content-type: application/json' \
  --data '{"provider_id":"smoke","messages":[{"role":"user","content":"ping"}],"max_steps":1,"reverse_request_timeout_ms":1000}')"
status="$(printf '%s' "$turn" | tail -n 1)"
body="$(printf '%s' "$turn" | sed '$d')"
[ "$status" = "200" ] || fail "authenticated POST /v1/turns returned $status: $body"
case "$body" in
*'"turn_id"'*) echo "ok: authenticated POST /v1/turns -> 200 $body" ;;
*) fail "authenticated POST /v1/turns returned 200 without a turn_id: $body" ;;
esac

# Hand the turn back rather than leaving it to time out on its own deadline —
# and take a second authenticated round trip while we are here.
turn_id="$(printf '%s' "$body" | sed -n 's/.*"turn_id":"\([^"]*\)".*/\1/p')"
[ -n "$turn_id" ] || fail "could not read turn_id out of: $body"
status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --request POST "${base}/v1/turns/${turn_id}/cancel" \
  --header "authorization: Bearer ${token}")"
[ "$status" = "200" ] || fail "cancelling ${turn_id} returned $status, want 200"
echo "ok: authenticated POST /v1/turns/${turn_id}/cancel -> 200"

# 4. It runs unprivileged. Two questions, two answers: `id -u` reports the uid
#    a command lands on, while /proc/1/status reports the uid the *server* is
#    actually running under — the one that matters, and the one an image whose
#    USER was overridden or ignored would fail.
uid="$(docker exec "$cid" id -u | tr -d '\r')"
[ "$uid" = "10001" ] || fail "docker exec id -u printed '$uid', want 10001"
server_uid="$(docker exec "$cid" awk '/^Uid:/ { print $2 }' /proc/1/status | tr -d '\r')"
[ "$server_uid" = "10001" ] || fail "the server process runs as uid '$server_uid', want 10001"
echo "ok: uid 10001 (exec and pid 1)"

# 5. The image's own HEALTHCHECK works: first the probe binary directly, then
#    the verdict Docker forms from it. Docker's status is the end-to-end proof
#    — it runs the declared CMD, in the image, with the image's environment.
docker exec "$cid" /usr/local/bin/stella-serve healthcheck \
  || fail "stella-serve healthcheck exited non-zero inside the container"
healthy=""
for _ in $(seq 1 30); do
  case "$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$cid")" in
  healthy)
    healthy=1
    break
    ;;
  unhealthy) fail "the container's HEALTHCHECK reported unhealthy" ;;
  esac
  sleep 2
done
[ -n "$healthy" ] || fail "the container never reported healthy within 60s"
echo "ok: HEALTHCHECK -> healthy"

# Nothing above should have knocked it over.
[ "$(docker inspect --format '{{.State.Running}}' "$cid")" = "true" ] \
  || fail "the container is no longer running at the end of the smoke"

echo "smoke-serve-image: OK — $image serves on 8080 as uid 10001."
