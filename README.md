# vibecode — AI-ассистент путешествий по Приморскому краю

Веб-ассистент: свободный запрос на русском → проверенный план поездки на 1–3 дня по Приморскому краю: места, маршрут, ночёвка, погода, ограничения, карта и trace того, как план собран.

**Хакатон:** Т-Банк × Сириус · 2 дня · защита 8 + 3 минуты  
**Стек:** Rust (axum + tokio), OpenRouter, OpenStreetMap / Open-Meteo / OSRM, SQLite, SPA + Leaflet  
**Основная папка кода:** [`src/`](src/)

---

## Документация

- [Архитектура и план проекта](docs/ARCHITECTURE.md) — требования, компоненты, агентный цикл, тулы, валидатор, API, эвал, деплой, таймлайн.
- [Команда и контракты](docs/TEAM.md) — роли, зоны ответственности, API для фронта/парсера/QA, git-процесс, Definition of Done.

Коротко о главном:

- **Ядро** — агентный цикл tool calling + терминальный `submit_plan`, а не один вызов LLM.
- **Качество** — детерминированный валидатор плана (V01–V15) и провенанс: каждая точка ссылается на `place_id` из тула.
- **Данные** — погода, места, геокодинг и маршруты живые; жильё, расписания и наличие топлива — моки с явной пометкой.
- **Guardrails** — только Приморский край и путешествия; отказ на другие регионы, офтоп и инъекции.
- **Замеры** — 20 запросов × 2–3 версии пайплайна: hard-pass, tool-choice, токены, p50/p95.

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
│   ├── agent/              # orchestrator, cycle, guard, repair
│   ├── domain/             # Intent, Plan, PlanDraft, validator
│   ├── llm/                # OpenRouter, stream, usage, retry
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
├── docs/
└── README.md
```

`src/` — главная папка: Rust-бэкенд лежит в корне `src/`, команды — в `src/frontend/`, `src/parser/`, `src/qa/`, `src/prompts/`. Cargo собирает только Rust-модули, вложенные папки команд для него — ресурсы.

---

## Быстрый старт (после появления backend)

```bash
cp .env.example .env       # OPENROUTER_API_KEY, OPENROUTER_MODEL
cargo run                  # сервер на http://127.0.0.1:8080
cargo test
cargo run --release --bin eval -- \
  --config configs/v2_compact.toml \
  --dataset data/eval/queries.jsonl \
  --out results/v2.json --markdown results/v2.md
```

Фронтенд:

```bash
pnpm --dir src/frontend install
pnpm --dir src/frontend dev
pnpm --dir src/frontend build   # dist отдаёт axum
```

Деплой:

```bash
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
| `GET` | `/api/stats` | запуски, токены, стоимость, hard-pass |
| `GET` | `/api/health` | статус, версия, модель, бюджет |
| `GET` | `/` | SPA из `src/frontend/dist` |

SSE-события: `stage`, `intent`, `tool_call`, `tool_result`, `plan`, `validation`, `usage`, `refusal`, `error`, `done`.

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
