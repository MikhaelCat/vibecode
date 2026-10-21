#!/usr/bin/env bash
# Install or update travel-ai as a systemd service.
# Run from the repository root after building: sudo bash deploy/install.sh

set -Eeuo pipefail

APP_USER="travel"
APP_DIR="/opt/travel-ai"
STATE_DIR="/var/lib/travel-ai"
SERVICE_NAME="travel-ai"
UNIT_SRC="deploy/travel-ai.service"
UNIT_DST="/etc/systemd/system/${SERVICE_NAME}.service"
HEALTH_URL="${HEALTH_URL:-http://127.0.0.1:8080/api/health}"

log() { printf '==> %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ "${EUID}" -eq 0 ]] || die "run as root: sudo bash deploy/install.sh"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd -P)"
cd -- "${REPO_ROOT}"

[[ -f Cargo.toml && -d src ]] || die "repository root not found at ${REPO_ROOT}"
[[ -f "${UNIT_SRC}" ]] || die "missing systemd unit: ${UNIT_SRC}"
[[ -f target/release/travel-ai ]] || die "missing target/release/travel-ai; run 'cargo build --release' first"
for required in src/prompts data migrations; do
    [[ -d "${required}" ]] || die "missing required directory: ${required}"
done
command -v systemctl >/dev/null 2>&1 || die "systemctl not found; systemd is required"
command -v curl >/dev/null 2>&1 || die "curl not found; install curl first"

if [[ ! -f "${APP_DIR}/.env" ]]; then
    cat >&2 <<EOF
ERROR: ${APP_DIR}/.env is required.

Prepare the configuration before installing:
  sudo install -d ${APP_DIR}
  sudo install -m 0640 .env.example ${APP_DIR}/.env
  sudoedit ${APP_DIR}/.env

Fill in OPENROUTER_API_KEY and OPENROUTER_MODEL, then re-run:
  sudo bash deploy/install.sh
EOF
    exit 1
fi

if ! getent group "${APP_USER}" >/dev/null 2>&1; then
    log "Creating system group ${APP_USER}"
    groupadd --system "${APP_USER}"
fi
if ! id -u "${APP_USER}" >/dev/null 2>&1; then
    log "Creating system user ${APP_USER}"
    useradd --system --gid "${APP_USER}" --home-dir "${STATE_DIR}" \
        --shell /usr/sbin/nologin "${APP_USER}"
fi

log "Preparing ${APP_DIR} and ${STATE_DIR}"
install -d -o "${APP_USER}" -g "${APP_USER}" -m 0755 \
    "${APP_DIR}" "${APP_DIR}/src" "${APP_DIR}/src/frontend"
install -d -o "${APP_USER}" -g "${APP_USER}" -m 0750 "${STATE_DIR}"

if [[ -x "${APP_DIR}/travel-ai" ]]; then
    cp -a -- "${APP_DIR}/travel-ai" "${APP_DIR}/travel-ai.previous"
    log "Kept previous binary as ${APP_DIR}/travel-ai.previous"
fi

log "Installing binary"
install -m 0755 -o "${APP_USER}" -g "${APP_USER}" \
    target/release/travel-ai "${APP_DIR}/travel-ai"

copy_tree() {
    local src="$1"
    local dst="$2"
    install -d -o "${APP_USER}" -g "${APP_USER}" -m 0755 "${dst}"
    cp -a -- "${src}/." "${dst}/"
}

log "Copying prompts, data and migrations"
copy_tree src/prompts "${APP_DIR}/src/prompts"
copy_tree data "${APP_DIR}/data"
copy_tree migrations "${APP_DIR}/migrations"

if [[ -d src/frontend/dist ]]; then
    log "Copying frontend build"
    install -d -o "${APP_USER}" -g "${APP_USER}" -m 0755 "${APP_DIR}/src/frontend/dist"
    cp -a -- src/frontend/dist/. "${APP_DIR}/src/frontend/dist/"
else
    log "WARNING: src/frontend/dist not found; the API will start without a static UI"
fi

chown -R "${APP_USER}:${APP_USER}" "${APP_DIR}" "${STATE_DIR}"
# systemd reads EnvironmentFile as root; keep secrets out of the service's write scope.
chown root:"${APP_USER}" "${APP_DIR}/.env"
chmod 0640 "${APP_DIR}/.env"

log "Installing systemd unit"
install -m 0644 -o root -g root "${UNIT_SRC}" "${UNIT_DST}"
systemctl daemon-reload
systemctl enable --now "${SERVICE_NAME}"
systemctl restart "${SERVICE_NAME}"

log "Waiting for ${HEALTH_URL} (up to 30s)"
for _ in {1..30}; do
    if health_body="$(curl -fsS --max-time 2 "${HEALTH_URL}")"; then
        log "Service is healthy"
        printf '%s\n' "${health_body}"
        exit 0
    fi
    sleep 1
done

systemctl --no-pager --full status "${SERVICE_NAME}" || true
journalctl -u "${SERVICE_NAME}" -n 50 --no-pager || true
die "service did not pass the health check at ${HEALTH_URL}"
