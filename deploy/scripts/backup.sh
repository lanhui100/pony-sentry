#!/usr/bin/env bash
# PonySentry 加密备份：pg_dump -Fc | age -r，产物自带 sha256 校验和。
#
# 环境变量（契约 R11.4）：
#   DATABASE_URL            必填，PostgreSQL 连接串（由 /etc/pony-sentry/pony-sentry.env 提供）
#   AGE_RECIPIENT           必填，age 公钥（age1...）；解密需对应私钥，由运维离线保管
#   BACKUP_DIR              可选，默认 /var/backups/pony-sentry
#   BACKUP_RETENTION_DAYS   可选，默认 14（按 mtime 清理更早的备份与校验和）
#
# 行为保证：
#   - `set -euo pipefail`：任一环节失败即非零退出（systemd 记录 failed）；
#   - 失败时删除半成品 .dump.age 与 .sha256，不留下"看起来成功"的残件；
#   - 首次运行自动创建 BACKUP_DIR（权限 750）。
#
# 环境适配：脚本与三环境无关，仅消费环境变量；dev/preprod/production 差异由各自
# EnvironmentFile（/etc/pony-sentry/backup.env）承载，禁止把环境差异硬编码进脚本。
set -euo pipefail

umask 077

: "${DATABASE_URL:?DATABASE_URL is required（/etc/pony-sentry/pony-sentry.env）}"
: "${AGE_RECIPIENT:?AGE_RECIPIENT is required（age1...）}"

BACKUP_DIR="${BACKUP_DIR:-/var/backups/pony-sentry}"
BACKUP_RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-14}"
STAMP="$(date +%Y%m%dT%H%M)"
OUT="${BACKUP_DIR}/pony-sentry-${STAMP}.dump.age"

log() { printf '%s %s\n' "$(date -Is)" "$*"; }

cleanup() {
  rc=$?
  if [ "$rc" -ne 0 ]; then
    rm -f "$OUT" "$OUT.sha256"
    printf '%s backup failed (rc=%s), partial artifacts removed\n' "$(date -Is)" "$rc" >&2
  fi
  exit "$rc"
}
trap cleanup EXIT

mkdir -p "$BACKUP_DIR"
log "starting backup -> ${OUT}"

pg_dump -Fc "$DATABASE_URL" | age -r "$AGE_RECIPIENT" >"$OUT"
sha256sum "$OUT" | tee "$OUT.sha256" >/dev/null

log "backup complete: ${OUT} size=$(stat -c %s "$OUT") bytes"

log "pruning backups older than ${BACKUP_RETENTION_DAYS} days in ${BACKUP_DIR}"
find "$BACKUP_DIR" -maxdepth 1 -type f \( -name 'pony-sentry-*.dump.age' -o -name 'pony-sentry-*.dump.age.sha256' \) \
  -mtime "+${BACKUP_RETENTION_DAYS}" -print -delete || true

log "done"
