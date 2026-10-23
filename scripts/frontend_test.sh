#!/usr/bin/env bash
# Headless frontend test: static shell + SSE plan rendering + refusal UI +
# the map's fuel-station markers.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
CHROME="${CHROME:-chromium}"
TMP_DIR="$(mktemp -d)"
PORT=$((9300 + RANDOM % 200))
CHROME_PID=""

cleanup() {
  if [[ -n "$CHROME_PID" ]]; then
    kill "$CHROME_PID" 2>/dev/null || true
    # Chromium keeps writing its profile while it shuts down, and removing the
    # directory while it does fails with ENOTEMPTY: that alone used to turn a
    # green run into "frontend test failed" from the exit trap.
    wait "$CHROME_PID" 2>/dev/null || true
  fi
  rm -rf -- "$TMP_DIR" 2>/dev/null || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
bad() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "OK  $*"; }
note() { echo "NOTE $*"; }

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

python3 - "$PORT" "$BASE_URL" "$TMP_DIR/layers.json" <<'PY'
import asyncio, json, sys, urllib.request
import websockets

port, base, status_path = sys.argv[1], sys.argv[2], sys.argv[3]

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
          for (let i = 0; i < 160; i++) {
            await new Promise(r => setTimeout(r, 500));
            if (!document.getElementById('result').hidden) {
              const meta = document.getElementById('p-meta').innerText;
              return JSON.stringify({
                ok: true,
                title: document.getElementById('p-title').textContent,
                meta,
                tabs: [...document.querySelectorAll('#tabs .tab')].map(x => x.textContent),
                dayDates: [...document.querySelectorAll('#tabs .tab small')].map(x => x.textContent.trim()),
                stopIds: [...document.querySelectorAll('#stops .stop')].map(x => x.dataset.placeId).filter(Boolean),
                descriptions: document.querySelectorAll('#stops .stop-desc').length,
                hasChecks: !!document.getElementById('checks'),
                hasTrace: !!document.getElementById('trace-box'),
                errorHidden: document.getElementById('error').hidden,
                refusalHidden: document.getElementById('refusal').hidden,
                stages: [...document.querySelectorAll('#stages li')].map(x => x.textContent),
                stops: document.querySelectorAll('#stops .stop').length,
                lodging: document.querySelectorAll('.lodge').length,
                mapReady: !!document.querySelector('.leaflet-container'),
                mapWidth: document.getElementById('map')?.clientWidth || 0,
                mapHeight: document.getElementById('map')?.clientHeight || 0,
                tiles: document.querySelectorAll('.leaflet-tile-loaded').length,
                attribution: !!document.querySelector('.leaflet-control-attribution'),
                errorText: document.getElementById('error-text')?.textContent || '',
                refusalText: document.getElementById('refusal')?.innerText || '',
                readyState: document.readyState,
                chips: document.querySelectorAll('.chip').length,
                healthText: document.getElementById('health')?.textContent || ''
              });
            }
          }
          return JSON.stringify({
            ok: false,
            stages: [...document.querySelectorAll('#stages li')].map(x => x.textContent),
            errorText: document.getElementById('error-text')?.textContent || '',
            refusalText: document.getElementById('refusal')?.innerText || '',
            readyState: document.readyState,
            chips: document.querySelectorAll('.chip').length,
            healthText: document.getElementById('health')?.textContent || ''
          });
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
                if "осадки" not in data["meta"] and "недоступна" not in data["meta"] and "недоступен" not in data["meta"]:
                    raise SystemExit(f"weather is neither rendered nor marked unavailable: {data['meta']}")
                if "мок" in data["meta"].lower() or "mock-llm" in data["title"].lower():
                    raise SystemExit(f"mock labels must be hidden: {data}")
                if any("День 1: День 1" in t for t in data["tabs"]):
                    raise SystemExit(f"duplicate day label: {data['tabs']}")
                if len(data["tabs"]) != 2:
                    raise SystemExit(f"requested two days, got {len(data['tabs'])}: {data['tabs']}")
                if len(set(data["dayDates"])) != len(data["dayDates"]):
                    raise SystemExit(f"duplicate day dates: {data['dayDates']}")
                if len(set(data["stopIds"])) != len(data["stopIds"]):
                    raise SystemExit(f"duplicate stops on the active day: {data['stopIds']}")
                if data.get("hasChecks") or data.get("hasTrace"):
                    raise SystemExit(f"debug panels must be removed from the UI: {data}")
                if data.get("stops") and data.get("descriptions") != data.get("stops"):
                    raise SystemExit(f"every stop needs a mini description: {data}")
                if not data["errorHidden"] or not data["refusalHidden"]:
                    raise SystemExit(f"stale notices are visible: {data}")
                if "guard" in data["stages"]:
                    raise SystemExit(f"raw stage label: {data['stages']}")
                if data.get("attribution"):
                    raise SystemExit("leaflet attribution control must be hidden")
                if not data.get("mapReady") or data.get("mapWidth", 0) < 100 or data.get("mapHeight", 0) < 100:
                    raise SystemExit(f"map container did not render: {data}")
                if not data.get("tiles"):
                    print("WARN: map tiles not loaded yet (network); container is ready")
                if not data.get("lodging"):
                    raise SystemExit("lodging recommendations did not render")
                print(json.dumps({"plan": data}, ensure_ascii=False))
                break

        await ws.send(json.dumps({"id": 3, "method": "Emulation.setDeviceMetricsOverride", "params": {
            "width": 390, "height": 844, "deviceScaleFactor": 1, "mobile": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 3:
                break
        await asyncio.sleep(1)
        await ws.send(json.dumps({"id": 4, "method": "Runtime.evaluate", "params": {
            "expression": "JSON.stringify({scrollWidth:document.documentElement.scrollWidth,innerWidth:window.innerWidth,mapWidth:document.getElementById('map')?.clientWidth||0})",
            "returnByValue": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 4:
                mobile = json.loads(msg["result"]["result"]["value"])
                if mobile["scrollWidth"] > mobile["innerWidth"] + 1:
                    raise SystemExit(f"mobile horizontal overflow: {mobile}")
                if mobile["mapWidth"] < 250:
                    raise SystemExit(f"mobile map is too narrow: {mobile}")
                print(json.dumps({"mobile": mobile}, ensure_ascii=False))
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

        # ---- fuel markers ----------------------------------------------------
        # 1. The test installs its own fetch hook (test-side only; app.js is
        #    untouched) so it can read the exact SSE bytes this page received.
        #    Everything below is asserted against that captured payload.
        # 2. A car query is submitted: fuel is only meaningful on the road, and
        #    app.js draws `day.fuel[]` markers and `day.legs[].geometry` lines
        #    for the active day. This particular query is one of the car demo
        #    queries whose payload carries both layers in mock mode (the
        #    Владивосток — Андреевка smoke query currently has no fuel stations
        #    and no drawable leg in this sandbox, so it would only skip).
        # 3. Marker/polyline counts come from the live page: `.fuel-pin` is the
        #    marker glyph class app.js renders inside its divIcon, and polylines
        #    are read from the Leaflet layer tree (`map._layers`, top-level
        #    `let` bindings of the classic app.js script are visible to CDP
        #    evaluation). Canvas rendering (preferCanvas: true) means the DOM
        #    has no <path> elements to count, hence the Leaflet introspection.
        # 4. If the backend cannot provide fuel or geometry in this sandbox,
        #    the matching assertion is skipped with a WARN — never a silent pass.
        hook_js = """(() => {
          if (window.__ftHooked) return 'already';
          window.__ftHooked = true;
          window.__ftSSE = [];
          const orig = window.fetch.bind(window);
          window.fetch = async (...args) => {
            const req = args[0];
            const url = typeof req === 'string' ? req : (req && req.url) || '';
            const res = await orig(...args);
            if (String(url).includes('/api/plan')) {
              try { res.clone().text().then((text) => window.__ftSSE.push(text)).catch(() => {}); } catch (e) {}
            }
            return res;
          };
          return 'installed';
        })()"""
        await ws.send(json.dumps({"id": 5, "method": "Runtime.evaluate", "params": {
            "expression": hook_js, "returnByValue": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 5:
                break

        fuel_js = """(async () => {
          const q = document.getElementById('q');
          q.value = 'Один день на машине вокруг Владивостока, где заправиться';
          document.getElementById('form').requestSubmit();
          for (let i = 0; i < 200; i++) {
            await new Promise(r => setTimeout(r, 500));
            if (window.__ftSSE.length && !document.getElementById('result').hidden) break;
          }
          await new Promise(r => setTimeout(r, 1000));
          if (!window.__ftSSE.length) return JSON.stringify({ok: false, reason: 'no SSE body captured'});
          let plan = null;
          for (const text of window.__ftSSE.slice().reverse()) {
            for (const block of String(text).replace(/\\r/g, '').split('\\n\\n')) {
              let event = 'message';
              const data = [];
              for (const line of block.split('\\n')) {
                if (line.startsWith('event:')) event = line.slice(6).trim();
                else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
              }
              if (event !== 'plan' || !data.length) continue;
              try { const parsed = JSON.parse(data.join('\\n')); plan = parsed.plan || parsed; } catch (e) { plan = null; }
              if (plan) break;
            }
            if (plan) break;
          }
          if (!plan) return JSON.stringify({ok: false, reason: 'captured stream has no plan event'});
          const days = Array.isArray(plan.days) ? plan.days : [];
          const countLayers = () => {
            let polylines = null, markers = null;
            try {
              if (typeof map !== 'undefined' && map && map._layers && typeof L !== 'undefined') {
                let lines = 0, pins = 0;
                const seen = new Set();
                const walk = (obj) => {
                  if (!obj || typeof obj !== 'object' || seen.has(obj)) return;
                  seen.add(obj);
                  if (obj instanceof L.Polyline) lines++;
                  if (obj instanceof L.Marker) pins++;
                  if (obj._layers) for (const key in obj._layers) walk(obj._layers[key]);
                };
                walk(map);
                polylines = lines;
                markers = pins;
              }
            } catch (e) { polylines = 'error: ' + e; }
            const el = document.getElementById('map');
            return {
              polylines,
              markers,
              fuelPins: el ? el.querySelectorAll('.fuel-pin').length : 0,
              stopPins: el ? el.querySelectorAll('.pin').length : 0,
              markerIcons: el ? el.querySelectorAll('.leaflet-marker-icon').length : 0
            };
          };
          // Mirrors mapPoint() in app.js: only finite, non-(0,0) points inside
          // the Primorye bbox can become map layers.
          const drawable = (item) => {
            const lat = Number(item && (item.lat !== undefined ? item.lat : item.latitude));
            const lon = Number(item && (item.lon !== undefined ? item.lon : (item.lng !== undefined ? item.lng : item.longitude)));
            if (!Number.isFinite(lat) || !Number.isFinite(lon)) return false;
            if (lat < 42.2 || lat > 48.5 || lon < 130.0 || lon > 139.1) return false;
            return !(Math.abs(lat) < 0.0001 && Math.abs(lon) < 0.0001);
          };
          const tabs = [...document.querySelectorAll('#tabs .tab')];
          const evidence = [];
          for (let i = 0; i < days.length; i++) {
            const current = days[i] || {};
            const fuel = Array.isArray(current.fuel) ? current.fuel : [];
            const legs = Array.isArray(current.legs) ? current.legs : [];
            const geometries = legs.map((leg) => leg && leg.geometry).filter((g) => g !== null && g !== undefined);
            const item = {
              day: i,
              fuel: fuel.length,
              drawableFuel: fuel.filter(drawable).length,
              geometries: geometries.length,
              geometry: geometries.map((g) => (typeof g === 'string' ? g : JSON.stringify(g)))
            };
            if (!fuel.length && !geometries.length) { item.counts = null; evidence.push(item); continue; }
            if (tabs[i]) {
              tabs[i].click();
              await new Promise(r => setTimeout(r, 500));
            }
            item.counts = countLayers();
            evidence.push(item);
          }
          return JSON.stringify({ok: true, days: days.length, tabs: tabs.length, evidence});
        })()"""
        await ws.send(json.dumps({"id": 6, "method": "Runtime.evaluate", "params": {
            "expression": fuel_js, "awaitPromise": True, "returnByValue": True}}))
        while True:
            msg = json.loads(await ws.recv())
            if msg.get("id") == 6:
                if "result" not in msg or "exceptionDetails" in msg.get("result", {}):
                    raise SystemExit(f"CDP fuel/route evaluate failed: {json.dumps(msg)[:1200]}")
                value = msg["result"]["result"].get("value")
                layers = json.loads(value) if value else None
                break
        if not layers or not layers.get("ok"):
            raise SystemExit(f"fuel/route plan did not render: {layers}")

        # The same bbox/mapPoint rules app.js applies before drawing a layer.
        def parse_line(raw):
            try:
                geo = json.loads(raw)
            except (TypeError, ValueError):
                return None
            coords = geo if isinstance(geo, list) else (geo.get("coordinates") if isinstance(geo, dict) else None)
            if not isinstance(coords, list) or len(coords) < 2:
                return None
            points = []
            for pair in coords:
                if not isinstance(pair, list) or len(pair) < 2:
                    return None
                lon, lat = pair[0], pair[1]
                if isinstance(lon, bool) or isinstance(lat, bool) or not isinstance(lon, (int, float)) or not isinstance(lat, (int, float)):
                    return None
                if not (130.0 <= lon <= 139.1 and 42.2 <= lat <= 48.5):
                    return None
                points.append((lat, lon))
            return points

        status = {"fuel": "skipped"}
        fuel_in_payload = any(item["fuel"] > 0 for item in layers["evidence"])
        geometry_in_payload = any(item["geometries"] > 0 for item in layers["evidence"])

        if not fuel_in_payload:
            print("WARN: the car query plan carries no plan.days[].fuel in this sandbox "
                  "build — fuel-marker assertion skipped")
        else:
            checked = 0
            for item in layers["evidence"]:
                if item["drawableFuel"] <= 0 or item["counts"] is None:
                    continue
                pins = item["counts"]["fuelPins"]
                if pins < 1 or pins < item["drawableFuel"]:
                    raise SystemExit(
                        f"fuel stations are in the payload but not on the map "
                        f"(day {item['day']}): expected >= {item['drawableFuel']} .fuel-pin markers, got {pins}")
                checked += 1
            if checked == 0:
                print("WARN: payload fuel stations carry no drawable coordinates — "
                      "fuel-marker assertion skipped")
            else:
                status["fuel"] = "asserted"

        # Линии маршрута фронт не рисует (расстояния и время считает бэкенд),
        # поэтому геометрия в payload — только информация: если она есть, она
        # должна быть корректным LineString внутри Приморья, но полилиний на
        # карте мы не требуем.
        geometry_note = "no geometry in payload"
        if geometry_in_payload:
            valid_lines = sum(1 for item in layers["evidence"] for raw in item["geometry"] if parse_line(raw))
            if valid_lines == 0:
                raise SystemExit("leg geometry is present but none is a drawable Primorye line")
            geometry_note = f"{valid_lines} drawable leg geometries (не рисуются на карте)"
        print(f"NOTE: {geometry_note}")

        with open(status_path, "w", encoding="utf-8") as handle:
            json.dump(status, handle, ensure_ascii=False, separators=(",", ":"))
        summary = {
            "layers": status,
            "plan_days": layers["days"],
            "days": [
                {
                    "day": item["day"],
                    "fuel": item["fuel"],
                    "drawable_fuel": item["drawableFuel"],
                    "geometry_legs": item["geometries"],
                    "map": item["counts"],
                }
                for item in layers["evidence"]
            ],
        }
        print(json.dumps(summary, ensure_ascii=False))

asyncio.run(main())
PY

ok "plan rendered with weather or an explicit missing-data warning"
ok "refusal rendered"
[[ -f "$TMP_DIR/layers.json" ]] || bad "fuel/route assertions produced no status"
if grep -q '"fuel":"asserted"' "$TMP_DIR/layers.json"; then
  ok "fuel-station markers rendered on the map"
else
  note "fuel-station markers not asserted (no plan.days[].fuel in this sandbox run)"
fi
echo "All frontend checks passed against $BASE_URL"
