# vibecode — AI-ассистент путешествий по Приморскому краю

Веб-ассистент: свободный запрос на русском → проверенный план поездки на 1–3 дня по Приморскому краю: места, маршрут, ночёвка, погода, ограничения, карта и trace того, как план собран.

**Хакатон:** Т-Банк × Сириус · 2 дня · защита 8 + 3 минуты  
**Стек:** Rust (axum + tokio), T-Bank Spirit LLM Proxy (DeepSeek) / OpenRouter, OpenStreetMap / Open-Meteo / OSRM, SQLite, SPA + Leaflet  
**Основная папка кода:** [`src/`](src/)

---

## Документация

- [Архитектура и план проекта](docs/ARCHITECTURE.md) — требования, компоненты, агентный цикл, тулы, валидатор, API, эвал, деплой, таймлайн.
- [Команда и контракты](docs/TEAM.md) — роли, зоны ответственности, API для фронта/парсера/QA, git-процесс, Definition of Done.
- [Замеры и методология](docs/EVAL.md) — mock-таблица, живой контрольный прогон и команды.
- `matvey/` — Python-прототип инструментов из одноимённой ветки (Nominatim, Open-Meteo, Wikipedia, OSRM, Overpass). Wikipedia geosearch портирован в Rust-тул `get_nearby_attractions`.

Коротко о главном:

- **Ядро** — агентный цикл tool calling + терминальный `submit_plan`, а не один вызов LLM.
- **Качество** — детерминированный валидатор плана (V01–V15) и провенанс: каждая точка ссылается на `place_id` из тула.
- **Данные** — погода, места, геокодинг и маршруты живые; жильё, расписания и наличие топлива — моки с явной пометкой.
- **Guardrails** — только Приморский край и путешествия; отказ на другие регионы, офтоп и инъекции.
- **Замеры** — 20 запросов × 2–3 версии пайплайна: hard-pass, tool-choice, токены, p50/p95.

## LLM

Поддерживаются два OpenAI-совместимых пути, выбор автоматический:

1. **T-Bank Spirit LLM Proxy** — приоритет, если задан `LLM_PROXY_TOKEN`.
   Адрес `https://llm-proxy.spirit.t-education.com/v1`, модель по умолчанию
   `openrouter/deepseek/deepseek-v4.1-flash`. Токен — только в `.env`, не в git.
2. **OpenRouter** — если задан `OPENROUTER_API_KEY` и нет proxy-токена.

Без ключа включается `MockLlm`: пайплайн, тулы, валидатор, SSE и эвал работают
офлайн и не тратят деньги. Для публичного демо задайте `APP_TOKEN` (тогда оба
POST-эндпоинта требуют `x-app-token`), а расход ограничивайте `DAILY_BUDGET_USD`
и `DAILY_TOKEN_BUDGET`. DeepSeek через proxy используйте экономно — только
на финальных прогонах и демо; повседневные тесты гоняйте на mock.

---

## Команда

| Роль | Зона | Папки |
|---|---|---|
| **Backend + deploy** | API/SSE, orchestrator, агентный цикл, валидатор, store, бюджет, VM и systemd | `src/api/`, `src/agent/`, `src/domain/`, `src/llm/`, `src/store/`, `src/main.rs`, `deploy/` |
| **Frontend** | SPA, SSE-клиент, карта, план, trace | `src/frontend/` |
| **Parser** | OSM/Open-Meteo/OSRM, нормализация, моки, fixtures, кэш | `src/tools/providers/`, `src/parser/`, `data/mock/`, `fixtures/` |
| **QA** | эвал, тесты, guardrails, метрики | `src/bin/eval.rs`, `src/qa/`, `data/eval/`, `tests/` |
| **System prompt engineer** | system/preflight/repair-промпты, описания тулов, отказы | `src/prompts/`, `configs/` |

---

## Структура репозитория

```text
vibecode/
├── Cargo.toml
├── src/                    # главная папка
│   ├── main.rs             # сервер
│   ├── api/                # axum, SSE, DTO
│   ├── agent/              # orchestrator, guard, prompt, repair
│   ├── domain/             # Intent, Plan, PlanDraft, validator
│   ├── llm/                # OpenRouter client, mock LLM, messages
│   ├── tools/              # trait Tool, registry, cache
│   │   └── providers/      # parser: OSM, Open-Meteo, OSRM, mocks
│   ├── store/              # SQLite, runs, budget
│   ├── bin/eval.rs         # QA: раннер эвалов
│   ├── prompts/            # prompt engineer
│   ├── frontend/           # frontend
│   ├── parser/             # parser: скрипты и fixtures
│   └── qa/                 # QA: сценарии и guardrails
├── data/                   # mock, geo, eval
├── configs/                # v1/v2/v3 для эвала
├── fixtures/               # записанные ответы провайдеров
├── migrations/             # SQLite
├── deploy/                 # systemd, install.sh, Dockerfile
├── scripts/                # smoke, eval_all, docker_smoke
├── tests/                  # интеграционные тесты валидатора
├── .github/workflows/ci.yml
├── docs/
└── README.md
```

`src/` — главная папка: Rust-бэкенд лежит в корне `src/`, команды — в `src/frontend/`, `src/parser/`, `src/qa/`, `src/prompts/`. Cargo собирает только Rust-модули, вложенные папки команд для него — ресурсы.

---

## Быстрый старт

```bash
cp .env.example .env       # ключ можно оставить пустым — включится mock LLM
cargo run                  # сервер на http://127.0.0.1:8080
cargo test
cargo run --release --bin eval -- \
  --config configs/v2_compact.toml \
  --dataset data/eval/queries.jsonl \
  --out results/v2.json --markdown results/v2.md
```

Проверки:

```bash
BASE_URL=http://127.0.0.1:8080 bash scripts/smoke.sh
MOCK_LLM=1 bash scripts/eval_all.sh
bash scripts/docker_smoke.sh
curl -s http://127.0.0.1:8080/api/health
curl -N -X POST http://127.0.0.1:8080/api/plan \
  -H 'content-type: application/json' \
  -d '{"query":"Хочу на выходные по Приморью на машине, не больше двух дней"}'
```

Фронтенд:

```bash
pnpm --dir src/frontend install
pnpm --dir src/frontend dev
pnpm --dir src/frontend build   # dist отдаёт axum
```

## Docker и CI

```bash
cp .env.example .env
docker compose up --build -d
docker compose logs -f travel-ai
curl -fsS http://127.0.0.1:8080/api/health
```

- `deploy/Dockerfile` — multi-stage Rust builder → debian slim, non-root `travel`,
  SQLite в `/var/lib/travel-ai`.
- `docker-compose.yml` — локальный запуск, named volume, healthcheck.
- `.github/workflows/ci.yml` — fmt/clippy/test/release build; на push в `main`
  публикует Docker-образ в GHCR (`ghcr.io/mikhaelcat/vibecode:latest` + `sha-*`).

Если compose-плагина нет:

```bash
docker build -f deploy/Dockerfile -t travel-ai:local .
docker run -d --name travel-ai --env-file .env -p 8080:8080 \
  -v travel-ai-data:/var/lib/travel-ai travel-ai:local
```

Подробнее: [deploy/README.md](deploy/README.md).

## Деплой на VM

```bash
cargo build --release
pnpm --dir src/frontend install && pnpm --dir src/frontend build
bash deploy/install.sh
sudo journalctl -u travel-ai -f
```

---

## API (кратко)

| Метод | Путь | Назначение |
|---|---|---|
| `POST` | `/api/plan` | запрос → SSE-поток плана |
| `POST` | `/api/plan/sync` | то же без SSE (эвал, отладка) |
| `GET` | `/api/runs/{id}` | план + trace |
| `GET` | `/api/stats` | запуски, токены, стоимость за сегодня |
| `GET` | `/api/health` | статус, версия, модель, бюджет |
| `GET` | `/` | SPA из `src/frontend/dist`, если фронт собран; иначе 404 |

SSE-события: `stage`, `intent`, `tool_call`, `tool_result`, `plan`, `validation`, `usage`, `refusal`, `done`. Ошибки до открытия потока — JSON 4xx; внутри прогона — fallback или `tool_result` с `ok:false`.

---

## Правила работы

- Маленькие атомарные коммиты, push после каждого логического шага.
- Контракты (`Intent`, `Plan`, `PlanDraft`, SSE, схемы тулов) меняются только через PR и сообщение в чат.
- Секреты — только в `.env`; в репо `.env.example`.
- Чужие зоны не переписываем без ревью.
- `main` держим зелёным: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.

---

## Открытые вопросы (закрыть до кодинга)

1. Эвал-набор выдаёт куратор или делаем свои 20 запросов?
2. В деке примеры про Урал, а продукт — Приморье; считаем целевым **Приморский край**.
3. Какая модель OpenRouter разрешена и какой дневной лимит считать целевым?
4. HTTPS/домен не нужен — работаем по `http://IP:PORT`?

---

## Данные и лицензии

- © OpenStreetMap contributors (ODbL).
- Open-Meteo (CC BY 4.0).
- OSRM / OpenRouteService — по условиям провайдеров.
- Жильё, расписания, события и наличие топлива на демо — моки; помечаются в API и UI.
