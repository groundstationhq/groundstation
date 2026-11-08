#!/usr/bin/env bash
# The edge's routing and limits, checked against stand-ins for api and ingest
# that answer with their own name. Needs only Docker: `make edge-test`.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
image=caddy:2.11.4
net=gs-edge-test-$$
big=$(mktemp)
cleanup() {
  docker rm -f "$net-edge" "$net-api" "$net-ingest" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  rm -f "$big"
}
trap cleanup EXIT

docker network create "$net" >/dev/null
docker run -d --name "$net-api" --network "$net" --network-alias api "$image" \
  caddy respond --listen :4321 --body api >/dev/null
# Ingest reads the whole body, which is when the edge's size limit applies;
# buffering it in a proxy does the same.
docker run -d --name "$net-ingest" --network "$net" --network-alias ingest "$image" sh -c '
  printf "{\n admin off\n}\n:4320 {\n reverse_proxy 127.0.0.1:4330 {\n  request_buffers 64MiB\n }\n}\n:4330 {\n respond ingest\n}\n" > /tmp/Caddyfile
  exec caddy run --config /tmp/Caddyfile' >/dev/null
docker run -d --name "$net-edge" --network "$net" -p 127.0.0.1::8080 \
  -e API_UPSTREAM=api:4321 -e INGEST_UPSTREAM=ingest:4320 \
  -v "$here/Caddyfile:/etc/caddy/Caddyfile:ro" "$image" >/dev/null
edge="http://$(docker port "$net-edge" 8080/tcp | head -1)"
for _ in $(seq 1 50); do
  curl -s -o /dev/null "$edge/" && break
  sleep 0.2
done

failed=0
# expect WHAT STATUS BODY curl-args...
expect() {
  local what=$1 status=$2 body=$3
  shift 3
  local got
  # curl can exit non-zero after the answer arrived: an edge that refuses a
  # body closes the connection while curl still sends it.
  got=$(curl -s -w ' %{http_code}' "$@") || true
  if [ "$got" = "$body $status" ] || { [ -z "$body" ] && [ "${got##* }" = "$status" ]; }; then
    echo "ok    $what"
  else
    echo "FAIL  $what: expected '$body $status', got '$got'"
    failed=1
  fi
}

expect "uploads go to ingest" 200 ingest -X POST --data '{}' "$edge/v1/events"
expect "reads of /v1/events go to api" 200 api "$edge/v1/events"
expect "the UI and /v1 go to api" 200 api "$edge/v1/health"
expect "writes other than uploads go to api" 200 api -X POST --data '{}' "$edge/v1/tokens"
expect "/auth goes to api" 200 api "$edge/auth/login"
expect "the services' health checks stay internal" 404 "" "$edge/healthz"
head -c $((9 * 1024 * 1024)) /dev/zero > "$big"
expect "an upload over 8 MiB is refused" 413 "" -X POST --data-binary "@$big" "$edge/v1/events"
exit "$failed"
