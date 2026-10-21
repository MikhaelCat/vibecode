# DEMO.md — финальное демо и эвал на DeepSeek

**Главное правило:** DeepSeek — только на финальном демо и, если останется бюджет, на одном live-прогоне эвала. Всё остальное — `MOCK_LLM=1`: разработка, репетиции, скриншоты, Docker, mock-эвал. Все 60 генераций (20 запросов × 3 конфига) живьём не запускаем.

## 1. Окружение

```bash
test -f .env || install -m 600 .env.example .env
chmod 600 .env && ${EDITOR:-nano} .env
```

В `.env` для live-демо:

```dotenv
BIND=0.0.0.0:8080
MOCK_LLM=0
LLM_PROXY_TOKEN=<токен>
LLM_PROXY_URL=https://llm-proxy.spirit.t-education.com/v1
LLM_PROXY_MODEL=openrouter/deepseek/deepseek-v4.1-flash
DAILY_BUDGET_USD=1
PRICE_IN_PER_M=0
PRICE_OUT_PER_M=0
DATABASE_URL=sqlite://travel-ai.db
```

Токен — только в `.env` (он в `.gitignore`): не в git, не в `export`, не в истории команд, не на экране. `PRICE_*` — USD за 1M токенов, `0` = цена неизвестна; если знаешь прайс, поставь фактические. На VM `DATABASE_URL` — `/var/lib/travel-ai/app.db`. После демо верни `MOCK_LLM=1`. Для проверок нужен `jq` (`apt install jq`).

## 2. Смоук без LLM

```bash
MOCK_LLM=1 cargo run    # терминал 1
# терминал 2:
curl -s http://127.0.0.1:8080/api/health | jq
curl -s -X POST http://127.0.0.1:8080/api/plan/sync -H 'content-type: application/json' -d '{"query":"Поездка на машине из Владивостока в Андреевку на два дня: что посмотреть по пути и где заправиться?"}' | jq '{days:(.plan.days|length), hard_failed:.report.hard_failed, generation:.plan.generation, tools:.tools_used, tokens:.usage.total_tokens}'
curl -s -X POST http://127.0.0.1:8080/api/plan/sync -H 'content-type: application/json' -d '{"query":"Реши мне задачу по алгебре: найди корни уравнения x^2 - 5x + 6 = 0"}' | jq '{refusal, plan, tokens:.usage.total_tokens}'
```

Health: `status:"ok"`, `model:"mock-llm"`, `mock_llm:true`, `prompt_version:"system_v1"`, `budget:{daily_usd,spent_today_usd}`.

Sync: HTTP 200, `days:2`, `hard_failed:false`, `generation:"llm"`, в `tools` — `search_places`/`get_weather`/`find_lodging` (mock-цикл; live может добавить `get_route`/`find_fuel`), `tokens>0`, `cost_usd:0`.

Refusal: HTTP 200, `refusal.category:"off_topic"`, `plan:null`, `tokens:0` — отказ локальный, LLM не вызывается.

## 3. Один живой запрос

Убедись, что `.env` live (`MOCK_LLM=0`), `MOCK_LLM` не экспортирован (`unset MOCK_LLM`), сервер перезапущен, а `/api/health` показывает `mock_llm:false` и `model:"openrouter/deepseek/deepseek-v4.1-flash"`.

```bash
curl -sS -X POST http://127.0.0.1:8080/api/plan/sync \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу на выходные по Приморью на машине, не больше двух дней"}' \
  -o /tmp/live-plan.json -w '\nHTTP %{http_code}, %{time_total}s\n'
jq '{days:(.plan.days|length), hard_failed:.report.hard_failed, generation:.plan.generation, model, tokens:.usage.total_tokens, estimated:.usage.estimated, cost_usd:.usage.cost_usd, latency_ms}' /tmp/live-plan.json
RUN_ID=$(jq -r .run_id /tmp/live-plan.json)
curl -s "http://127.0.0.1:8080/api/runs/$RUN_ID" | jq '{generation:.plan.generation, hard_failed:.trace.validation.hard_failed, steps:(.trace.steps|length), tools:(.trace.tool_calls|map(.name))}'
```

Ориентир: 15–25 с и ~15 тыс. токенов. Успех: `days:2`, `hard_failed:false`, `generation:"llm"`. Если `model:"mock-llm"` — live не включился, запрос бесплатный, но это не проверка DeepSeek. Если `generation:"fallback"` — LLM не довёл план (сбой прокси, лимит шагов или ремонтов), это не успешный live-прогон.

`usage.cost_usd` — стоимость от провайдера (`usage.cost`), если прокси её вернул, иначе `0.0`; в `/api/stats` при нуле подставляется оценка по `PRICE_*`. Ноль не значит «бесплатно»: цена может быть неизвестна. Смотри также `prompt_tokens`/`completion_tokens` и `estimated`.

## 4. Бюджет

```bash
curl -s http://127.0.0.1:8080/api/stats | jq
curl -s http://127.0.0.1:8080/api/health | jq '.budget'
```

После каждого live-запроса. `DAILY_BUDGET_USD=1` (или `2`, если останется на live-эвал). Останавливайся на 70–80% лимита: проверка идёт до запроса (`spent < limit`), поэтому последний запрос может превысить бюджет.

Если `cost_usd` равен 0, а `PRICE_*` тоже 0, работает `DAILY_TOKEN_BUDGET` (1M токенов/день). Mock-прогоны в бюджет не пишутся и не блокируются.

## 5. Финальный эвал

```bash
mkdir -p results
for v in v1_baseline v2_compact v3_fast; do
  MOCK_LLM=1 cargo run --release --bin eval -- \
    --config "configs/$v.toml" --dataset data/eval/queries.jsonl \
    --out "results/$v.json" --markdown "results/$v.md"
done
jq '.summary' results/v2.json
```

Это 20 кейсов × 3 конфига, бесплатно (без `--live` эвал и так mock; LLM-токены не тратятся, но внешние тулы могут ходить в сеть). Сервер для демо всегда берёт `system_v1.md`; конфиги эвала переключают `system_v1/v2/v3`.

Опционально — один live-конфиг и только с `--limit`, не все 60:

```bash
set -a; . <(grep -E '^(LLM_PROXY_TOKEN|LLM_PROXY_URL|LLM_PROXY_MODEL|DAILY_BUDGET_USD|PRICE_)' .env); set +a
MOCK_LLM=0 cargo run --release --bin eval -- --config configs/v2_compact.toml --dataset data/eval/queries.jsonl --limit 3 --live --out results/v2_live.json --markdown results/v2_live.md
```

Eval не читает `.env` сам, поэтому переменные экспортируются из файла (токен не печатается). Eval пишет в in-memory БД: `/api/stats` его не видит, `DAILY_BUDGET_USD` его не остановит — ограничивай `--limit`. Бюджета мало — пропусти этот шаг.

## 6. Сценарий демо (максимум 3 live-запроса включая §3, ретраи считаются)

1. **Машина, выходные (live).** «Хочу на выходные по Приморью на машине, не больше двух дней»: план на 2 дня, `hard_failed:false`, `generation:"llm"`, trace (UI или `/api/runs`), карта — если SPA собрана. Если §3 уже был, не повторяй запрос — покажи сохранённый `/tmp/live-plan.json`.
2. **Погода/пешком (live).** «Пеший маршрут к мысу Тобизина на один день — как пройти и что взять с собой?» (или велосипедный q07): `get_weather`/`get_route` и запасной вариант при дожде.
3. **Отказ (бесплатно).** «Реши задачу по алгебре» → `refusal` без вызова LLM, 0 токенов.

Trace: если SPA собрана, открой «как собран план»; иначе — `/api/runs/{run_id}` и `trace.steps` (≥2 шагов) + `tool_calls`; валидация — `report`/`trace.validation` с `hard_failed:false`.

Что говорить: «Это не один промпт, а агент: он вызывает инструменты, проверяет план детерминированным валидатором и показывает trace. Один живой запрос DeepSeek занимает ~15–25 с и ~15 тыс. токенов, поэтому остальное мы гоняем на mock. Отказ обрабатывается локально и не тратит бюджет».

## 7. Прокси упал, квота или бюджет

1. Поставь `MOCK_LLM=1` в `.env` и `unset MOCK_LLM` в shell.
2. Перезапусти: локально Ctrl+C → `MOCK_LLM=1 cargo run`; на VM — `sudo systemctl restart travel-ai`; Docker — пересоздай контейнер (§8).
3. Проверь `/api/health`: `mock_llm:true`; повтори тот же запрос — придёт mock-план.
4. Скажи честно: «Провайдер/лимит недоступен, показываю офлайн-план; логика, тулы и валидатор те же, LLM заменён на mock». Если live-ответ уже пришёл с `generation:"fallback"`, это тоже повод перейти на mock.
5. Если пришёл 429 `budget_exhausted`, это live-лимит. Переключись на `MOCK_LLM=1` — mock-прогоны исключены из бюджета; отдельно можно поднять `DAILY_TOKEN_BUDGET` или обнулить `DAILY_BUDGET_USD`.

## 8. Docker

```bash
docker build -f deploy/Dockerfile -t travel-ai:demo .
docker run -d --name travel-ai-demo --env-file .env -p 8080:8080 \
  -e DATABASE_URL=sqlite:///var/lib/travel-ai/app.db \
  -v travel-ai-data:/var/lib/travel-ai \
  --health-cmd 'curl -fsS http://127.0.0.1:8080/api/health || exit 1' \
  --health-interval 10s --health-timeout 3s --health-retries 6 \
  travel-ai:demo
docker ps --filter name=travel-ai-demo
docker inspect --format '{{.State.Health.Status}}' travel-ai-demo
docker logs -f travel-ai-demo
```

Токен — только через `--env-file`, не `-e LLM_PROXY_TOKEN=...` (иначе виден в `ps`). `DATABASE_URL` переопределён, чтобы SQLite лёг в volume, а не в `/app`. После правки `.env` контейнер пересоздай: `docker restart` не перечитывает env. SPA в образ не попадает (`src/frontend/dist` в `.dockerignore`) — UI показывай с основного стенда.

## 9. Чек-лист

- [ ] Токен только в `.env`; `chmod 600`; в репозитории, логах, истории и на экране его нет.
- [ ] `MOCK_LLM=1 cargo run` + health/sync/refusal из §2 проходят.
- [ ] `.env` live: `mock_llm:false`, `DAILY_BUDGET_USD=1/2`, `/api/stats` проверен.
- [ ] Бэкап БД: `sqlite3 travel-ai.db ".backup 'travel-ai-$(date +%F).db'"` (VM — `/var/lib/travel-ai/app.db`, Docker — volume `travel-ai-data`).
- [ ] Порт 8080 открыт снаружи: `curl` с другого хоста, не только с localhost.
- [ ] Резервное видео 2–3 минуты (mock-прогон) + скриншоты trace; проигрывается локально.
- [ ] Проверен fallback: `MOCK_LLM=1`, рестарт, тот же запрос.
