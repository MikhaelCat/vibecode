#!/usr/bin/env bash
#
# Smoke-test a running vibecode API. The server is expected to be up already.
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
BASE_URL="${BASE_URL%/}"

CAR_QUERY="Поездка на машине из Владивостока в Андреевку на два дня: что посмотреть по пути и где заправиться?"
OFFTOPIC_QUERY="Реши мне задачу по алгебре"

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf -- "$TMP_DIR"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

section() {
  printf '\n== %s ==\n' "$*"
}

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

section "Health"
health_json="$TMP_DIR/health.json"
if ! curl --silent --show-error --fail --max-time 10 \
    "$BASE_URL/api/health" --output "$health_json"; then
  printf 'FAIL: GET %s/api/health failed.\n' "$BASE_URL" >&2
  printf 'Hint: start the API (for example, `MOCK_LLM=1 cargo run` in the repo root),\n' >&2
  printf 'then retry, or set BASE_URL to the correct server.\n' >&2
  exit 1
fi

if ! python3 - "$health_json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        health = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    print(f"FAIL: /api/health returned invalid JSON: {error}", file=sys.stderr)
    raise SystemExit(1)

status = health.get("status")
model = health.get("model")
mock = health.get("mock_llm")
if status != "ok":
    print(f"FAIL: /api/health status={status!r}, expected 'ok'", file=sys.stderr)
    raise SystemExit(1)
if not isinstance(model, str) or not model:
    print("FAIL: /api/health is missing its 'model' field", file=sys.stderr)
    raise SystemExit(1)
if not isinstance(mock, bool):
    print("FAIL: /api/health is missing a boolean 'mock_llm' field", file=sys.stderr)
    raise SystemExit(1)
print(f"OK  status={status} model={model} mock={str(mock).lower()}")
PY
then
  exit 1
fi

section "Sync plan: car weekend"
car_payload="$TMP_DIR/car-request.json"
car_response="$TMP_DIR/car-response.json"
write_query_payload "$CAR_QUERY" "$car_payload"
http_code=""
if ! http_code="$(curl --silent --show-error --max-time 180 \
    --request POST "$BASE_URL/api/plan/sync" \
    --header 'Content-Type: application/json' \
    --data-binary "@$car_payload" \
    --output "$car_response" \
    --write-out '%{http_code}')"; then
  printf 'FAIL: POST %s/api/plan/sync failed (connection error or timeout).\n' "$BASE_URL" >&2
  exit 1
fi
if [[ "$http_code" != "200" ]]; then
  printf 'FAIL: POST /api/plan/sync returned HTTP %s.\n' "$http_code" >&2
  printf 'Response body (truncated):\n' >&2
  head -c 2000 "$car_response" >&2 || true
  printf '\n' >&2
  exit 1
fi

if ! python3 - "$car_response" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        result = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    print(f"FAIL: sync response is not valid JSON: {error}", file=sys.stderr)
    raise SystemExit(1)


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


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
write_query_payload "$OFFTOPIC_QUERY" "$refusal_payload"
http_code=""
if ! http_code="$(curl --silent --show-error --max-time 180 \
    --request POST "$BASE_URL/api/plan/sync" \
    --header 'Content-Type: application/json' \
    --data-binary "@$refusal_payload" \
    --output "$refusal_response" \
    --write-out '%{http_code}')"; then
  printf 'FAIL: POST %s/api/plan/sync failed (connection error or timeout).\n' "$BASE_URL" >&2
  exit 1
fi
if [[ "$http_code" != "200" ]]; then
  printf 'FAIL: POST /api/plan/sync returned HTTP %s.\n' "$http_code" >&2
  printf 'Response body (truncated):\n' >&2
  head -c 2000 "$refusal_response" >&2 || true
  printf '\n' >&2
  exit 1
fi

if ! python3 - "$refusal_response" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        result = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    print(f"FAIL: refusal response is not valid JSON: {error}", file=sys.stderr)
    raise SystemExit(1)


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


refusal = result.get("refusal")
if refusal is None:
    fail("refusal is null for an off-topic request")
if not isinstance(refusal, dict):
    fail(f"expected refusal object, got {type(refusal).__name__}")
if result.get("plan") is not None:
    fail("plan is not null for an off-topic request")
print(f"OK  refusal category={refusal.get('category', 'unknown')} plan=null")
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
    --data-binary "@$sse_payload" \
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
printf 'OK  SSE stream contained event: plan and event: done\n'

printf '\nAll smoke checks passed against %s\n' "$BASE_URL"
