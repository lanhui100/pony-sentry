#!/usr/bin/env bash
# deploy-console.sh —— PonySentry 生产控制台部署（构建期烘焙 ADMIN_TOKEN）
#
# 用法: bash deploy/scripts/deploy-console.sh
# 幂等可重复执行；token 轮换后重跑一次即整体更新。
# 设计约束：
#   - ADMIN_TOKEN 从 /etc/pony-sentry/pony-sentry.env 读取，不落库、不入 git；
#   - token 经 VITE_PONY_SENTRY_TOKEN 烘焙进 web 产物，再经 include_str! 内嵌进
#     服务端二进制（README 方式 A）——浏览器无需 localStorage；
#   - 构建完成后无条件把工作区 web/dist 还原为无 token 版本（EXIT trap），
#     任一失败路径都不把 token 产物留在 git 工作区；
#   - 安装前备份上一版本，服务/健康/鉴权读接口任一自检失败即自动回滚；
#   - 内嵌 token 的二进制只允许 root 和 service group 读取（默认 0750）；
#   - 任一步失败即非零退出（set -Eeuo pipefail）。
set -Eeuo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENV_FILE="${PONY_SENTRY_ENV_FILE:-/etc/pony-sentry/pony-sentry.env}"
BIN_SRC="$REPO_DIR/target/release/pony-sentry-server"
BIN_DEST="/usr/local/bin/pony-sentry-server"
SERVICE="pony-sentry"
BASE_URL="http://127.0.0.1:3000"

SUDO=""
if command -v sudo >/dev/null 2>&1 && [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

SERVICE_USER="${PONY_SENTRY_SERVICE_USER:-$($SUDO systemctl show -p User --value "$SERVICE" 2>/dev/null || true)}"
SERVICE_USER="${SERVICE_USER:-dm}"
SERVICE_GROUP="${PONY_SENTRY_SERVICE_GROUP:-$($SUDO systemctl show -p Group --value "$SERVICE" 2>/dev/null || true)}"
if [ -z "$SERVICE_GROUP" ]; then
  SERVICE_GROUP="$(id -gn "$SERVICE_USER" 2>/dev/null || echo dm)"
fi
BIN_MODE="${PONY_SENTRY_BIN_MODE:-0750}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pony-sentry-deploy.XXXXXX")"
BIN_BACKUP="$TMP_DIR/pony-sentry-server.previous"
BIN_STAGE="$BIN_DEST.new"
INSTALL_STARTED=0
DEPLOY_OK=0

wait_for_service() {
  local deadline=$((SECONDS + 15))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if $SUDO systemctl is-active --quiet "$SERVICE"; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

restore_worktree_dist() {
  if [ ! -d "$REPO_DIR/.git" ]; then
    echo "WARN: $REPO_DIR 不是 git 工作区，跳过 web/dist 还原检查" >&2
    return 0
  fi
  if git -C "$REPO_DIR" diff --quiet -- web/dist; then
    return 0
  fi
  if git -C "$REPO_DIR" checkout -- web/dist; then
    echo "已还原工作区 web/dist 为无 token 版本。"
    return 0
  fi
  echo "ERROR: 无法还原工作区 web/dist，请立即手工执行 git checkout -- web/dist" >&2
  return 1
}

rollback_previous_binary() {
  if [ ! -f "$BIN_BACKUP" ]; then
    echo "ERROR: 无可回滚的上一版本二进制" >&2
    return 1
  fi
  echo "==> 回滚上一版本二进制并重启服务" >&2
  $SUDO install -m "$BIN_MODE" -o root -g "$SERVICE_GROUP" "$BIN_BACKUP" "$BIN_DEST" || return 1
  $SUDO systemctl restart "$SERVICE" || return 1
  if ! wait_for_service; then
    echo "ERROR: 回滚后服务仍未存活" >&2
    return 1
  fi
  echo "回滚完成：服务已恢复为上一版本。" >&2
  return 0
}

cleanup() {
  local rc=$?
  trap - EXIT
  if [ "$rc" -ne 0 ] && [ "$INSTALL_STARTED" -eq 1 ] && [ "$DEPLOY_OK" -ne 1 ]; then
    if ! rollback_previous_binary; then
      echo "CRITICAL: 自动回滚失败，请立即人工处理" >&2
      rc=1
    fi
  fi
  if ! restore_worktree_dist; then
    rc=1
  fi
  if [ -e "$BIN_STAGE" ]; then
    $SUDO rm -f "$BIN_STAGE" || true
  fi
  $SUDO rm -rf "$TMP_DIR" || true
  exit "$rc"
}
trap cleanup EXIT

check_http_status() {
  local expected="$1"
  local url="$2"
  shift 2
  local code
  code="$(curl -sS -o /dev/null -w '%{http_code}' -m 5 "$@" "$url")" || return 1
  if [ "$code" != "$expected" ]; then
    echo "ERROR: $url -> HTTP $code（期待 $expected）" >&2
    return 1
  fi
}

# ---- 0. 读取 ADMIN_TOKEN（权限 600 root，经 sudo） ----
if ! $SUDO test -r "$ENV_FILE"; then
  echo "ERROR: 读取不到 $ENV_FILE" >&2
  exit 1
fi
ADMIN_TOKEN="$($SUDO sed -n 's/^ADMIN_TOKEN=//p' "$ENV_FILE" | tr -d '\r' | head -1)"
if [ -z "$ADMIN_TOKEN" ]; then
  echo "ERROR: $ENV_FILE 未配置 ADMIN_TOKEN" >&2
  exit 1
fi

echo "==> 1/6 构建 web（烘焙 VITE_PONY_SENTRY_TOKEN）"
( cd "$REPO_DIR/web" && VITE_PONY_SENTRY_TOKEN="$ADMIN_TOKEN" pnpm build )

echo "==> 2/6 编译 release 二进制（include_str! 内嵌产物）"
( cd "$REPO_DIR" && cargo build --release -p pony-sentry-server )
test -f "$BIN_SRC" || { echo "ERROR: 二进制未产出 $BIN_SRC" >&2; exit 1; }

echo "==> 3/6 备份上一版本并安装（权限 $BIN_MODE root:$SERVICE_GROUP）"
if $SUDO test -f "$BIN_DEST"; then
  $SUDO cp -a "$BIN_DEST" "$BIN_BACKUP"
else
  echo "WARN: $BIN_DEST 不存在，本次为首次安装，无自动回滚备份" >&2
fi
$SUDO install -m "$BIN_MODE" -o root -g "$SERVICE_GROUP" "$BIN_SRC" "$BIN_STAGE"
$SUDO mv -f "$BIN_STAGE" "$BIN_DEST"
INSTALL_STARTED=1
$SUDO systemctl restart "$SERVICE"
wait_for_service || { echo "ERROR: 服务未存活" >&2; exit 1; }

echo "==> 4/6 自检：健康、鉴权读接口、内嵌 token、二进制权限"
HEALTH="$(curl -fsS -m 5 --retry 5 --retry-connrefused --retry-delay 1 "$BASE_URL/healthz")"
echo "healthz: $HEALTH"
echo "$HEALTH" | grep -q '"db":"up"' || { echo "ERROR: DB 不健康" >&2; exit 1; }

for path in '/api/v1/issues?' '/api/v1/projects' '/api/v1/traces?limit=100'; do
  check_http_status 200 "$BASE_URL$path" -H "x-admin-token: $ADMIN_TOKEN" \
    || { echo "ERROR: 读接口自检失败 $path" >&2; exit 1; }
done

curl -fsS -m 5 "$BASE_URL/" | grep -qF "$ADMIN_TOKEN" \
  || { echo "ERROR: 产物未内嵌 token（构建期烘焙失效）" >&2; exit 1; }

ACTUAL_MODE="$($SUDO stat -c '%a' "$BIN_DEST")"
if [ "$ACTUAL_MODE" != "${BIN_MODE#0}" ] && [ "$ACTUAL_MODE" != "$BIN_MODE" ]; then
  echo "ERROR: 二进制权限异常：$ACTUAL_MODE（期待 $BIN_MODE）" >&2
  exit 1
fi
if [ -n "$($SUDO find "$BIN_DEST" -perm /004 -print -quit)" ]; then
  echo "ERROR: 二进制对 other 可读，内嵌 token 可能泄漏" >&2
  exit 1
fi

echo "==> 5/6 部署自检通过"
DEPLOY_OK=1
echo "==> 6/6 还原工作区 web/dist 为无 token 版本"
echo "部署完成：控制台令牌已随二进制内嵌（无 localStorage 依赖），读接口冒烟 200，失败可自动回滚。"
