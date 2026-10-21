#!/usr/bin/env bash
#
# Run the fixed eval suite against every pipeline config.
# Mock mode is the default; LIVE=1 enables real LLM calls and requires a token.
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DATASET="data/eval/queries.jsonl"
RESULTS_DIR="results"
CONFIGS=(
  "configs/v1_baseline.toml"
  "configs/v2_compact.toml"
  "configs/v3_fast.toml"
)

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

[[ -f "$DATASET" ]] || fail "dataset not found: $DATASET"
for config in "${CONFIGS[@]}"; do
  [[ -f "$config" ]] || fail "config not found: $config"
done

LIVE="${LIVE:-0}"
case "$LIVE" in
  0)
    export MOCK_LLM=1
    live_args=()
    mode="mock (MOCK_LLM=1, no API spend)"
    ;;
  1)
    if [[ -z "${LLM_PROXY_TOKEN:-}" ]]; then
      fail "LIVE=1 requires LLM_PROXY_TOKEN to be set (the value is never printed)."
    fi
    export MOCK_LLM=0
    live_args=(--live)
    mode="live (LLM_PROXY_TOKEN is set)"
    ;;
  *)
    fail "LIVE must be 0 or 1, got '$LIVE'"
    ;;
esac

mkdir -p "$RESULTS_DIR"

printf 'Repo:    %s\n' "$ROOT_DIR"
printf 'Results: %s\n' "$ROOT_DIR/$RESULTS_DIR"
printf 'Mode:    %s\n' "$mode"

names=()
for config in "${CONFIGS[@]}"; do
  base="${config##*/}"
  name="${base%.toml}"
  json_path="$RESULTS_DIR/${name}.json"
  markdown_path="$RESULTS_DIR/${name}.md"

  printf '\n== %s ==\n' "$name"
  cmd=(
    cargo run --release --bin eval --
    --config "$config"
    --dataset "$DATASET"
    --out "$json_path"
    --markdown "$markdown_path"
  )
  if (( ${#live_args[@]} > 0 )); then
    cmd+=("${live_args[@]}")
  fi

  if ! "${cmd[@]}"; then
    fail "eval failed for $config; stopping on first failure."
  fi

  [[ -s "$json_path" ]] || fail "eval did not produce $json_path"
  [[ -s "$markdown_path" ]] || fail "eval did not produce $markdown_path"

  if ! python3 - "$name" "$json_path" <<'PY'
import json
import sys

name = sys.argv[1]
path = sys.argv[2]
try:
    with open(path, encoding="utf-8") as handle:
        payload = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    print(f"FAIL: cannot read {path}: {error}", file=sys.stderr)
    raise SystemExit(1)

summary = payload.get("summary")
if not isinstance(summary, dict):
    print(f"FAIL: {path} has no summary object", file=sys.stderr)
    raise SystemExit(1)

keys = (
    "hard_pass_rate",
    "tool_choice_rate",
    "intent_fit_rate",
    "refusal_rate",
    "avg_tokens",
    "p95_latency_ms",
)
missing = [key for key in keys if not isinstance(summary.get(key), (int, float))]
if missing:
    print(
        f"FAIL: {path} summary is missing numeric field(s): {', '.join(missing)}",
        file=sys.stderr,
    )
    raise SystemExit(1)

print(
    f"{name}: "
    f"hard_pass_rate={summary['hard_pass_rate']:.1%} "
    f"tool_choice_rate={summary['tool_choice_rate']:.1%} "
    f"intent_fit_rate={summary['intent_fit_rate']:.1%} "
    f"refusal_rate={summary['refusal_rate']:.1%} "
    f"avg_tokens={summary['avg_tokens']:.1f} "
    f"p95_latency_ms={summary['p95_latency_ms']:.0f}"
)
PY
  then
    exit 1
  fi

  names+=("$name")
done

printf '\nAll eval configs completed (%s).\n' "$mode"
printf 'Artifacts:\n'
for name in "${names[@]}"; do
  printf '  %s\n' "$ROOT_DIR/$RESULTS_DIR/${name}.json"
  printf '  %s\n' "$ROOT_DIR/$RESULTS_DIR/${name}.md"
done
