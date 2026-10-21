#!/usr/bin/env bash
# Headless frontend test: static shell + SSE plan rendering + refusal UI.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
CHROME="${CHROME:-chromium}"
TMP_DIR="$(mktemp -d)"
PORT=$((9300 + RANDOM % 200))
CHROME_PID=""

cleanup() {
  [[ -n "$CHROME_PID" ]] && kill "$CHROME_PID" 2>/dev/null || true
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "OK  $*"; }

command -v "$CHROME" >/dev/null || fail "chromium not found (set CHROME=...)"
curl -fsS "$BASE_URL/api/health" -o "$TMP_DIR/health.json" || fail "backend is not running at $BASE_URL"
grep -q '"status":"ok"' "$TMP_DIR/health.json" || fail "backend health is not ok"

(cd "$ROOT/src/frontend" && mkdir -p dist && cp index.html styles.css app.js dist/)
[[ -f "$ROOT/src/frontend/dist/index.html" ]] || fail "frontend build failed"
ok "frontend built"

curl -fsS "$BASE_URL/" -o "$TMP_DIR/index.html" || fail "GET / is not served"
grep -q 'vibecode' "$TMP_DIR/index.html" || fail "GET / is not the SPA shell"
curl -fsS "$BASE_URL/app.js" -o /dev/null || fail "GET /app.js is not served"
ok "SPA shell and app.js served"

"$CHROME" --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage \
  --remote-debugging-address=127.0.0.1 --remote-debugging-port="$PORT" \
  --user-data-dir="$TMP_DIR/chrome" --window-size=1440,1200 "$BASE_URL/" \
  >"$TMP_DIR/chrome.log" 2>&1 &
CHROME_PID=$!

for _ in $(seq 1 40); do
  curl -fsS "http://127.0.0.1:$PORT/json" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -fsS "http://127.0.0.1:$PORT/json" >/dev/null || fail "chromium CDP did not start"
sleep 3

python3 - "$PORT" "$BASE_URL" <<'PY'
import asyncio, json, sys, urllib.request
import websockets

port, base = sys.argv[1], sys.argv[2]

async def main():
    targets = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json"))
    pages = [t for t in targets if t.get("type") == "page" and t.get("url", "").startswith(base)]
    if not pages:
        raise SystemExit("no page target")
    async with websockets.connect(pages[0]["webSocketDebuggerUrl"], max_size=20_000_000) as ws:
        js = """(async () => {
          const q = document.getElementById('q');
          q.value = 'Хочу на выходные по Приморью на машине, не больше двух дней';
          document.getElementById('form').requestSubmit();
          for (let i = 0; i < 100; i++) {
            await new Promise(r => setTimeout(r, 400));
            if (!document.getElementById('result').hidden) {
              const meta = document.getElementById('p-meta').innerText;
              return JSON.stringify({
                ok: true,
                title: document.getElementById('p-title').textContent,
                meta,
                tabs: [...document.querySelectorAll('#tabs .tab')].map(x => x.textContent),
                stages: [...document.querySelectorAll('#stages li')].map(x => x.textContent),
                stops: document.querySelectorAll('#stops .stop').length,
                checks: document.getElementById('checks').innerText
              });
            }
          }
          return JSON.stringify({ok: false, stages: [...document.querySelectorAll('#stages li')].map(x => x.textContent)});
        })()"""
        await ws.send(json.dumps({"id": 1, "method": "Runtime.evaluate", "params": {
            "expression": js, "awaitPromise": True, "returnByValue": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 1:
                if "result" not in msg:
                    raise SystemExit(f"CDP evaluate failed: {json.dumps(msg)[:1200]}")
                value = msg["result"]["result"].get("value")
                data = json.loads(value)
                if not data.get("ok"):
                    raise SystemExit(f"plan did not render: {data}")
                if "Проверки пройдены" not in data["checks"]:
                    raise SystemExit(f"validation not rendered: {data['checks']}")
                if "осадки" not in data["meta"]:
                    raise SystemExit(f"weather not rendered: {data['meta']}")
                if "мок" in data["meta"].lower() or "mock-llm" in data["title"].lower():
                    raise SystemExit(f"mock labels must be hidden: {data}")
                if any("День 1: День 1" in t for t in data["tabs"]):
                    raise SystemExit(f"duplicate day label: {data['tabs']}")
                if "guard" in data["stages"]:
                    raise SystemExit(f"raw stage label: {data['stages']}")
                print(json.dumps({"plan": data}, ensure_ascii=False))
                break

        refuse_js = """(async () => {
          const q = document.getElementById('q');
          q.value = 'Реши мне задачу по алгебре';
          document.getElementById('form').requestSubmit();
          for (let i = 0; i < 40; i++) {
            await new Promise(r => setTimeout(r, 250));
            const box = document.getElementById('refusal');
            if (!box.hidden) return JSON.stringify({ok: true, text: box.innerText});
          }
          return JSON.stringify({ok: false});
        })()"""
        await ws.send(json.dumps({"id": 2, "method": "Runtime.evaluate", "params": {
            "expression": refuse_js, "awaitPromise": True, "returnByValue": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 2:
                if "result" not in msg:
                    raise SystemExit(f"CDP refusal failed: {json.dumps(msg)[:1200]}")
                data = json.loads(msg["result"]["result"]["value"])
                if not data.get("ok"):
                    raise SystemExit("refusal UI did not render")
                if "Запрос не по теме" not in data["text"]:
                    raise SystemExit(f"bad refusal text: {data['text']}")
                print(json.dumps({"refusal": data}, ensure_ascii=False))
                break

asyncio.run(main())
PY

ok "plan rendered with validation and weather"
ok "refusal rendered"
echo "All frontend checks passed against $BASE_URL"
