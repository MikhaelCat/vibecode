# API-контракт travel-ai

Контракт для фронтенда и QA, сверенный с кодом (`src/api/`, `src/agent/orchestrator.rs`, `src/domain/`). Базовый URL — `http://IP:PORT`, по умолчанию `BIND=0.0.0.0:8080`. Аутентификация опциональна: если задан `APP_TOKEN`, `POST /api/plan` и `/api/plan/sync` требуют `x-app-token` или `Authorization: Bearer`. CORS не включён — фронт работает с того же origin или через dev-proxy. Все ответы API, кроме SSE, — JSON UTF-8.

## 1. Общие ограничения

| Ограничение | Значение по умолчанию | Ответ при нарушении |
|---|---|---|
| Тело запроса | 16 КиБ (`16 * 1024`), на весь router | 413 от `RequestBodyLimitLayer`; тело не из `ApiError` |
| `query` | 1–1000 символов после `trim()` (`MAX_QUERY_CHARS`) | пусто — 400 `bad_request`; длиннее — 413 `too_large` |
| Одновременные прогоны (`/api/plan` и `/api/plan/sync`) | 4 (`CONCURRENT_RUNS`, минимум 1) | 429 `rate_limited`, слот не ждёт |
| Дневной бюджет | 25 USD (`DAILY_BUDGET_USD`) и 1M токенов (`DAILY_TOKEN_BUDGET`) | 429 `budget_exhausted` + `hint` |
| `/api/plan` | жёсткий таймаут 120 с | `refusal` с `category:"timeout"` + `done` без деталей |
| `/api/plan/sync` | берёт тот же семафор, таймаут 180 с | 429 `rate_limited` или 504 `upstream_timeout` |

`request_timeout` (90 с) ограничивает HTTP-запрос к LLM; `/api/plan/sync` дополнительно ограничен 180 с. Таймаут одного тула — 15 с. Лимиты шагов: `MAX_STEPS=8`, `MAX_REPAIRS=2`.

| Метод | Путь | Ответ |
|---|---|---|
| `POST` | `/api/plan` | `text/event-stream` |
| `POST` | `/api/plan/sync` | JSON |
| `GET` | `/api/health` | JSON |
| `GET` | `/api/stats` | JSON |
| `GET` | `/api/runs/{id}` | JSON или 404 |

## 2. POST /api/plan — SSE

Запрос: `Content-Type: application/json`, тело `{"query":"...","start_date":"YYYY-MM-DD","end_date":"YYYY-MM-DD","transport":"car"}`. `start_date`/`end_date`/`transport` необязательны: если даты переданы, они переопределяют разобранный intent и ограничивают план 1–7 днями; `transport` — `car`, `foot`, `bike`, `transit`. Успех — `200 text/event-stream`, `Cache-Control: no-cache`; кадр — `event: <имя>\ndata: <json>\n\n`. Ошибки валидации, бюджета, авторизации и занятости семафора приходят до открытия потока обычным JSON с 4xx.

| Событие | Поля `data` | Когда |
|---|---|---|
| `stage` | `stage`, `status` | `{"stage":"guard","status":"running"}` в начале; `{"stage":"intent","status":"ok"}` после guard; `{"stage":"fallback","status":<bool>}` перед fallback-планом |
| `intent` | полный `Intent` | один раз после успешного guard |
| `tool_call` | `id`, `name`, `args` | перед каждым вызовом с корректным JSON-аргументом, включая `submit_plan` |
| `tool_result` | `Ok`-ветка: `id`, `name`, `ok`, `ms`, `cached`, `source`, `summary` (`ok` — флаг `ToolOutput`, может быть `false`); `Err`-ветка: `id`, `name`, `ok:false`, `ms`, `error` | после обычного тула; для `submit_plan` не бывает |
| `plan` | полный `Plan` | один раз: принятый `submit_plan` или fallback |
| `validation` | `ValidationReport` | один раз сразу после `plan` |
| `usage` | `TokenUsage` | один раз после `validation` |
| `refusal` | `category`, `message` | только ветка отказа, вместо `intent`/`plan` |
| `done` | успех: `run_id`, `latency_ms`, `outcome`, `generation`, `usage`; отказ guard: `run_id`, `latency_ms`, `outcome:"refusal"`; таймаут: только `outcome:"refusal"` | всегда последнее событие, затем поток закрывается |
| `error` | — | **не отправляется**: в коде такого события нет |

Типы: `stage.status` — строка у `guard`/`intent`, boolean у `fallback`. `generation` — `llm`, `repaired` или `fallback`; `repaired` выставляется, если план был принят после ремонта. `outcome` — `plan`, `degraded` или `refusal`. Отказ — это HTTP 200 и событие `refusal`, а не 4xx/5xx.

### Порядок событий

1. Успех LLM: `stage(guard)` → `stage(intent)` → `intent` → пары `tool_call`/`tool_result` → `tool_call(submit_plan)` → `plan` → `validation` → `usage` → `done(generation:"llm")`. `plan` приходит после обработки всех calls текущего ответа, даже если `submit_plan` был не последним.
2. Fallback: `stage(guard)` → `stage(intent)` → `intent` → возможные тулы → `stage(fallback, status:<bool>)` → `plan(generation:"fallback")` → `validation` → `usage` → `done(outcome:"degraded")`. `status:true` — была ошибка LLM или кончились ремонты; `status:false` — шаги кончились без зафиксированной ошибки.
3. Отказ: `stage(guard)` → `refusal` → `done(outcome:"refusal")`; `intent`, `plan`, `usage` не приходят.
4. Если `submit_plan` не прошёл hard-валидацию, LLM получает feedback и до 2 ремонтов; события `plan` нет, пары `tool_call`/`tool_result` могут повторяться. `plan` появляется только после успешного submit.

### Keep-alive и отмена

Каждые 15 с без событий приходит SSE-комментарий `: keep-alive\n\n`; клиент обязан его игнорировать. `CancellationToken` в коде нет: `AbortController.abort()` закрывает HTTP-поток, но прогон продолжается до конца, сохраняется в БД и держит слот семафора. Если прогон не уложился в 120 с, сервер отправляет `refusal` с `category:"timeout"` и `done` только с `outcome:"refusal"`; такой прогон в БД не сохраняется. Внутренняя ошибка LLM уводит прогон в fallback, ошибка тула видна как `tool_result` с `ok:false`; SSE-события `error` нет.

### Пример потока

Запрос:

```json
{"query":"Хочу один день по Приморью на машине"}
```

Данные событий (реальные JSON-объекты, по одному на строку):

```text
event: stage
data: {"stage":"guard","status":"running"}

event: stage
data: {"stage":"intent","status":"ok"}

event: intent
data: {"region":"primorye","start_date":"2026-10-24","end_date":"2026-10-24","max_days":1,"transport":"car","budget_rub":null,"party_size":1,"with_children":false,"interests":[],"must_see":[],"avoid":["ночёвка"],"pace":"normal","assumptions":["даты не указаны — взял ближайшие выходные","число людей не указано — считаю одного путешественника"]}

event: tool_call
data: {"id":"mock_call_0","name":"search_places","args":{"area":"Приморский край","categories":["nature","museum","viewpoint"],"limit":12}}

event: tool_result
data: {"id":"mock_call_0","name":"search_places","ok":true,"ms":3,"cached":false,"source":"mock","summary":"OSM недоступен: демонстрационные места"}

event: tool_call
data: {"id":"mock_call_1","name":"get_weather","args":{"lat":43.1155,"lon":131.8855,"start_date":"2026-10-24","end_date":"2026-10-24"}}

event: tool_result
data: {"id":"mock_call_1","name":"get_weather","ok":true,"ms":2,"cached":false,"source":"mock","summary":"Погода: демо-данные (провайдер недоступен)"}

event: tool_call
data: {"id":"mock_call_2","name":"submit_plan","args":{"title":"План по Приморью","summary":"1 дн.","transport":"car","days":[{"date":"2026-10-24","items":[{"place_id":"mock:place:oceanarium"},{"place_id":"mock:place:fortress"}]}],"lodging_ids":[]}}

event: plan
data: {"id":"3f0f5f7e-6a5b-4c2f-9f1a-2d3c4b5a6e7f","title":"План по Приморью","summary":"1 дн. — офлайн-план для проверки пайплайна","intent":{"region":"primorye","start_date":"2026-10-24","end_date":"2026-10-24","max_days":1,"transport":"car","budget_rub":null,"party_size":1,"with_children":false,"interests":[],"must_see":[],"avoid":["ночёвка"],"pace":"normal","assumptions":["даты не указаны — взял ближайшие выходные","число людей не указано — считаю одного путешественника"]},"days":[{"date":"2026-10-24","title":"День 1 — Приморский океанариум","items":[{"place_id":"mock:place:oceanarium","name":"Приморский океанариум","kind":"museum","lat":43.0142,"lon":131.9302,"start":"10:00:00","end":"12:00:00","opening_hours":null,"price_rub":null,"source":"mock","note":null},{"place_id":"mock:place:fortress","name":"Владивостокская крепость","kind":"historic","lat":43.1225228,"lon":131.8764755,"start":"14:00:00","end":"16:00:00","opening_hours":null,"price_rub":null,"source":"mock","note":null}],"legs":[{"from_place_id":"mock:place:oceanarium","to_place_id":"mock:place:fortress","mode":"car","distance_km":4.4,"duration_min":5,"geometry":null,"source":"estimated"}],"lodging_id":null}],"lodging":[],"lodging_options":[{"id":"mock:lodge/lotte-vladivostok","name":"ЛОТТЕ Отель Владивосток","settlement":"Владивосток","lat":43.1182252,"lon":131.8886769,"price_per_night":9000,"kind":"hotel","source":"mock"}],"transport":"car","total_km":4.4,"cost_estimate_rub":null,"warnings":[{"code":"demo_data","message":"Часть данных демонстрационная — проверьте перед выездом"}],"weather":[{"date":"2026-10-24","t_min":10.6,"t_max":16.0,"precip_prob":4.0,"wind_max":17.1,"desc":"осадки 4%, ветер 17 м/с"}],"sources":[{"provider":"search_places","url":null,"fetched_at":"2026-10-21T04:10:00Z","kind":"mock"},{"provider":"get_weather","url":null,"fetched_at":"2026-10-21T04:10:01Z","kind":"mock"}],"generation":"llm","created_at":"2026-10-21T04:10:02Z"}

event: validation
data: {"violations":[],"hard_failed":false,"checks_run":15,"checks_passed":15,"score":1.0}

event: usage
data: {"prompt_tokens":1810,"completion_tokens":420,"total_tokens":2230,"cost_usd":0.0,"estimated":true}

event: done
data: {"run_id":"a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d","latency_ms":1240,"outcome":"plan","generation":"llm","usage":{"prompt_tokens":1810,"completion_tokens":420,"total_tokens":2230,"cost_usd":0.0,"estimated":true}}
```

Отказ (запрос «Спланируй неделю в Сочи»):

```text
event: refusal
data: {"category":"other_region","message":"Пока я работаю только с Приморским краем. Другие регионы не смогу спланировать, а вот маршрут по югу Приморья — с удовольствием."}

event: done
data: {"run_id":"b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e","latency_ms":18,"outcome":"refusal"}
```

## 3. POST /api/plan/sync

Тело — то же, что у SSE: `query` + необязательные `start_date`, `end_date`, `transport`. Ответ — JSON без SSE; бюджет, fallback и семафор те же, что у SSE: sync занимает слот `CONCURRENT_RUNS` и при занятости получает 429 `rate_limited`. Таймаут sync — 180 с.

| Поле | Тип |
|---|---|
| `run_id` | UUID-строка |
| `model` | строка (`mock-llm` для mock) |
| `intent` | `Intent` или `null` |
| `plan` | `Plan` или `null` |
| `report` | `ValidationReport` или `null` |
| `refusal` | `{category,message}` или `null` |
| `usage` | `TokenUsage` |
| `latency_ms` | число |
| `tools_used` | массив имён тулов без `submit_plan`; возможны повторы и неудачные вызовы |
| `trace` | `RunTrace` |

При успехе `refusal:null`; при отказе `intent/plan/report:null`, `usage` — нули, `refusal` заполнен, `trace.outcome:"refusal"`. Отдельного `outcome` на верхнем уровне нет. Пример отказа:

```json
{"run_id":"b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e","model":"mock-llm","intent":null,"plan":null,"report":null,"refusal":{"category":"off_topic","message":"Я помогаю планировать поездки по Приморскому краю, поэтому не буду решать эту задачу. Могу собрать маршрут на выходные — например, Владивосток — Андреевка."},"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"cost_usd":0.0,"estimated":false},"latency_ms":1,"tools_used":[],"trace":{"run_id":"b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e","query":"Реши мне задачу по алгебре","prompt_version":"system_v1","model":"mock-llm","steps":[],"tool_calls":[],"validation":null,"outcome":"refusal","latency_ms":1,"error":null}}
```

## 4. GET /api/health, /api/stats, /api/runs/{id}

`GET /api/health` всегда 200 и не проверяет доступность LLM; ошибка чтения БД не меняет ответ: `{"status":"ok","version":"0.1.0","model":"mock-llm","mock_llm":true,"auth_required":false,"prompt_version":"system_v1","uptime_secs":312,"budget":{"daily_usd":25.0,"spent_today_usd":0.0}}`. `mock_llm` — признак отсутствия реального LLM; `auth_required` — задан ли `APP_TOKEN` (тогда UI показывает поле токена); `uptime_secs` — секунды с запуска.

`GET /api/stats` — агрегат за текущий день Владивостока: `{"date":"2026-10-21","runs":3,"prompt_tokens":5400,"completion_tokens":900,"cost_usd":0.0}`. `hard-pass`, p50/p95 и разбивки по статусам в ответе **нет**; `runs` включает отказы и degraded. Фильтр в SQL идёт по UTC-дате из `created_at`, поэтому на границе суток возможны расхождения с `date`.

`GET /api/runs/{id}`: `id` — UUID-строка из `done.run_id`; ищется как есть. Поля: `id`, `created_at` (RFC3339), `query`, `status` (`plan` / `refusal` / `degraded`), `latency_ms`, `prompt_tokens`, `completion_tokens`, `cost_usd`, `plan` (объект или `null`), `trace` (объект или `null`). Отдельного `usage` нет. Неизвестный id — 404 `not_found`.

`RunTrace`: `run_id`, `query`, `prompt_version`, `model`, `steps[]` (`step`, `llm_ms`, `tool_names[]`), `tool_calls[]` (`id`, `name`, `args_summary`, `ms`, `ok`, `cached`, `source`, `summary`), `validation`, `outcome`, `latency_ms`, `error`. `steps` — по одной записи на успешный ответ LLM; `submit_plan` в trace имеет `source:"validator"`, ошибки аргументов — `source:"parse_error"`, ошибки тула — `source:"error"`, исчерпанный лимит поиска — `source:"budget"`.

## 5. Ошибки

Формат тела из `src/error.rs` — плоский объект, `hint` присутствует всегда (может быть `null`):

```json
{"code":"bad_request","message":"query must not be empty","hint":null}
```

| HTTP | `code` | Когда |
|---|---|---|
| 400 | `bad_request` | пустой `query` после trim |
| 401 | `unauthorized` | задан `APP_TOKEN`, но заголовок не совпал |
| 413 | `too_large` | `query` длиннее лимита; тело >16 КиБ — 413 от layer, без этого кода |
| 429 | `rate_limited` | нет свободного слота семафора (`CONCURRENT_RUNS`) |
| 429 | `budget_exhausted` | дневной бюджет исчерпан; `hint` заполнен |
| 404 | `not_found` | неизвестный `run_id` |
| 504 | `upstream_timeout` | `/api/plan/sync` не уложился в 180 с |
| 500 | `internal` | объявлен, но текущие обработчики его не возвращают |

Битый JSON, отсутствие `query` или неверный `Content-Type` отсекает extractor axum: 400/422/415 с текстовым телом, **не** в конверте `ApiError`. Сбой LLM внутри прогона уводит в fallback; `llm_unavailable` не используется.

## 6. SSE-клиент на fetch

`EventSource` не подходит, потому что нужен POST.

```js
async function plan(query, onEvent, signal) {
  const res = await fetch("http://IP:PORT/api/plan", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query }),
    signal,
  });
  if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    buf += decoder.decode(value, { stream: true });
    for (let i; (i = buf.indexOf("\n\n")) >= 0; ) {
      const raw = buf.slice(0, i);
      buf = buf.slice(i + 2);
      if (raw.startsWith(":")) continue; // keep-alive
      const lines = raw.split("\n");
      const event = (lines.find((l) => l.startsWith("event:")) ?? "event: message").slice(6).trim();
      const data = lines.filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("\n");
      if (data) onEvent(event, JSON.parse(data));
    }
  }
}

const ctrl = new AbortController();
plan("Хочу один день по Приморью", (event, data) => console.log(event, data), ctrl.signal);
// ctrl.abort() закроет поток, но не остановит прогон на сервере.
```

## 7. Подсказки фронтенду

- Погода плана — `plan.weather[]` (`date`, `t_min`, `t_max`, `precip_prob`, `wind_max`, `desc`); в нём только даты плана.
- Бейдж mock/live стройте по `plan.sources[].kind` (`live`, `cache`, `mock`, `stale`, `estimated`); `provider` — имя тула (`search_places`, `get_weather`, ...), а не провайдер. `tool_result.source` — тот же `kind`, а не имя источника.
- `plan.warnings[]` — `{code,message}`: `unknown_place`, `lodging_auto`, `demo_data`, `fallback`. Показывайте все; `fallback` — признак degraded.
- `report.violations[]` — `{code,severity,message,hint,place_id}`; `severity` — `hard`/`soft`, `hard_failed` — блокирующий флаг. `hint` и `place_id` могут быть `null`.
- `generation:"fallback"` или `done.outcome:"degraded"` — плашка «план собран без LLM». `generation:"repaired"` — план был принят после ремонта.
- `tool_result.cached:true` — попадание в кэш; `source` при этом может стать `cache` (только если был `live`). `cached` не означает mock/live и не заменяет `source`.
- Не считайте `usage.cost_usd` признаком live: при mock и при `PRICE_*_PER_M=0` он равен `0`.

## 8. Подсказки QA

- `run_id` берите из `done` (SSE) или из sync-ответа, затем `GET /api/runs/{run_id}`. Отказы guard тоже сохраняются, кроме `timeout`. После обрыва SSE прогон дописывается в БД, но слот занят до его завершения.
- `trace.steps` фиксирует успешные раунды LLM (`step` с 0, `llm_ms`, `tool_names`); `args_summary` обрезан до 200 символов (обычный `summary` — нет, кроме feedback `submit_plan`). `trace.validation` есть только у plan/degraded; `trace.outcome` — `plan`/`refusal`/`degraded`, `error` в текущем потоке не появляется.
- `usage.cost_usd` может быть `0` (mock или неизвестные цены); `usage.estimated` говорит, оценка ли это. В `done` объект `usage` дублирует событие `usage`.
- `latency_ms` фиксируется до отправки `plan/validation/usage/done` и до записи в SQLite, поэтому меньше времени жизни SSE-соединения.
- В HTTP достижимы отказы `off_topic`, `other_region`, `injection` и `timeout` (120-секундный таймаут SSE); `empty` и `too_long` перехватываются `validate_query` раньше (400/413).
- Sync использует тот же семафор, что и SSE: `CONCURRENT_RUNS=4` ограничивает суммарное число одновременных прогонов, а не только SSE.

## 9. curl

```bash
BASE=http://127.0.0.1:8080
curl -s "$BASE/api/health"
curl -N -X POST "$BASE/api/plan" \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу на выходные по Приморью на машине, не больше двух дней"}'
curl -s -X POST "$BASE/api/plan/sync" \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу один день по Приморью на машине"}' | python3 -m json.tool
curl -N -X POST "$BASE/api/plan" \
  -H 'content-type: application/json' \
  -d '{"query":"Спланируй неделю в Сочи"}'
curl -s "$BASE/api/runs/$RUN_ID" | python3 -m json.tool
```

## 10. Осознанные упрощения

- `RunTrace` не содержит `usage`/`timings`/`git_sha`; usage лежит в `RunRecord`. Валидатор выполняет 15 проверок (`checks_run=15`).
- Отмены прогона по обрыву SSE нет: поток закрывается, серверный прогон доигрывает и сохраняется. Это в бэклоге.
- `GET /api/evals/latest` не реализован; таблицы эвалов лежат в `results/` и README.
- Фронтенд `src/frontend/dist` в репозитории отсутствует: `/` отдаёт 404, пока команда фронта не соберёт SPA. API и SSE работают независимо.
- `empty`/`too_long` для `query` по HTTP перехватываются раньше (400/413), поэтому `refusal` с этими категориями через HTTP недостижим; он нужен для прямых вызовов guard.
