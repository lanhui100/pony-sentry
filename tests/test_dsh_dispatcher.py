import pytest
from unittest.mock import patch, MagicMock
from scripts.dsh_dispatcher import (
    is_aliyun_notifier_api,
    build_wechat_payload,
    send_wechat_work_notification,
    process_webhook_event,
)


def test_is_aliyun_notifier_api():
    assert is_aliyun_notifier_api("https://notify.example.com/api/v1/send_msg") is True
    assert is_aliyun_notifier_api("https://notify.example.com/send_msg/") is True
    assert is_aliyun_notifier_api("https://qyapi.weixin.qq.com/cgi-bin/webhook/send") is False


def test_build_wechat_payload():
    aliyun_payload = build_wechat_payload("http://test.com/send_msg", "hello", "user1")
    assert aliyun_payload == {"msg": "hello", "choice": "wx_work"}

    generic_payload = build_wechat_payload("http://test.com/api/notify", "hello", "user1")
    assert generic_payload == {"content": "hello", "to_user": "user1"}


@patch("scripts.dsh_dispatcher.httpx.Client")
def test_send_wechat_work_notification(mock_client, monkeypatch):
    monkeypatch.setenv("WX_WORK_EXTERNAL_API_URL", "https://notify.com/send_msg")
    monkeypatch.setenv("WX_WORK_EXTERNAL_API_KEY", "secret-key")
    monkeypatch.setenv("MY_USER_ID", "dm")

    mock_resp = MagicMock()
    mock_resp.is_success = True
    mock_resp.json.return_value = {"message": "消息发送成功"}
    mock_client.return_value.__enter__.return_value.post.return_value = mock_resp

    success = send_wechat_work_notification("测试报警")
    assert success is True


@patch("scripts.dsh_dispatcher.send_wechat_work_notification")
@patch("scripts.dsh_dispatcher.run_dsh_diagnosis")
def test_process_webhook_event(mock_run_dsh, mock_send_wechat, tmp_path):
    mock_run_dsh.return_value = {
        "session_id": "session-12345",
        "summary": "分析出除零错误，建议添加边界判断",
        "exit_code": 0,
    }

    event = {
        "issue": {
            "title": "ZeroDivisionError",
            "culprit": "main.py:10",
            "project_path": str(tmp_path),
        },
        "latest_event": {
            "stacktrace": "ZeroDivisionError: division by zero",
        },
    }

    process_webhook_event(event)

    mock_run_dsh.assert_called_once()
    mock_send_wechat.assert_called_once()
    msg = mock_send_wechat.call_args[0][0]
    assert "session-12345" in msg
    assert "ZeroDivisionError" in msg
    assert "修复待确认" in msg
