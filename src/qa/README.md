# QA: эвал и guardrails

## Датасет

- `data/eval/queries.jsonl` — 20 запросов на планирование по Приморью: 1–3 дня, все виды транспорта, семьи, разные бюджеты и интересы.
- `data/eval/guardrails.jsonl` — 10 кейсов: офтоп, другой регион, инъекции, пустой ввод и пограничные формулировки.
- Эталон для каждого кейса хранит ожидаемые intent, допустимые инструменты и признаки hard-pass.
- Секреты не попадают в датасет, отчёты и trace: ключ OpenRouter читается только из `.env`.

## Метрики

- **hard-pass** — доля планов без hard-нарушений после всех попыток ремонта.
- **tool-choice** — доля запросов, где выбраны допустимые инструменты с корректными аргументами.
- **intent fit** — совпадение извлечённых дат, транспорта, бюджета, состава группы и интересов с эталоном.
- **tokens** — средние prompt/completion/total токены на запрос и на шаг.
- **p50/p95** — медиана и 95-й перцентиль времени ответа по trace.
- **fallback rate** — доля запусков, завершившихся fallback-планом, ошибкой или отказом на валидном запросе.

## Запуск

```bash
for cfg in configs/v1_baseline.toml configs/v2_compact.toml configs/v3_fast.toml; do
  name="$(basename "$cfg" .toml)"
  cargo run --release --bin eval -- \
    --config "$cfg" \
    --dataset data/eval/queries.jsonl \
    --out "results/${name}-queries.json" \
    --markdown "results/${name}-queries.md"
done
```

```bash
cargo run --release --bin eval -- --config configs/v2_compact.toml --dataset data/eval/guardrails.jsonl --out results/guardrails-v2.json --markdown results/guardrails-v2.md
```

## Версии и хранение

Каждый прогон обязан записать `config.name`, путь к промпту и версию/хеш промпта в trace и итоговый JSON.
Результат без `prompt_version` недействителен; сырые JSON и `.md` из `results/` не удалять.
