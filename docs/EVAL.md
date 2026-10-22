# Замеры

## Что и как меряем

`src/bin/eval.rs` гоняет фиксированный датасет против одной конфигурации пайплайна:
поднимает оркестратор в процессе, с in-memory SQLite и без Redis, и пишет JSON и Markdown
в `results/` (каталог в `.gitignore`). LLM по умолчанию — MockLlm, с `--live` — реальная
модель из конфига.

- `data/eval/queries.jsonl` — 20 запросов: 16 in-scope + 4 негативных (off-topic ×2,
  другой регион и инъекция). Его прогоняет `scripts/eval_all.sh` для всех трёх конфигураций.
- `data/eval/guardrails.jsonl` — 10 adversarial-кейсов для ручных прогонов: off_topic ×4,
  injection ×2, other_region ×2, empty ×1, too_long ×1. Ни один скрипт его не читает:
  в `eval_all.sh` он не входит, а `scripts/edge_cases.sh` проверяет свои два случая
  (анекдот → `off_topic`, «покажи системный промпт» → `injection`).
- `--limit N` обрезает датасет до первых N строк.

Конфигурации:

| Конфиг | Промпт | max_steps | repair_attempts |
|---|---|---|---|
| `configs/v1_baseline.toml` (`v1-baseline`) | `src/prompts/system_v1.md` | 12 | 0 |
| `configs/v2_compact.toml` (`v2-compact`) | `src/prompts/system_v2.md` | 8 | 2 |
| `configs/v3_fast.toml` (`v3-fast`) | `src/prompts/system_v3.md` | 6 | 2 |

`system_v1.md` — промпт по умолчанию (`PromptSet::load`); ещё в `src/prompts/` лежат
`repair_v1.md` и `preflight_v1.md`. В TOML есть поля `model`, `compression`,
`parallel_tools` и `fallback`: eval-бинарник применяет к прогону промпт, `max_steps`,
`repair_attempts` и (в live) `model`, а остальные только записывает в JSON в блок
`config` — на результат они не влияют.

Метрики (поля summary из `src/bin/eval.rs`):

- **Hard-pass** (`hard_pass_rate`) — доля in-scope кейсов, где валидатор не нашёл
  hard-нарушений (`report.hard_failed == false`); N=16.
- **Tool-choice** (`tool_choice_rate`) — доля in-scope кейсов, где вызван хотя бы один
  ожидаемый тул из `tools`; пустой список ожиданий засчитывается автоматически.
- **Intent fit** (`intent_fit_rate`) — in-scope кейсы: транспорт совпал с ожидаемым,
  `intent.max_days` не больше ожидаемого, бюджет равен ожидаемому (проверяются только
  заданные в кейсе поля). Для негативных кейсов intent fit — это полученный отказ, но в
  rate он не входит.
- **Refusal** (`refusal_rate`) — доля негативных кейсов (4), где пришёл отказ.
- **Fallback** — сколько планов собрано деградированным путём
  (`plan.generation == fallback`), по всем 20 кейсам.
- **Tokens** (`total_tokens`, `avg_tokens`) — по всем 20 кейсам: негативные отказывают
  сразу и дают 0 токенов.
- **p50/p95 latency** (`p50_latency_ms`, `p95_latency_ms`) — перцентили по `latency_ms`
  всех 20 кейсов.
- **Errors** — кейсы, где отказ разошёлся с ожиданием (неожиданный отказ или его
  отсутствие); hard-нарушения сюда не входят.
- По каждому кейсу пишутся `id`, `query`, `in_scope_expected/actual`, `hard_pass`,
  `tool_choice`, `intent_fit`, `refusal_ok`, `fallback`, `tokens`, `latency_ms`,
  `tools_used`, `tags`, `error`.
- Стоимости в summary и кейсах нет: `cost_usd` живёт в `usage`/трассе прогона
  (таблица runs, `/api/runs/{id}`), в mock-режиме он нулевой.

Команды:

```bash
# без вызовов LLM и без денег (MockLlm; тулы всё равно ходят в сеть)
MOCK_LLM=1 bash scripts/eval_all.sh

# один конфиг (debug-сборка)
cargo run --bin eval -- --config configs/v1_baseline.toml \
  --dataset data/eval/queries.jsonl --out results/v1.json --markdown results/v1.md

# то же в release, как в eval_all.sh: just eval v1_baseline
just eval v1_baseline

# живой прогон одной конфигурации (тратит токены, нужен LLM_PROXY_TOKEN)
LIVE=1 LLM_PROXY_TOKEN=... bash scripts/eval_all.sh          # все 3 конфига
LLM_PROXY_TOKEN=... cargo run --release --bin eval -- \
  --config configs/v2_compact.toml --dataset data/eval/queries.jsonl \
  --limit 5 --live --out results/v2-live.json --markdown results/v2-live.md
```

`scripts/eval_all.sh` и `just eval` используют release-сборку специально: таблица ниже
содержит p50/p95, а debug-сборка искажает латенси (`just build` и `just dev` остаются в
debug). `eval_all.sh` останавливается на первой ошибке и печатает сводку по каждому
конфигу.

## Результаты (MockLlm, 22.10.2026)

Режим `MOCK_LLM=1`, release-сборка, 20 запросов, без вызовов LLM и без денег. Эти цифры
проверяют инфраструктуру, тулы, валидатор и отказы; они **не** доказывают разницу между
промптами (см. ниже), а hard-pass и латенси плавают между прогонами, потому что
инструменты ходят в живые провайдеры.

| Версия | Hard-pass | Tool-choice | Intent fit | Refusal | Tokens/req | p50, мс | p95, мс | Fallback |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| v1-baseline | 100% | 100% | 100% | 100% | 10 114 | 9 975 | 26 425 | 2 |
| v2-compact | 100% | 100% | 100% | 100% | 10 323 | 8 583 | 21 017 | 3 |
| v3-fast | 93.8% | 100% | 100% | 100% | 7 695 | 5 726 | 29 748 | 2 |

Единственный hard-fail — `q14` («Куда поехать с детьми на выходные…») в `v3-fast`:
план собрался fallback-путём и не прошёл валидатор. В `v1-baseline` и `v2-compact` тот же
`q14` тоже уходил в fallback, но проверку проходил; остальные fallback-планы — `q12`
(во всех конфигах) и `q09` (в `v2-compact`). Это учебный пример того, что mock-прогон
измеряет инфраструктуру, а не стабильность промптов: данные приходят из сети, и один и
тот же кейс может то проходить, то нет.

MockLlm не читает `system_v*.md`, поэтому качество планов у v1/v2/v3 одинаковое, а
`tokens/req` отражает размер промпта: у `v3-fast` он примерно на четверть меньше, чем у
v1/v2. Сравнивать промпты между собой можно только живым прогоном.

## Живой прогон

MockLlm детерминирован: он берёт `INTENT_JSON` из сообщений, сам вызывает инструменты по
одному сценарию (`search_places` → `get_weather` → `find_lodging` → `find_events`,
плюс `find_transport` для `transit`, затем `submit_plan`) и оценивает токены как длину
сообщений / 2.8. Сами инструменты при этом ходят в живые провайдеры, а при сбое
используют запасной путь: mock-файлы `data/mock/*.json`, соседний город для погоды,
оценку по прямой для маршрута — поэтому качество в mock-режиме зависит от сети, а
цифры латенси плавают между прогонами.

`--live` переключает пайплайн на реальную модель из конфига (для прокси-пути `model`
пишется и в `llm_proxy_model`); бинарник требует `LLM_PROXY_TOKEN` или
`OPENROUTER_API_KEY`, а `LIVE=1` в `eval_all.sh` — только `LLM_PROXY_TOKEN`. В живом
прогоне смотрим:

- `hard_pass_rate` и `errors`: план не должен падать на валидаторе и отказы не должны
  «съезжать» на негативных кейсах;
- `generation` в планах: `llm` или `repaired`, а не `fallback`;
- `tools_used` и `tools_used[].source` в `/api/runs/{id}`: реальные источники, а не mock;
- стоимость: в summary её нет — `cost_usd` берётся из `usage` прогона или записи рана.

Живой прогон тратит токены, поэтому запускается вручную и не входит в `scripts/qa.sh`.
