#!/usr/bin/env bash
# deploy-console.sh —— PonySentry 生产控制台部署（构建期烘焙 ADMIN_TOKEN）
#
# 用法: bash deploy/scripts/deploy-console.sh
# 幂等可重复执行；token 轮换后重跑一次即整体更新。
# 设计约束：
#   - ADMIN_TOKEN 从 /etc/pony-sentry/pony-sentry.env 读取，不落库、不入 git；
#   - token 经 VITE_PONY_SENTRY_TOKEN 烘焙进 web 产物，再经 include_str! 内嵌进
#     服务端二进制（README 方式 A）——浏览器无需 localStorage；
#   - 构建完成后把工作区 web/dist 还原为无 token 版本，保持仓库卫生与 CI 密钥扫描；
#   - 任一步失败即非零退出（set -euo pipefail），自检不过不报成功。
set -euo pipefail

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

echo "==> 1/5 构建 web（烘焙 VITE_PONY_SENTRY_TOKEN）"
( cd "$REPO_DIR/web" && VITE_PONY_SENTRY_TOKEN="$ADMIN_TOKEN" pnpm build )

echo "==> 2/5 编译 release 二进制（include_str! 内嵌产物）"
( cd "$REPO_DIR" && cargo build --release -p pony-sentry-server )
test -f "$BIN_SRC" || { echo "ERROR: 二进制未产出 $BIN_SRC" >&2; exit 1; }

echo "==> 3/5 安装并重启服务"
$SUDO install -m 0755 -o root -g root "$BIN_SRC" "$BIN_DEST"
$SUDO systemctl restart "$SERVICE"
sleep 1
$SUDO systemctl is-active --quiet "$SERVICE" || { echo "ERROR: 服务未存活" >&2; exit 1; }

echo "==> 4/5 自检"
HEALTH="$(curl -fsS -m 5 "$BASE_URL/healthz")"
echo "healthz: $HEALTH"
echo "$HEALTH" | grep -q '"db":"up"' || { echo "ERROR: DB 不健康" >&2; exit 1; }

HTTP="$(curl -s -o /dev/null -w '%{http_code}' -m 5 -H "x-admin-token: $ADMIN_TOKEN" "$BASE_URL/api/v1/issues?")"
[ "$HTTP" = "200" ] || { echo "ERROR: 读接口自检 $HTTP（期待 200）" >&2; exit 1; }

curl -fsS -m 5 "$BASE_URL/" | grep -qF "$ADMIN_TOKEN" \
  || { echo "ERROR: 产物未内嵌 token（构建期烘焙失效）" >&2; exit 1; }

echo "==> 5/5 还原工作区 web/dist 为无 token 版本"
( cd "$REPO_DIR" && git checkout -- web/dist )

echo "部署完成：控制台令牌已随二进制内嵌（无 localStorage 依赖），读接口冒烟 200。"