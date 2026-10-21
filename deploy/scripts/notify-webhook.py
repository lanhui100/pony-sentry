#!/usr/bin/env python3
"""PonySentry → 企业微信通知 relay（ADR-003 §1 的轻量 dispatcher）。

PonySentry 仅在 issue 新建（issue.created）或 resolved 后回归（issue.regression）
时向 WEBHOOK_URL 投递一次；本进程把该 payload 转成企微文本并发出。

监听（仅回环，避免公网暴露）：
  POST /webhook   接收 PonySentry webhook
  GET  /health    健康探针（返回 {"status":"ok","sent":N,"failed":M}）

环境变量（/etc/pony-sentry/notify.env，600）：
  WX_WORK_EXTERNAL_API_URL  必填，企微 relay 地址
  WX_WORK_EXTERNAL_API_KEY  必填，X-API-KEY 鉴权
  WX_WORK_USER_ID / MY_USER_ID  可选，通知接收人（通用中继 payload 的 to_user）
  NOTIFY_PORT               可选，默认 3091（3090 在 devserver 已被占用）
  NOTIFY_PROJECTS           可选，逗号分隔的项目白名单；空=全部项目

失败只记日志、不重试（与 PonySentry webhook 的 5s 旁路语义一致）。
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.getenv("NOTIFY_PORT", "3091"))  # 3090 在 devserver 已被其它 node 服务占用
API_URL = os.getenv("WX_WORK_EXTERNAL_API_URL", "").strip()
API_KEY = os.getenv("WX_WORK_EXTERNAL_API_KEY", "").strip()
USER_ID = (os.getenv("WX_WORK_USER_ID") or os.getenv("MY_USER_ID") or "").strip()
PROJECTS = {p.strip() for p in os.getenv("NOTIFY_PROJECTS", "").split(",") if p.strip()}

SENT = 0
FAILED = 0


def log(msg: str) -> None:
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%S%z')} notify: {msg}", flush=True)


def build_content(payload: dict) -> tuple[str, str]:
    """返回 (content, severity_marker)。"""
    event_type = str(payload.get("event_type") or "issue.created")
    issue = payload.get("issue") or {}
    latest = payload.get("latest_event") or {}
    title = str(issue.get("title") or "unknown")
    env = str(issue.get("environment") or "unknown")
    project = str(issue.get("project") or "unknown")
    level = str(issue.get("level") or "error").upper()
    culprit = str(issue.get("culprit") or "-")
    count = issue.get("count")
    message = str(latest.get("message") or "").strip()
    marker = "🔁" if event_type == "issue.regression" else "🚨"
    lines = [
        f"{marker} PonySentry 缺陷 [{level}]",
        f"> 标题: {title[:200]}",
        f"> 项目: {project} / {env}",
        f"> 位置: {culprit[:200]}",
    ]
    if message:
        lines.append(f"> 消息: {message[:400]}")
    if count is not None:
        lines.append(f"> 累计次数: {count}")
    if event_type == "issue.regression":
        lines.append("> 状态: 已解决缺陷再次回归")
    lines.append(f"> 时间: {time.strftime('%Y-%m-%d %H:%M:%S%z')}")
    return "\n".join(lines), marker


def send_wecom(content: str) -> tuple[bool, str]:
    if not API_URL or not API_KEY:
        return False, "wecom_not_configured"
    body: dict[str, str] = {"content": content}
    if USER_ID:
        body["to_user"] = USER_ID
    req = urllib.request.Request(
        API_URL,
        data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json", "X-API-KEY": API_KEY},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            raw = resp.read(2048).decode("utf-8", "replace")
            status = resp.status
    except urllib.error.HTTPError as exc:
        return False, f"http_{exc.code}: {str(exc.reason)[:200]}"
    except (urllib.error.URLError, TimeoutError) as exc:
        return False, f"transport_error: {exc}"
    try:
        data = json.loads(raw) if raw else {}
    except json.JSONDecodeError:
        data = {}
    errcode = data.get("errcode") if isinstance(data, dict) else None
    ok = (isinstance(data, dict) and data.get("status") == "success") or errcode == 0
    if not ok:
        return False, f"http_{status}: {raw[:200]}"
    return True, "delivered"


class Handler(BaseHTTPRequestHandler):
    server_version = "PonySentryNotify/1.0"

    def log_message(self, fmt: str, *args) -> None:  # 抑制默认 access log，统一由 journal 记录
        return

    def _json(self, code: int, obj: dict) -> None:
        data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802 - stdlib 约定
        if self.path == "/health":
            self._json(200, {"status": "ok", "sent": SENT, "failed": FAILED})
            return
        self._json(404, {"error": "not_found"})

    def do_POST(self) -> None:  # noqa: N802 - stdlib 约定
        global SENT, FAILED
        if self.path != "/webhook":
            self._json(404, {"error": "not_found"})
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            self._json(400, {"error": "bad_length"})
            return
        if length <= 0 or length > 262144:
            self._json(400, {"error": "bad_length"})
            return
        raw = self.rfile.read(length)
        try:
            payload = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._json(400, {"error": "invalid_json"})
            return
        project = str((payload.get("issue") or {}).get("project") or "")
        if PROJECTS and project not in PROJECTS:
            log(f"skip project={project!r}（不在 NOTIFY_PROJECTS={sorted(PROJECTS)}）")
            self._json(200, {"result": "skipped", "reason": "project_filtered"})
            return
        content, _ = build_content(payload)
        ok, detail = send_wecom(content)
        if ok:
            SENT += 1
            log(f"sent project={project!r} title={str((payload.get('issue') or {}).get('title'))[:80]!r}")
            self._json(200, {"result": "sent"})
        else:
            FAILED += 1
            log(f"failed project={project!r} detail={detail}")
            self._json(502, {"result": "failed", "detail": detail})


def main() -> int:
    if not API_URL or not API_KEY:
        log("FATAL: WX_WORK_EXTERNAL_API_URL / WX_WORK_EXTERNAL_API_KEY 未配置，拒绝启动")
        return 2
    log(f"listening on 127.0.0.1:{PORT} projects={sorted(PROJECTS) or 'ALL'} relay={API_URL.split('?')[0]}")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
