# Деплой travel-ai

Один release-бинарник под systemd. Приложение: `/opt/travel-ai`, состояние:
`/var/lib/travel-ai`, unit: `travel-ai.service`. Нужен Linux x86_64 с systemd.
Без systemd есть Docker: образ из `deploy/Dockerfile` и `docker-compose.yml`
(см. §7).

## 1. Переменные окружения
```bash
sudo install -d /opt/travel-ai
sudo install -m 0640 .env.example /opt/travel-ai/.env
sudoedit /opt/travel-ai/.env
```

Заполните один из путей LLM:

- **T-Bank Spirit LLM Proxy (приоритет):** `LLM_PROXY_TOKEN`, `LLM_PROXY_URL`
  (`https://llm-proxy.spirit.t-education.com/v1`), `LLM_PROXY_MODEL`
  (`openrouter/deepseek/deepseek-v4.1-flash`). URL должен включать `/v1`: к нему
  дописывается `/chat/completions`, без `/v1` прокси отвечает 404.
- **OpenRouter:** `OPENROUTER_API_KEY` и `OPENROUTER_MODEL`
  (плюс необязательный `OPENROUTER_FALLBACK_MODELS`).

Без ключа (или при `MOCK_LLM=1`) включается детерминированный MockLlm —
подменяется только LLM; инструменты при наличии сети по-прежнему берут живые
данные (OSM, Wikipedia, Nominatim, Яндекс.Погода, Open-Meteo, VL.ru), а при их недоступности —
каталог `data/mock`. Для публичного демо задайте `APP_TOKEN`: он передаётся в
`x-app-token` или `Authorization: Bearer` и закрывает `/api/plan`,
`/api/plan/sync`, `/api/stats` и `/api/runs/{id}`. Расход ограничивают
`DAILY_BUDGET_USD` (по умолчанию 1 000 000) и `DAILY_TOKEN_BUDGET` (по умолчанию
2·10¹⁸ токенов — фактически без дневного лимита); стоимость считается по
`PRICE_IN_PER_M` и `PRICE_OUT_PER_M` — цена за 1M токенов, `0` = неизвестна.

Для systemd укажите `DATABASE_URL=sqlite:///var/lib/travel-ai/app.db` (каталог
создаёт `install.sh`); пути `PROMPTS_DIR`, `DATA_DIR` и `WEB_DIR` относительны
`WorkingDirectory=/opt/travel-ai`; `BIND` по умолчанию — `0.0.0.0:8080`.

**Redis (опционально):** установите `redis-server`, задайте
`REDIS_URL=redis://127.0.0.1:6379/0` и не открывайте 6379 наружу. Redis хранит
только кэш результатов тулов (TTL 10 минут); SQLite остаётся источником истины.
Если `REDIS_URL` пуст или Redis недоступен, приложение использует in-memory кэш.

## 2. Сборка
```bash
cargo build --release
npm --prefix src/frontend run build   # или just web
```

Нужны `target/release/travel-ai`, `src/prompts`, `data`, `migrations` и
`src/frontend/dist`. `data/` обязателен уже при компиляции: geo-JSON и
`data/mock` встроены в бинарник через `include_str!`. Сборка — под Linux x86_64
с той же libc, что на сервере; альтернатива:
`docker build -f deploy/Dockerfile -t travel-ai:local .` — внутри образа
фронтенд копируется в `src/frontend/dist` сам, без npm.

## 3. Установка
Запускайте из корня репозитория:

```bash
sudo bash deploy/install.sh
```

Скрипт создаёт пользователя `travel` и каталоги, сохраняет предыдущий бинарник
как `/opt/travel-ai/travel-ai.previous`, копирует бинарник, prompts, `data` и
migrations, ставит unit и запускает сервис. `/opt/travel-ai/.env` обязателен,
также нужны `curl` и systemd. Если `src/frontend/dist` нет, сервис стартует без
статики (предупреждение) — соберите фронтенд заранее. Health `/api/health`
проверяется с повторами до 30 секунд.

## 4. systemd и проверка
```bash
sudo systemctl status travel-ai
sudo systemctl restart travel-ai
sudo systemctl stop travel-ai
sudo journalctl -u travel-ai -f
curl -fsS http://127.0.0.1:8080/api/health
```

`/api/health` возвращает `status`, `version`, `model`, `mock_llm` (true, когда
реальный LLM не настроен), `prompt_version`, `auth_required`, `uptime_secs` и
`budget` с `daily_usd` и `spent_today_usd`.

## 5. Обновление и откат
Соберите новую версию и повторите `sudo bash deploy/install.sh`: скрипт сохранит
предыдущий бинарник в `/opt/travel-ai/travel-ai.previous`. Перед обновлением сделайте backup БД.

Откат:

```bash
sudo install -m 0755 /opt/travel-ai/travel-ai.previous /opt/travel-ai/travel-ai
sudo systemctl restart travel-ai
```

## 6. Порт, firewall и SSE
По умолчанию сервис слушает `0.0.0.0:8080`; порт задаётся `BIND` в `.env`.
Откройте его в firewall VM и облака (если меняли `BIND` — соответствующий порт):

```bash
sudo ufw allow 8080/tcp
```

Не публикуйте SQLite и Redis. При reverse proxy отключите буферизацию SSE:
`proxy_buffering off;`, `proxy_cache off;`, `proxy_read_timeout 3600s;`, иначе
события придут пачками. Проверяйте через `curl -N`.

## 7. Docker локально

Через compose (нужен плагин):

```bash
cp .env.example .env
# без ключа включится MockLlm
docker compose up --build -d
docker compose logs -f travel-ai
curl -fsS http://127.0.0.1:8080/api/health
```

Compose поднимает приложение и `redis:7-alpine` (`REDIS_URL=redis://redis:6379/0`,
приложение ждёт healthy Redis), публикует `8080:8080` и хранит SQLite в named
volume `travel-ai-data` (`/var/lib/travel-ai`), поэтому `docker compose down` не
удаляет базу; полный сброс — `docker compose down -v`. Healthcheck ходит в
`/api/health`.

На этой машине compose-плагина нет — рабочий вариант обычным Docker:

```bash
docker build -f deploy/Dockerfile -t travel-ai:local .
docker run -d --name travel-ai --network host --env-file .env \
  -e BIND=0.0.0.0:8080 \
  -e DATABASE_URL=sqlite:///var/lib/travel-ai/app.db \
  -e PROMPTS_DIR=/app/src/prompts -e DATA_DIR=/app/data \
  -e WEB_DIR=/app/src/frontend/dist -e REDIS_URL= \
  -e DAILY_BUDGET_USD=1000000 -e DAILY_TOKEN_BUDGET=2000000000000000000 \
  -v travel-ai-data:/var/lib/travel-ai travel-ai:local
curl -fsS http://127.0.0.1:8080/api/health
```

- `--network host` обязателен: с bridge-сети контейнер не резолвит хост
  LLM-прокси (`llm-proxy.spirit.t-education.com`), а с хоста он резолвится.
  Порт слушает сам процесс (`BIND`), поэтому `-p` не нужен.
- `-e REDIS_URL=` отключает Redis (в этом варианте Redis-контейнера нет),
  кэш тулов остаётся в памяти.
- `-e ...` перекрывают `.env`: база попадает в volume, а не в `/app`;
  дублируемые `DAILY_*` совпадают с `.env.example`.
- Образ multi-stage: Rust-зависимости собираются один раз за stub-слоем,
  runtime — `debian:bookworm-slim` с ca-certificates/curl, non-root пользователь
  `travel`; `data/` копируется в образ, потому что geo-JSON встроены в бинарник.

Обновление через compose — `docker compose build --pull && docker compose up -d`;
без compose — `docker rm -f travel-ai`, пересборка и запуск команды выше.

Полная проверка образа без compose:

```bash
bash scripts/docker_smoke.sh        # IMAGE, PORT, CONTAINER, KEEP=1 — опционально
```

Скрипт собирает образ, поднимает временный контейнер с `MOCK_LLM=1` (порт
18090), ждёт до 30 с `/api/health` (`status=ok`, `mock_llm=true`), проверяет
SPA (`/` и `/app.js`), синхронный план на запрос про машину и выходные (дни и
пункты, `hard_failed=false`), отказ на офтоп и SSE-поток с событиями `plan` и
`done`. Нужны `docker`, `curl`, `python3`; контейнер удаляется на выходе, если
не задан `KEEP=1`, а при ошибке печатаются последние 80 строк логов.

## 8. GitHub Actions и GHCR

`.github/workflows/ci.yml` на каждый push и на PR в `main`:

1. Job `backend`: `cargo fmt --all -- --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`,
   `cargo build --release`, затем копирование `index.html`, `styles.css` и
   `app.js` в `src/frontend/dist`.
2. Job `docker` (после `backend`): buildx-сборка `deploy/Dockerfile`; на push в
   `main` образ публикуется в GHCR: `ghcr.io/mikhaelcat/vibecode:latest`, плюс
   теги ветки и `sha-<commit>` (на PR — тег `pr-<номер>`, без публикации).

Права: `contents: read`, `packages: write`; используется встроенный
`GITHUB_TOKEN`, отдельный секрет не нужен. Для приватного пакета добавьте
`docker login ghcr.io -u <user>` с PAT `read:packages` на машине деплоя.

Забрать опубликованный образ на VM (флаги — как в §7):

```bash
docker pull ghcr.io/mikhaelcat/vibecode:latest
docker run -d --name travel-ai --network host --env-file .env \
  -e BIND=0.0.0.0:8080 \
  -e DATABASE_URL=sqlite:///var/lib/travel-ai/app.db \
  -e PROMPTS_DIR=/app/src/prompts -e DATA_DIR=/app/data \
  -e WEB_DIR=/app/src/frontend/dist -e REDIS_URL= \
  -e DAILY_BUDGET_USD=1000000 -e DAILY_TOKEN_BUDGET=2000000000000000000 \
  -v travel-ai-data:/var/lib/travel-ai \
  ghcr.io/mikhaelcat/vibecode:latest
```

## 9. Backup SQLite
```bash
sudo install -d /var/backups/travel-ai
sudo sqlite3 /var/lib/travel-ai/app.db \
  ".backup '/var/backups/travel-ai/app-$(date +%F).db'"
```

Для восстановления остановите сервис, положите backup в `/var/lib/travel-ai/app.db`,
удалите `app.db-wal` и `app.db-shm`, верните владельца `travel:travel`, запустите
сервис снова.
