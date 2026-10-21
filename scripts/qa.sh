#!/usr/bin/env bash
# Full local QA pass: format, lints, unit tests, mock API, SSE and browser UI.
# Usage:
#   bash scripts/qa.sh                 # starts a mock server on 18090
#   BASE_URL=http://127.0.0.1:8080 bash scripts/qa.sh  # use an existing server
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QA_PORT="${QA_PORT:-18090}"
SERVER_PID=""
TMP_DB="/tmp/vibecode-qa-${QA_PORT}.db"

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -f "$TMP_DB" "$TMP_DB-wal" "$TMP_DB-shm"
}
trap cleanup EXIT

step() { printf '\n== %s ==\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null || fail "cargo not found"
command -v curl >/dev/null || fail "curl not found"
command -v jq >/dev/null || fail "jq not found (install jq)"

step "Format"
cargo fmt --check || fail "cargo fmt --check failed"

step "Clippy"
cargo clippy --all-targets -- -D warnings || fail "clippy failed"

step "Unit and integration tests"
cargo test || fail "cargo test failed"

step "Frontend build"
npm --prefix src/frontend run build || fail "frontend build failed"

BASE_URL="${BASE_URL:-}"
if [[ -z "$BASE_URL" ]]; then
  BASE_URL="http://127.0.0.1:${QA_PORT}"
  step "Starting mock server at ${BASE_URL}"
  env MOCK_LLM=1 BIND="127.0.0.1:${QA_PORT}" DATABASE_URL="sqlite://${TMP_DB}" \
    WEB_DIR="./src/frontend/dist" cargo run --quiet --bin travel-ai >/tmp/vibecode-qa.log 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 90); do
    curl -sf "${BASE_URL}/api/health" >/dev/null 2>&1 && break
    sleep 1
  done
  curl -sf "${BASE_URL}/api/health" >/dev/null || fail "mock server did not start (see /tmp/vibecode-qa.log)"
fi
BASE_URL="${BASE_URL%/}"

step "API smoke"
BASE_URL="$BASE_URL" bash scripts/smoke.sh || fail "smoke failed"

step "Browser UI"
BASE_URL="$BASE_URL" bash scripts/frontend_test.sh || fail "frontend test failed"

step "Planning invariants"
explicit="$(curl -sS --max-time 120 -X POST "$BASE_URL/api/plan/sync" \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу с 5 по 8 ноября по Приморью на машине"}')"
echo "$explicit" | jq -e '.refusal == null and (.plan.days | length) == 4' >/dev/null \
  || fail "explicit date range did not produce 4 days: $(echo "$explicit" | jq -c '{intent,refusal}')"
echo "$explicit" | jq -e '[.plan.days[].items[].place_id] as $ids | ($ids|length) == ($ids|unique|length)' >/dev/null \
  || fail "explicit date range has duplicate places"

week="$(curl -sS --max-time 120 -X POST "$BASE_URL/api/plan/sync" \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу на семь дней по Приморью на машине"}')"
echo "$week" | jq -e '( .plan.days | length) == 7' >/dev/null \
  || fail "seven-day request did not produce 7 days"
echo "$week" | jq -e '[.plan.days[].items[].place_id] as $ids | ($ids|length) == ($ids|unique|length)' >/dev/null \
  || fail "seven-day request has duplicate places"

curl -sS --max-time 30 -X POST "$BASE_URL/api/plan/sync" \
  -H 'content-type: application/json' \
  -d '{"query":"Реши мне задачу по алгебре"}' \
  | jq -e '.refusal.category == "off_topic"' >/dev/null \
  || fail "off-topic guardrail did not refuse"

printf '\nAll QA checks passed against %s\n' "$BASE_URL"
