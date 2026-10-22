#!/usr/bin/env bash
# deploy-console.sh —— PonySentry 生产控制台部署（服务端短期会话）
#
# 用法: bash deploy/scripts/deploy-console.sh
# 幂等可重复执行；ADMIN_TOKEN 轮换后重跑一次即整体更新。
# 设计约束：
#   - ADMIN_TOKEN 从 /etc/pony-sentry/pony-sentry.env 读取，仅用于部署冒烟登录；
#     它不再进入 web 产物或服务端二进制；
#   - 构建完成后无条件把工作区 web/dist 还原为 committed 版本（EXIT trap）；
#   - 安装前备份上一版本，服务/健康/登录/会话读接口/CSRF 任一自检失败即自动回滚；
#   - 二进制只允许 root 和 service group 读取（默认 0750，纵深防御）；
#   - 任一步失败即非零退出（set -Eeuo pipefail）。
set -Eeuo pipefail
umask 077

if [ "$(id -u)" -eq 0 ]; then
  echo "ERROR: 请以普通部署用户运行本脚本（脚本内部会按需 sudo），避免 root 拥有构建产物。" >&2
  exit 1
fi

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
COOKIE_JAR="$TMP_DIR/session.cookies"
LOGIN_JSON="$TMP_DIR/login.json"
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

# 信任自检专用：外部调用硬超时 3s（NFR `external_call_timeout_ms=3000`），不复用上面的 5s 口径。
check_http_status_trust() {
  local expected="$1"
  local url="$2"
  shift 2
  local code
  code="$(curl -sS -o /dev/null -w '%{http_code}' -m 3 "$@" "$url")" || return 1
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

echo "==> 1/6 构建 web（不注入任何令牌）"
( cd "$REPO_DIR/web" && pnpm build )

echo "==> 2/6 编译 release 二进制（include_str! 内嵌产物）"
( cd "$REPO_DIR" && cargo build --release -p pony-sentry-server )
test -f "$BIN_SRC" || { echo "ERROR: 二进制未产出 $BIN_SRC" >&2; exit 1; }
chmod 0600 "$BIN_SRC"

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

echo "==> 4/6 自检：健康、匿名拒绝、短期会话登录、Cookie 读接口与 CSRF、产物无 token"
HEALTH="$(curl -fsS -m 5 --retry 5 --retry-connrefused --retry-delay 1 "$BASE_URL/healthz")"
echo "healthz: $HEALTH"
grep -q '"db":"up"' <<<"$HEALTH" || { echo "ERROR: DB 不健康" >&2; exit 1; }

# 管理读接口匿名必须 401（若服务端配置了 ADMIN_TOKEN / CLIENT_TOKEN）。
check_http_status 401 "$BASE_URL/api/v1/issues?" \
  || { echo "ERROR: 匿名读接口未按预期拒绝" >&2; exit 1; }

# ── Tailnet 信任双门自检（ADR 004 / 契约 R-T1..R-T13）─────────────────────
# 信任 = 开关 ∧ 对端网段 ∧ 非代理 ∧ Host 白名单，四者取与：
#   ① 负向：loopback 伪造可信 Host 仍必须 401（Host 门不能单独放行）；
#   ② 正向：从本机 tailnet 地址访问（对端落在可信网段且 Host 命中）匿名读必须 200。
TRUST_ENABLED="$($SUDO sed -n 's/^CONSOLE_TAILNET_TRUST=//p' "$ENV_FILE" | tr -d '\r' | head -1)"
TRUST_ENABLED_LC="$(printf '%s' "$TRUST_ENABLED" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
TRUSTED_HOSTS="$($SUDO sed -n 's/^CONSOLE_TRUSTED_HOSTS=//p' "$ENV_FILE" | tr -d '\r' | head -1)"
TAILNET_IP="$(timeout 3 tailscale ip -4 2>/dev/null | head -1 || true)"

case "$TRUST_ENABLED_LC" in
  1|true|on)
    if [ -z "$TRUSTED_HOSTS" ]; then
      echo "WARN: CONSOLE_TAILNET_TRUST 已开启但 CONSOLE_TRUSTED_HOSTS 为空 → 信任不生效（启动应有 WARN）" >&2
    else
      FIRST_HOST="$(printf '%s' "${TRUSTED_HOSTS%%,*}" | tr -d '[:space:]')"
      # ① 负向：loopback 不在可信网段，伪造 Host 也必须 401。
      check_http_status_trust 401 "$BASE_URL/api/v1/issues?" -H "Host: $FIRST_HOST" \
        || { echo "ERROR: loopback + 伪造可信 Host 未被拒绝（信任双门失效）" >&2; exit 1; }
      # ② 正向：从本机 tailnet 地址访问（对端落在可信网段），Host 取白名单首项，
      #    因此 MagicDNS-only 的白名单配置同样可验证。
      if [ -n "$TAILNET_IP" ]; then
        check_http_status_trust 200 "http://$TAILNET_IP:3000/api/v1/issues?" -H "Host: $FIRST_HOST" \
          || { echo "ERROR: tailnet 对端匿名读未放行（信任未生效）" >&2; exit 1; }
        echo "tailnet trust: 双门自检通过（loopback+伪造 Host → 401 / $TAILNET_IP+白名单 Host 匿名读 → 200）"
      else
        echo "tailnet trust: 负向自检通过；正向跳过（未取到本机 tailnet IPv4）"
      fi
    fi
    ;;
  *)
    echo "tailnet trust: 未开启（CONSOLE_TAILNET_TRUST=${TRUST_ENABLED:-<unset>}），控制台仍走 token/会话登录"
    ;;
esac

# 静态 ADMIN_TOKEN 仅用于交换短期会话；token 经 stdin 传递，不进入 curl 命令行。
LOGIN_CODE="$(printf '%s' "$ADMIN_TOKEN"   | python3 -c 'import json,sys; print(json.dumps({"token": sys.stdin.read()}))'   | curl -sS -o "$LOGIN_JSON" -c "$COOKIE_JAR" -w '%{http_code}' -m 5 \
      -X POST -H 'Content-Type: application/json' --data-binary @- \
      "$BASE_URL/api/v1/auth/session")"
[ "$LOGIN_CODE" = "200" ] || { echo "ERROR: 会话登录自检 HTTP $LOGIN_CODE（期待 200）" >&2; exit 1; }
python3 - "$LOGIN_JSON" <<'PYLOGIN'
import json, re, sys
body = json.load(open(sys.argv[1], encoding="utf-8"))
assert body.get("authenticated") is True, "session login response missing authenticated=true"
assert re.fullmatch(r"[0-9a-f]{32}", body.get("csrf_token", "")), "invalid csrf_token"
assert isinstance(body.get("expires_in"), int) and body["expires_in"] >= 60, "invalid expires_in"
PYLOGIN

check_http_status 200 "$BASE_URL/api/v1/auth/session" -b "$COOKIE_JAR" \
  || { echo "ERROR: 会话状态接口自检失败" >&2; exit 1; }
for path in '/api/v1/issues?' '/api/v1/projects' '/api/v1/traces?limit=100'; do
  check_http_status 200 "$BASE_URL$path" -b "$COOKIE_JAR" \
    || { echo "ERROR: Cookie 会话读接口自检失败 $path" >&2; exit 1; }
done

# Cookie 会话执行 PATCH 但缺 CSRF 头必须 403；请求在中间件层被拒绝，不会触达业务。
check_http_status 403 "$BASE_URL/api/v1/issues/00000000-0000-0000-0000-000000000000" \
  -b "$COOKIE_JAR" -X PATCH -H 'Content-Type: application/json' \
  --data '{"status":"resolved"}' \
  || { echo "ERROR: Cookie 会话 PATCH 未强制 CSRF" >&2; exit 1; }

# 产物必须不包含 ADMIN_TOKEN；登录页与诊断文案应存在。
ROOT_HTML="$(curl -fsS -m 5 "$BASE_URL/")"
if grep -qF -- "$ADMIN_TOKEN" <<<"$ROOT_HTML"; then
  echo "ERROR: 控制台产物仍包含 ADMIN_TOKEN" >&2
  exit 1
fi
grep -qF -- '控制台登录' <<<"$ROOT_HTML" \
  || { echo "ERROR: 控制台产物缺少登录入口" >&2; exit 1; }

# 注销后旧 Cookie 必须立即失效。
check_http_status 204 "$BASE_URL/api/v1/auth/session" -b "$COOKIE_JAR" -X DELETE \
  || { echo "ERROR: 会话注销自检失败" >&2; exit 1; }
check_http_status 401 "$BASE_URL/api/v1/issues?" -b "$COOKIE_JAR" \
  || { echo "ERROR: 注销后旧 Cookie 仍可读" >&2; exit 1; }

ACTUAL_MODE="$($SUDO stat -c '%a' "$BIN_DEST")"
if [ "$ACTUAL_MODE" != "${BIN_MODE#0}" ] && [ "$ACTUAL_MODE" != "$BIN_MODE" ]; then
  echo "ERROR: 二进制权限异常：$ACTUAL_MODE（期待 $BIN_MODE）" >&2
  exit 1
fi
if [ -n "$($SUDO find "$BIN_DEST" -perm /004 -print -quit)" ]; then
  echo "ERROR: 二进制对 other 可读" >&2
  exit 1
fi

echo "==> 5/6 部署自检通过"
DEPLOY_OK=1
echo "==> 6/6 还原工作区 web/dist 为 committed 版本"
echo "部署完成：控制台使用服务端短期会话（HttpOnly Cookie + CSRF），静态 ADMIN_TOKEN 不进入产物，失败可自动回滚。"
