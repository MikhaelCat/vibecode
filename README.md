# vibecode — планировщик поездок по Приморскому краю

Свободный запрос на русском → проверенный план на 1–7 дней: места, переезды,
ночёвка, погода, события и карта. Бэкенд на Rust, интерфейс — ванильный
JavaScript + Leaflet без сборщика.

## Из чего состоит проект

Проект разделён на четыре независимых слоя. У каждого своя папка и своя
ответственность; HTTP-обработчики не ходят во внешние API напрямую.

| Слой | Папка | Что делает |
|---|---|---|
| **Backend** | `src/api/`, `src/agent/`, `src/domain/`, `src/store/`, `src/llm/` | HTTP/SSE, агентный цикл, правила плана, SQLite |
| **Parsing** | `src/tools/` | Провайдеры данных: OSM, Wikipedia, погода, маршруты, события, жильё, АЗС, кэш |
| **Frontend** | `src/frontend/` | Форма, SSE-клиент, дни плана, жильё, ленивая карта Leaflet |
| **QA** | `tests/`, `src/bin/eval.rs`, `data/eval/`, `scripts/` | Тесты, эвалы, guardrails, браузерные и живые проверки |

```
src/
├── main.rs              # запуск сервера
├── lib.rs               # сборка состояния и LLM-клиента
├── api/                 # axum: /api/plan, /api/plan/sync, health, stats
├── agent/               # guard → tools → submit_plan → validate → repair
├── domain/              # Intent, Plan, Facts, geo, validator V01–V15, fallback
├── llm/                 # OpenAI-совместимый клиент + MockLlm
├── store/               # SQLite: runs и дневной бюджет
├── tools/               # registry, cache и providers/*
├── prompts/             # system/preflight/repair-промпты
└── frontend/            # index.html, app.js, styles.css, dist/
```

### Поток запроса

1. `POST /api/plan` разбирает даты, транспорт и интересы (`domain::intent`).
2. Guard отсекает офтоп, инъекции и другие регионы.
3. Агент вызывает инструменты; результаты складываются в `Facts` с `place_id`
   и источником. Независимые вызовы идут параллельно.
4. Модель вызывает `submit_plan`; `domain::plan::enrich` раскладывает точки по
   дням, убирает дубли и считает переезды.
5. Валидатор проверяет регион, даты, погоду, часы работы, жильё и провенанс.
   При ошибке — repair, затем безопасный fallback.
6. Готовый план уходит в SSE; фронтенд рисует дни и маркеры.

### Карта

Маркер создаётся только для конечных координат внутри Приморья
(`42.2…48.5`, `130.0…139.1`). Бэкенд дополнительно проверяет сушу: точки в
бухтах, озёрах и море отбрасываются (`domain::geo::on_land`), а берег имеет
допуск ~200 м, чтобы пляжи, мысы и пирсы оставались в плане. Озёра и реки
используются как районы поиска, точки выбираются на берегу. Бары и клубы
ищутся только по явному запросу. Линия маршрута дня рисуется фронтендом по
геометрии OSRM и не попадает в контекст LLM.

## Быстрый старт

```bash
cp .env.example .env          # без ключа включится MockLlm
cargo run                     # http://127.0.0.1:8080
```

Порт и адрес задаются через `BIND` в `.env`. Если `src/frontend/dist` ещё нет:

```bash
npm --prefix src/frontend run build
# без npm: mkdir -p src/frontend/dist && cp src/frontend/*.{html,css,js} src/frontend/dist/
```

Проверки:

```bash
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
bash scripts/qa.sh            # mock-сервер, smoke, браузерный тест
bash scripts/docker_smoke.sh  # сборка образа и проверка контейнера
```

## LLM

Поддерживаются два OpenAI-совместимых пути:

1. **T-Bank Spirit LLM Proxy** — если задан `LLM_PROXY_TOKEN`.
   `LLM_PROXY_URL` должен включать `/v1`; модель по умолчанию
   `openrouter/deepseek/deepseek-v4.1-flash`.
2. **OpenRouter** — если задан `OPENROUTER_API_KEY` и нет proxy-токена.

Без ключа работает `MockLlm`: агент, инструменты, валидатор, SSE и эвалы
проверяются офлайн. `MOCK_LLM=1` подменяет только модель — инструменты
по-прежнему сначала обращаются к живым провайдерам (OSM/Overpass, Wikipedia,
Nominatim, Open-Meteo, VL.ru) и переходят на `data/mock` лишь при их
недоступности. Ключи хранятся только в `.env` (файл в `.gitignore`);
в репозитории есть только `.env.example`.

Для публичного демо задайте `APP_TOKEN` — оба POST-эндпоинта начнут требовать
`x-app-token`. Расход ограничивают `DAILY_BUDGET_USD` и `DAILY_TOKEN_BUDGET`.

## Данные

| Данные | Источник | Fallback |
|---|---|---|
| Места, еда, отели, АЗС | OpenStreetMap Overpass | `data/mock/*.json` |
| Кафе, бары, заведения | Яндекс Карты Search API (если задан ключ) / OSM Overpass | `data/mock/*.json` |
| Достопримечательности | Wikipedia geosearch | `data/mock/*.json` |
| Города, озёра, реки | Nominatim / Photon | список известных районов |
| Погода | Open-Meteo | VL.ru |
| Маршруты | OSRM | расчёт по прямой |
| События | VL.ru (JSON-LD) | пустой список |
| Жильё | OSM + локальный справочник | `data/mock/lodging.json` |
| Суша и границы | OSM Nominatim (граница края) + GSHHS (суша) | bbox края |

`data/mock` — единственный источник локальных заглушек: каталог реальных
объектов Приморья (координаты сверены с OSM через Nominatim/Photon).
Живые данные помечаются `source: live`, fallback — `mock`/`stale`; валидатор
не выдаёт непроверенную погоду за прогноз. Геоданные лежат в `data/geo/`:
граница края из OSM и суша из GSHHS, источники и лицензии — в
`data/geo/README.md`.

## API

| Метод | Путь | Назначение |
|---|---|---|
| `POST` | `/api/plan` | запрос → SSE-поток |
| `POST` | `/api/plan/sync` | тот же результат без SSE |
| `GET` | `/api/runs/{id}` | план + trace |
| `GET` | `/api/stats` | запуски, токены, стоимость за сегодня |
| `GET` | `/api/health` | статус, модель, остаток бюджета |

SSE-события: `stage`, `intent`, `tool_call`, `tool_result`, `plan`,
`validation`, `usage`, `refusal`, `done`. Подробнее — [docs/API.md](docs/API.md).

## SQLite и Redis

- **SQLite** (`DATABASE_URL`) — источник истины: таблицы `runs` и
  `daily_budget`. `/api/runs/{id}`, `/api/stats` и бюджет в `/api/health`
  читают только её.
- **Redis** (`REDIS_URL`, необязательно) — общий кэш результатов инструментов
  на 10 минут. Если Redis недоступен, используется кэш в памяти. SQLite от
  Redis не зависит.

## Docker и деплой

```bash
cp .env.example .env          # без ключей включится MockLlm
docker compose up --build -d
curl -fsS http://127.0.0.1:8080/api/health
```

Если compose-плагина нет:

```bash
docker build -f deploy/Dockerfile -t travel-ai:local .
docker run -d --name travel-ai --env-file .env -p 8080:8080 \
  -v travel-ai-data:/var/lib/travel-ai travel-ai:local
# без ключей: docker run ... -e MOCK_LLM=1 ...
```

Проверка образа целиком (build, запуск, health, sync, SSE, SPA):

```bash
bash scripts/docker_smoke.sh
```

- `deploy/Dockerfile` — multi-stage сборка, зависимости кэшируются отдельным слоем, non-root пользователь.
- `docker-compose.yml` — приложение, Redis, volume для SQLite.
- `deploy/install.sh` + `deploy/travel-ai.service` — установка на VM.
- CI (`.github/workflows/ci.yml`) гоняет fmt, clippy, тесты и собирает образ.

Подробности: [deploy/README.md](deploy/README.md).

## Документация

- [Архитектура](docs/ARCHITECTURE.md) — слои, поток данных, правила.
- [API](docs/API.md) — контракт SSE и sync.
- [UI](docs/UI.md) — экран и состояния.
- [Замеры](docs/EVAL.md) — методология и результаты.

## Лицензии

- © OpenStreetMap contributors, ODbL.
- Open-Meteo, CC BY 4.0.
- VL.ru — афиша (JSON-LD, robots.txt разрешает страницу).
- OSRM — по условиям провайдера.
