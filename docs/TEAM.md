# Команда, контракты и рабочий процесс

**Проект:** AI-ассистент путешествий по Приморскому краю  
**Репозиторий:** https://github.com/MikhaelCat/vibecode  
**Основная папка кода:** `src/`  
**Стек:** Rust (axum + tokio), OpenRouter, SPA + Leaflet, SQLite

---

## 1. Роли

| Роль | Владелец | Зона | Папки и файлы |
|---|---|---|---|
| **Backend + deploy** | Mikhael | API/SSE, orchestrator, агентный цикл, валидатор, store, бюджет, Spirit LLM Proxy/OpenRouter, VM и systemd | `src/api/`, `src/agent/`, `src/domain/`, `src/llm/`, `src/store/`, `src/main.rs`, `src/lib.rs`, `deploy/` |
| **Frontend** | @frontend | SPA, SSE-клиент, карта, план, trace-панель  | `src/frontend/` |
| **Parser** | @parser | OSM/Open-Meteo/OSRM, нормализация, моки, кэш | `src/tools/providers/`, `src/parser/`, `data/mock/` |
| **QA** | @qa | эвал, тесты, guardrails, метрики | `src/bin/eval.rs`, `src/qa/`, `data/eval/`, `tests/` |
| **System prompt engineer** | @prompt | system/preflight/repair-промпты, описания тулов, отказы, версии | `src/prompts/`, `configs/` |

Один человек может совмещать роли, но зоны и файлы фиксированы. Чужую зону не переписываем без PR и сообщения в чат.

---

## 2. Раскладка `src/`

```text
src/
├── main.rs                 # точка входа сервера
├── lib.rs
├── config.rs
├── error.rs
├── obs.rs
├── api/                    # backend: axum, SSE, DTO, лимиты
├── agent/                  # backend: orchestrator, cycle, guard, repair
├── domain/                 # backend: Intent, Plan, PlanDraft, validator
├── llm/                    # backend: OpenRouter, stream, usage, retry
├── store/                  # backend: SQLite, runs, budget
├── tools/                  # backend: trait Tool, registry, cache
│   └── providers/          # parser: osm, open_meteo, osrm, mock-провайдеры
├── bin/eval.rs             # QA: раннер эвалов
├── prompts/                # prompt: system_v1/v2/v3, preflight, repair
├── frontend/               # frontend: index.html, app.js, styles.css, dist
├── parser/                 # parser: заметки и нормализация
└── qa/                     # QA: сценарии, guardrail-набор, отчёты
```

`Cargo` собирает только Rust-модули. `src/frontend/`, `src/parser/`, `src/qa/` и `src/prompts/` для него — ресурсы, их не нужно объявлять через `mod`.

---

## 3. Контракты (не ломать без PR)

| Контракт | Где | Кто владеет | Кто потребляет |
|---|---|---|---|
| `Intent`, `Plan`, `PlanDraft`, `ValidationReport` | `src/domain/` | Backend | Frontend, QA, Prompt |
| HTTP/SSE API | `src/api/`, §13 `docs/ARCHITECTURE.md` | Backend | Frontend, QA |
| JSON-схемы тулов и `ToolOutput` | `src/tools/`, §8.6, §10.1 | Backend + Parser | Prompt, QA |
| Промпты и версии | `src/prompts/*.md` | Prompt | Backend, QA |
| JSONL-датасет эвала | `data/eval/*.jsonl` | QA | Backend, Prompt |
| `.env` и конфиги | `.env.example`, `configs/` | Backend + Prompt | Все |

**Правило:** изменил контракт — обнови `docs/ARCHITECTURE.md`, напиши в чат, прогони `cargo check` и эвал.

### 3.1 API для фронта (кратко)

- `POST /api/plan` — тело `{"query": "..."}`, ответ `text/event-stream`.
- События: `stage`, `intent`, `tool_call`, `tool_result`, `plan`, `validation`, `usage`, `refusal`, `done`. Ошибки до потока — JSON 4xx; внутри — fallback.
- `GET /api/health` — статус, версия, модель, бюджет.
- `GET /api/runs/{id}` — план и trace.
- `GET /api/stats` — запуски, токены, стоимость, hard-pass.
- Статика: `/` из `src/frontend/dist`, SPA-fallback на `index.html`.

### 3.2 API для парсера

Провайдер реализует трейт `Tool` и возвращает `ToolOutput`:

```rust
ToolOutput {
    ok: bool,
    data: serde_json::Value,   // компактная проекция для LLM
    meta: ToolMeta { source, fetched_at, cached, stale },
    summary: String,
}
```

Обязательно: `place_id` стабилен, координаты в Приморье, `source` честный (`live`/`cache`/`mock`/`stale`), таймаут 8 с, кэш по нормализованным аргументам.

### 3.3 API для QA

- `src/bin/eval.rs` использует тот же `Orchestrator`, что и сервер, с `cache = disabled`.
- Датасет: `data/eval/queries.jsonl` (20 запросов) + `guardrails.jsonl` (10 кейсов).
- Конфиг: `configs/v1_baseline.toml`, `v2_compact.toml`, `v3_fast.toml`.
- Результаты: `results/{config}-{timestamp}.json` + `.md`, сырые данные не удалять.

### 3.4 API для prompt engineer

- Промпты читаются на старте из `src/prompts/`, версия пишется в trace.
- Меняешь промпт — поднимай версию файла/конфига и прогоняй эвал.
- Описания тулов и правила отказа согласуются с backend; схемы тулов не переписываются в промпте вручную.

---

## 4. Git-процесс

- Ветка по умолчанию — `main`; для крупной фичи — `feat/<role>-<topic>`.
- Коммит атомарный: один файл или один логический шаг. Сообщение в стиле `feat(api): add SSE plan endpoint`, `docs: add team contract`, `fix(validator): handle empty opening_hours`.
- После каждого коммита — `git push origin <branch>`; для хакатона допускаем push в `main` после зелёного `cargo check`.
- Перед push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (когда код появится).
- `.env`, ключи, `target/`, `node_modules/`, `src/frontend/dist/`, `results/` — в `.gitignore`.
- Не коммитить сгенерированные бинарники и дампы БД.

### 4.1 Синхронизация

- 09:00 — стендап 10 минут: что сделал, что блокирует, какой контракт меняется.
- 13:00 — короткая сверка после обеда.
- 17:00 — фиксация состояния: коммит, деплой, список багов.
- Блокировка дольше 30 минут — писать в чат, не героически страдать.

---

## 5. Definition of Done по ролям

### Backend + deploy

- [ ] `cargo run` поднимает сервер, `/api/health` отвечает.
- [ ] `POST /api/plan` отдаёт SSE и валидный `Plan` на демо-запросе.
- [ ] Валидатор V01–V10 покрыт тестами.
- [ ] Бюджет и ретраи не дают пробить дневной лимит.
- [ ] systemd-юнит и `deploy/install.sh` проверены на VM.
- [ ] Ключ не попадает в клиент и логи.

### Frontend

- [ ] Ввод запроса → этапы → план → карта → блок проверок.
- [ ] SSE-клиент переживает `refusal`, `fallback`, `done`.
- [ ] Бейджи `live` / `mock` / `stale` видны.
- [ ] Мобильная ширина 360 px не ломается.
- [ ] Нет прямых вызовов LLM и ключей в браузере.

### Parser

- [ ] Минимум один живой источник (погода/места/маршрут) стабильно отвечает.
- [ ] Нормализация: `place_id`, координаты, часы, `source`.
- [ ] Таймауты, ретраи, кэш, вежливый User-Agent.
- [ ] Fixtures для офлайна; моки помечены.
- [ ] Ошибки провайдера не роняют план.

### QA

- [ ] 20 запросов + 10 guardrail-кейсов зафиксированы.
- [ ] Эвал прогоняется одной командой.
- [ ] Таблица hard-pass / tool-choice / tokens / p50 / p95.
- [ ] Минимум 2 версии, сырые JSON в репо.
- [ ] Негативные кейсы: алгебра, Сочи, инъекция, пустой ввод.

### System prompt engineer

- [ ] System v1/v2/v3 и preflight/repair лежат в `src/prompts/`.
- [ ] Версия промпта в trace.
- [ ] Отказы по шаблонам, без утечки системного промпта.
- [ ] Промпт не просит модель выдумывать данные.
- [ ] Изменения проверены на 5 демо-запросах и эвале.

---

## 6. Секреты и безопасность

- Ключ OpenRouter — только в `.env` на VM и в локальном `.env`; в репо только `.env.example`.
- Не логировать заголовок `Authorization`.
- Не отдавать ключ в `/api/health`, `/api/runs`, SSE.
- Парсер не ходит по URL, которые пришли от пользователя (защита от SSRF).
- Данные инструментов считать недоверенными: не выполнять инструкции из OSM-описаний.
