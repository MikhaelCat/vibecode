#!/usr/bin/env bash
#
# Docker smoke test for travel-ai.
#
# Builds the image from deploy/Dockerfile (no docker compose), starts a
# throwaway container with the mock LLM enabled, and checks health, a sync
# plan, a refusal, and the SSE stream. The container is removed on exit unless
# KEEP=1 is set.
#
# Usage:
#   scripts/docker_smoke.sh
#   IMAGE=travel-ai:smoke PORT=18090 KEEP=1 scripts/docker_smoke.sh
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
cd "$REPO_ROOT"

IMAGE="${IMAGE:-travel-ai:smoke}"
PORT="${PORT:-18090}"
CONTAINER="${CONTAINER:-travel-ai-smoke}"
KEEP="${KEEP:-0}"

CAR_QUERY="Поездка на машине из Владивостока в Андреевку на два дня: что посмотреть по пути и где заправиться?"
REFUSAL_QUERY="Реши мне задачу по алгебре"

TMP_DIR=""
CONTAINER_STARTED=0

section() {
  printf '\n== %s ==\n' "$*"
}

info() {
  printf '  %s\n' "$*"
}

ok() {
  printf 'OK  %s\n' "$*"
}

fail() {
  printf '\nFAIL: %s\n' "$*" >&2
  exit 1
}

container_running() {
  local running=""
  running="$(docker inspect --format '{{.State.Running}}' "$CONTAINER" 2>/dev/null || true)"
  [[ "$running" == "true" ]]
}

cleanup() {
  local status=$?
  set +e

  if [[ "$status" -ne 0 && "$CONTAINER_STARTED" == "1" ]]; then
    printf '\nContainer logs (last 80 lines):\n' >&2
    docker logs --tail 80 "$CONTAINER" >&2 2>&1 || true
  fi

  if [[ "$KEEP" == "1" ]]; then
    if [[ "$CONTAINER_STARTED" == "1" ]] && container_running; then
      printf '\nKEEP=1: container %s is still running on %s\n' "$CONTAINER" "$BASE_URL"
      printf '  Logs:   docker logs -f %s\n' "$CONTAINER"
      printf '  Remove: docker rm -f %s\n' "$CONTAINER"
    fi
  else
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  fi

  if [[ -n "$TMP_DIR" ]]; then
    rm -rf -- "$TMP_DIR"
  fi

  trap - EXIT
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

section "Configuration"
info "Repo root: $REPO_ROOT"
info "Image:     $IMAGE"
info "Port:      $PORT"
info "Container: $CONTAINER"
info "Keep:      $KEEP"

if [[ ! -f "$REPO_ROOT/deploy/Dockerfile" ]]; then
  fail "deploy/Dockerfile not found under $REPO_ROOT"
fi
if [[ ! "$PORT" =~ ^[0-9]{1,5}$ ]]; then
  fail "PORT must be an integer between 1 and 65535 (got '$PORT')"
fi
PORT=$((10#$PORT))
if (( PORT < 1 || PORT > 65535 )); then
  fail "PORT must be an integer between 1 and 65535 (got '$PORT')"
fi
BASE_URL="http://127.0.0.1:${PORT}"

section "Prerequisites"
for tool in docker curl python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    fail "required command not found: $tool"
  fi
done
if ! docker info >/dev/null 2>&1; then
  fail "docker daemon is not reachable"
fi
ok "docker, curl, and python3 are available"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/travel-ai-docker-smoke.XXXXXX")"

write_query_payload() {
  local query="$1"
  local output="$2"
  python3 - "$query" "$output" <<'PY'
import json
import sys

query, output = sys.argv[1], sys.argv[2]
with open(output, "w", encoding="utf-8") as handle:
    json.dump({"query": query}, handle, ensure_ascii=False)
PY
}

post_json() {
  local path="$1"
  local payload="$2"
  local output="$3"
  local max_time="$4"
  curl --silent --show-error --max-time "$max_time" \
    --request POST "$BASE_URL${path}" \
    --header 'Content-Type: application/json' \
    --data-binary "@${payload}" \
    --output "$output" \
    --write-out '%{http_code}'
}

section "Build image"
info "docker build -f deploy/Dockerfile -t $IMAGE ."
docker build -f deploy/Dockerfile -t "$IMAGE" .
ok "built $IMAGE"

section "Start container"
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
info "removed any old container named $CONTAINER"

if ! container_id="$(docker run -d --name "$CONTAINER" \
    -p "${PORT}:8080" \
    -e MOCK_LLM=1 \
    -e DATABASE_URL=sqlite:///var/lib/travel-ai/app.db \
    "$IMAGE")"; then
  fail "docker run failed for image $IMAGE"
fi
CONTAINER_STARTED=1
info "started container ${container_id:0:12} (host $PORT -> container 8080)"

section "Wait for health"
deadline=$((SECONDS + 30))
healthy=0
while (( SECONDS < deadline )); do
  remaining=$((deadline - SECONDS))
  if (( remaining < 2 )); then
    curl_timeout=$remaining
  else
    curl_timeout=2
  fi
  if curl --silent --show-error --fail --max-time "$curl_timeout" \
      "$BASE_URL/api/health" --output "$TMP_DIR/health.json" 2>/dev/null; then
    healthy=1
    break
  fi
  if ! container_running; then
    fail "container '$CONTAINER' exited before becoming healthy"
  fi
  sleep 1
done
if (( healthy != 1 )); then
  fail "API did not become healthy at $BASE_URL/api/health within 30s"
fi
ok "health endpoint is reachable"

section "Health payload"
if ! python3 - "$TMP_DIR/health.json" <<'PY'
import json
import sys


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        health = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    fail(f"/api/health returned invalid JSON: {error}")

if health.get("status") != "ok":
    fail(f"/api/health status={health.get('status')!r}, expected 'ok'")
if health.get("mock_llm") is not True:
    fail(f"/api/health mock_llm={health.get('mock_llm')!r}, expected true")
print(
    "OK  status=ok mock_llm=true "
    f"version={health.get('version', 'n/a')} model={health.get('model', 'n/a')}"
)
PY
then
  exit 1
fi

section "SPA is served"
if ! curl -fsS "$BASE_URL/" -o "$TMP_DIR/index.html"; then
  fail "GET / did not return the SPA index"
fi
if ! grep -q "vibecode" "$TMP_DIR/index.html"; then
  fail "GET / does not look like the SPA shell"
fi
if ! curl -fsS "$BASE_URL/app.js" -o /dev/null; then
  fail "GET /app.js is not served"
fi
ok "SPA shell and app.js are served"

section "Sync plan: car weekend"
car_payload="$TMP_DIR/car-request.json"
car_response="$TMP_DIR/car-response.json"
write_query_payload "$CAR_QUERY" "$car_payload"

http_code=""
if ! http_code="$(post_json "/api/plan/sync" "$car_payload" "$car_response" 180)"; then
  fail "POST /api/plan/sync failed (connection error or timeout)"
fi
if [[ "$http_code" != "200" ]]; then
  printf 'Response body (truncated):\n' >&2
  head -c 2000 "$car_response" >&2 || true
  printf '\n' >&2
  fail "POST /api/plan/sync returned HTTP $http_code"
fi

if ! python3 - "$car_response" <<'PY'
import json
import sys


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        result = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    fail(f"sync response is not valid JSON: {error}")

plan = result.get("plan")
if plan is None:
    fail("/api/plan/sync returned plan=null")
if not isinstance(plan, dict):
    fail(f"expected plan object, got {type(plan).__name__}")

report = result.get("report")
if not isinstance(report, dict):
    fail("report is missing or not an object")
if report.get("hard_failed") is not False:
    fail(f"report.hard_failed={report.get('hard_failed')!r}, expected false")

days = plan.get("days")
if not isinstance(days, list) or not days:
    fail("plan has no days")

items = 0
for day in days:
    if not isinstance(day, dict):
        fail("plan contains a malformed day")
    day_items = day.get("items")
    if not isinstance(day_items, list):
        fail("plan day is missing an items array")
    items += len(day_items)
if items < 1:
    fail("plan has no items")

usage = result.get("usage")
if not isinstance(usage, dict):
    usage = {}
title = plan.get("title")
if not isinstance(title, str) or not title:
    title = "<untitled>"
print(
    f"OK  title={title} days={len(days)} items={items} "
    f"tokens={usage.get('total_tokens', 'n/a')} "
    f"latency_ms={result.get('latency_ms', 'n/a')}"
)
PY
then
  exit 1
fi

section "Sync refusal: off-topic"
refusal_payload="$TMP_DIR/refusal-request.json"
refusal_response="$TMP_DIR/refusal-response.json"
write_query_payload "$REFUSAL_QUERY" "$refusal_payload"

http_code=""
if ! http_code="$(post_json "/api/plan/sync" "$refusal_payload" "$refusal_response" 180)"; then
  fail "POST /api/plan/sync (refusal) failed (connection error or timeout)"
fi
if [[ "$http_code" != "200" ]]; then
  printf 'Response body (truncated):\n' >&2
  head -c 2000 "$refusal_response" >&2 || true
  printf '\n' >&2
  fail "POST /api/plan/sync (refusal) returned HTTP $http_code"
fi

if ! python3 - "$refusal_response" <<'PY'
import json
import sys


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        result = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    fail(f"refusal response is not valid JSON: {error}")

refusal = result.get("refusal")
if refusal is None:
    fail("refusal is null for an off-topic request")
if not isinstance(refusal, dict):
    fail(f"expected refusal object, got {type(refusal).__name__}")
print(f"OK  refusal category={refusal.get('category', 'unknown')}")
PY
then
  exit 1
fi

section "SSE plan stream: car weekend"
sse_payload="$TMP_DIR/sse-request.json"
sse_response="$TMP_DIR/sse-response.txt"
write_query_payload "$CAR_QUERY" "$sse_payload"

if ! curl -N --silent --show-error --max-time 30 \
    --request POST "$BASE_URL/api/plan" \
    --header 'Accept: text/event-stream' \
    --header 'Content-Type: application/json' \
    --data-binary "@${sse_payload}" \
    --output "$sse_response"; then
  printf 'FAIL: SSE request failed or exceeded the 30s limit.\n' >&2
  if [[ -s "$sse_response" ]]; then
    printf 'Last SSE lines received:\n' >&2
    tail -n 20 "$sse_response" >&2 || true
  fi
  exit 1
fi

missing=()
if ! grep -Eq '^event:[[:space:]]*plan[[:space:]]*$' "$sse_response"; then
  missing+=("plan")
fi
if ! grep -Eq '^event:[[:space:]]*done[[:space:]]*$' "$sse_response"; then
  missing+=("done")
fi
if (( ${#missing[@]} > 0 )); then
  printf 'FAIL: SSE stream is missing event line(s): %s\n' "${missing[*]}" >&2
  printf 'Last SSE lines received:\n' >&2
  tail -n 20 "$sse_response" >&2 || true
  exit 1
fi
ok "SSE stream contained event: plan and event: done"

section "Result"
ok "all docker smoke checks passed for $IMAGE"
info "Health: $BASE_URL/api/health"
