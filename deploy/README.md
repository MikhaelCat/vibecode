# Деплой travel-ai

Один release-бинарник под systemd. Приложение: `/opt/travel-ai`, состояние:
`/var/lib/travel-ai`, unit: `travel-ai.service`. Нужен Linux x86_64 с systemd.

## 1. Переменные окружения
```bash
sudo install -d /opt/travel-ai
sudo install -m 0640 .env.example /opt/travel-ai/.env
sudoedit /opt/travel-ai/.env
```

Заполните `OPENROUTER_API_KEY` и `OPENROUTER_MODEL`; без ключа ставьте `MOCK_LLM=1`.
`DATABASE_URL` указывает на `/var/lib/travel-ai/app.db`; `PRICE_IN_PER_M` и
`PRICE_OUT_PER_M` — цена за 1M токенов, `0` = неизвестна. Пути `PROMPTS_DIR`, `DATA_DIR` и `WEB_DIR` относительны `/opt/travel-ai`.

## 2. Сборка
```bash
cargo build --release
pnpm --dir src/frontend install
pnpm --dir src/frontend build
```

Нужны `target/release/travel-ai`, `src/prompts`, `data`, `migrations` и собранный
`src/frontend/dist`. Сборка — под Linux x86_64 с той же libc, что на сервере;
альтернатива: `docker build -f deploy/Dockerfile -t travel-ai .` (статику соберите заранее).

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
Сервис слушает `0.0.0.0:8080`. Откройте порт в firewall VM и облака:

```bash
sudo ufw allow 8080/tcp
```

Не публикуйте SQLite. При reverse proxy отключите буферизацию SSE:
`proxy_buffering off;`, `proxy_cache off;`, `proxy_read_timeout 3600s;`, иначе
события придут пачками. Проверяйте через `curl -N`.

## 7. Backup SQLite
```bash
sudo install -d /var/backups/travel-ai
sudo sqlite3 /var/lib/travel-ai/app.db \
  ".backup '/var/backups/travel-ai/app-$(date +%F).db'"
```

Для восстановления остановите сервис, положите backup в `/var/lib/travel-ai/app.db`,
удалите `app.db-wal` и `app.db-shm`, верните владельца `travel:travel`, запустите
сервис снова.
