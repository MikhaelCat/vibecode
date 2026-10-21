# Деплой travel-ai

Один release-бинарник под systemd. Приложение: `/opt/travel-ai`, состояние:
`/var/lib/travel-ai`, unit: `travel-ai.service`. Нужен Linux x86_64 с systemd.

## 1. Переменные окружения
```bash
sudo install -d /opt/travel-ai
sudo install -m 0640 .env.example /opt/travel-ai/.env
sudoedit /opt/travel-ai/.env
```

Заполните один из путей LLM:

- **T-Bank Spirit LLM Proxy (приоритет):** `LLM_PROXY_TOKEN`, `LLM_PROXY_URL`
  (`https://llm-proxy.spirit.t-education.com/v1`), `LLM_PROXY_MODEL`
  (`openrouter/deepseek/deepseek-v4.1-flash`).
- **OpenRouter:** `OPENROUTER_API_KEY` и `OPENROUTER_MODEL`.

Без ключа поставьте `MOCK_LLM=1` — приложение работает офлайн. Для публичного
демо задайте `APP_TOKEN`, `DAILY_BUDGET_USD` и `DAILY_TOKEN_BUDGET`; `APP_TOKEN`
передаётся в `x-app-token` или `Authorization: Bearer`.
Для systemd укажите `DATABASE_URL=sqlite:///var/lib/travel-ai/app.db` (каталог
создаёт `install.sh`); `PRICE_IN_PER_M` и `PRICE_OUT_PER_M` — цена за 1M токенов,
`0` = неизвестна. Пути `PROMPTS_DIR`, `DATA_DIR` и `WEB_DIR` относительны `/opt/travel-ai`;
`BIND` по умолчанию — `0.0.0.0:8080`.

**Redis (опционально):** установите `redis-server`, задайте
`REDIS_URL=redis://127.0.0.1:6379/0` и не открывайте 6379 наружу. Redis хранит
только кэш результатов тулов (TTL 10 минут); SQLite остаётся источником истины.
Если `REDIS_URL` пуст или Redis недоступен, приложение использует in-memory кэш.

## 2. Сборка
```bash
cargo build --release
npm --prefix src/frontend run build
```

Нужны `target/release/travel-ai`, `src/prompts`, `data`, `migrations`,
`src/frontend/dist`. Сборка — под Linux x86_64 с той же libc, что на сервере;
альтернатива: `docker build -f deploy/Dockerfile -t travel-ai .` — фронтенд
собирается внутри образа.

## 3. Установка
Запускайте из корня репозитория:

```bash
sudo bash deploy/install.sh
```

Скрипт создаёт пользователя `travel`, каталоги, копирует бинарник и ресурсы, ставит
unit и запускает сервис. `/opt/travel-ai/.env` обязателен. Health `/api/health`
проверяется с повторами до 30 секунд.

## 4. systemd и проверка
```bash
sudo systemctl status travel-ai
sudo systemctl restart travel-ai
sudo systemctl stop travel-ai
sudo journalctl -u travel-ai -f
curl -fsS http://127.0.0.1:8080/api/health
```

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

```bash
cp .env.example .env
# без ключа можно оставить OPENROUTER_API_KEY пустым — включится mock LLM
docker compose up --build -d   # нужен compose plugin; либо docker build/run ниже
docker compose logs -f travel-ai
curl -fsS http://127.0.0.1:8080/api/health
```

Образ собирается из `deploy/Dockerfile` (multi-stage: Rust builder → debian slim).
SQLite лежит в named volume `travel-ai-data` (`/var/lib/travel-ai`), поэтому
`docker compose down` не удаляет базу. Полный сброс — `docker compose down -v`.
Порт задан в `docker-compose.yml` (`8080:8080`), healthcheck ходит в `/api/health`.

Если compose-плагина нет:

```bash
docker build -f deploy/Dockerfile -t travel-ai:local .
docker run -d --name travel-ai --env-file .env -p 8080:8080 \
  -v travel-ai-data:/var/lib/travel-ai travel-ai:local
```

Обновление:

```bash
docker compose build --pull
docker compose up -d
```

## 8. GitHub Actions и GHCR

`.github/workflows/ci.yml` на каждый push/PR в `main`:

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo build --release`.
2. Docker build; на push в `main` образ публикуется в GHCR:
   `ghcr.io/mikhaelcat/vibecode:latest`, плюс теги `sha-<commit>` и ветки.

Права: `contents: read`, `packages: write`; используется встроенный
`GITHUB_TOKEN`, отдельный секрет не нужен. Для приватного пакета добавьте
`docker login ghcr.io -u <user>` с PAT `read:packages` на машине деплоя.

Забрать опубликованный образ на VM:

```bash
docker pull ghcr.io/mikhaelcat/vibecode:latest
docker run -d --name travel-ai \
  --env-file .env -p 8080:8080 \
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
