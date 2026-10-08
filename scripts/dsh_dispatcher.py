#!/usr/bin/env python3
"""
PonySentry Webhook & DSH Dispatcher with WeChat Work Notification.
"""

import json
import logging
import os
import subprocess
import sys
from typing import Any, Dict, Optional
from urllib.parse import urlparse

import httpx

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")
logger = logging.getLogger("dsh_dispatcher")


def is_aliyun_notifier_api(api_url: str) -> bool:
    path = urlparse(api_url).path.rstrip("/")
    return path.endswith("/send_msg") or path == "/send_msg"


def build_wechat_payload(api_url: str, message: str, user_id: Optional[str]) -> Dict[str, Any]:
    if is_aliyun_notifier_api(api_url):
        return {
            "msg": message,
            "choice": "wx_work",
        }
    return {
        "content": message,
        "to_user": user_id,
    }


def send_wechat_work_notification(message: str, user_id: Optional[str] = None) -> bool:
    api_url = os.getenv("WX_WORK_EXTERNAL_API_URL")
    api_key = os.getenv("WX_WORK_EXTERNAL_API_KEY")
    default_user = os.getenv("WX_WORK_USER_ID") or os.getenv("MY_USER_ID")

    if not api_url or not api_key:
        logger.warning("WX_WORK_EXTERNAL_API_URL 或 WX_WORK_EXTERNAL_API_KEY 未配置，跳过企微通知。")
        return False

    target_user = user_id or default_user
    headers = {
        "X-API-KEY": api_key,
        "Content-Type": "application/json",
    }
    payload = build_wechat_payload(api_url, message, target_user)

    try:
        # 避免受环境中的 http_proxy 或 no_proxy 中 IPv6 特殊字符干扰
        with httpx.Client(timeout=10.0, trust_env=False) as client:
            resp = client.post(api_url, headers=headers, json=payload)
            data = resp.json()
            if is_aliyun_notifier_api(api_url):
                if resp.is_success and data.get("message") == "消息发送成功":
                    logger.info("企业微信通知发送成功 (Aliyun Notifier)")
                    return True
            elif data.get("status") == "success" or data.get("errcode") == 0:
                logger.info("企业微信通知发送成功 (Standard Gateway)")
                return True
            logger.warning(f"企业微信发送返回失败: {data}")
            return False
    except Exception as e:
        logger.error(f"企业微信发送异常: {e}")
        return False


def run_dsh_diagnosis(workspace_path: str, issue_info: Dict[str, Any]) -> Dict[str, Any]:
    title = issue_info.get("title", "Unknown Error")
    culprit = issue_info.get("culprit", "Unknown Culprit")
    stacktrace = issue_info.get("stacktrace", "")

    prompt = (
        f"【PonySentry 故障诊断任务】\n"
        f"发现错误: {title}\n"
        f"出错位置: {culprit}\n"
        f"堆栈信息:\n{stacktrace}\n\n"
        f"请基于 dev-team 流程对该错误进行根因诊断，并给出修复建议及测试用例草案。\n"
        f"注意：仅输出诊断分析报告与建议方案，绝对严禁直接修改或提交任何代码，等待用户在 Web 页面中审核确认。"
    )

    cmd = ["dsh", "headless", "--json", prompt]
    logger.info(f"在工作区 {workspace_path} 中启动 DSH 诊断任务...")

    session_id = None
    final_text = ""

    try:
        proc = subprocess.Popen(
            cmd,
            cwd=workspace_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert proc.stdout is not None
        for line in proc.stdout:
            line_str = line.strip()
            if not line_str:
                continue
            try:
                event = json.loads(line_str)
                event_type = event.get("type")
                if event_type == "session" and not session_id:
                    session_id = event.get("sessionId")
                    logger.info(f"捕获到 DSH Session ID: {session_id}")
                elif event_type == "final":
                    final_text = event.get("text", "")
            except json.JSONDecodeError:
                continue

        proc.wait()
        return {
            "session_id": session_id,
            "summary": final_text,
            "exit_code": proc.returncode,
        }
    except Exception as e:
        logger.error(f"DSH 调度失败: {e}")
        return {
            "session_id": None,
            "summary": f"执行失败: {e}",
            "exit_code": -1,
        }


def process_webhook_event(event_payload: Dict[str, Any], web_base_url: str = "http://127.0.0.1:3080") -> None:
    issue = event_payload.get("issue", {})
    latest_event = event_payload.get("latest_event", {})

    workspace_path = issue.get("project_path")
    if not workspace_path or not os.path.exists(workspace_path):
        logger.warning(f"工作区路径未指定或不存在 ({workspace_path})，尝试回退使用当前工作目录...")
        workspace_path = os.getcwd()

    issue_info = {
        "title": issue.get("title"),
        "culprit": issue.get("culprit"),
        "stacktrace": latest_event.get("stacktrace", ""),
    }

    result = run_dsh_diagnosis(workspace_path, issue_info)
    session_id = result.get("session_id")
    summary = result.get("summary", "无详细诊断内容")

    session_link = f"{web_base_url}/?session={session_id}" if session_id else "无法获取"

    msg = (
        f"🚨【PonySentry 故障诊断已就绪】\n"
        f"● 缺陷标题：{issue_info['title']}\n"
        f"● 出错位置：{issue_info['culprit']}\n"
        f"● 诊断会话：{session_link}\n"
        f"● 诊断摘要：\n{summary[:300]}...\n\n"
        f"⚠️ 修复待确认：请在 Web UI 打开上述会话审查方案，回复确认后方可执行修复。"
    )

    send_wechat_work_notification(msg)


def start_webhook_server(port: int = 9090) -> None:
    from http.server import HTTPServer, BaseHTTPRequestHandler

    class WebhookHandler(BaseHTTPRequestHandler):
        def do_POST(self):
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"received"}')

            try:
                payload = json.loads(body.decode("utf-8"))
                logger.info(f"收到 Webhook 投递: event_type={payload.get('event_type')}")
                # 在后台线程处理，避免阻塞 Webhook 回调
                import threading
                threading.Thread(target=process_webhook_event, args=(payload,), daemon=True).start()
            except Exception as e:
                logger.error(f"处理 Webhook 异常: {e}")

        def log_message(self, format, *args):
            return

    server = HTTPServer(("0.0.0.0", port), WebhookHandler)
    logger.info(f"PonySentry Webhook Dispatcher 监听在 0.0.0.0:{port}...")
    server.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test-notification":
        send_wechat_work_notification("PonySentry 测试企业微信连通性")
    elif len(sys.argv) > 1 and sys.argv[1] == "--serve":
        port = int(sys.argv[2]) if len(sys.argv) > 2 else 9090
        start_webhook_server(port)
