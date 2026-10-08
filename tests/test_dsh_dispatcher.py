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
    assert str(tmp_path) in msg


@patch("scripts.dsh_dispatcher.send_wechat_work_notification")
@patch("scripts.dsh_dispatcher.run_dsh_diagnosis")
def test_process_webhook_event_matches_project_candidate(mock_run_dsh, mock_send_wechat, tmp_path, monkeypatch):
    mock_run_dsh.return_value = {
        "session_id": "session-proj-001",
        "summary": "诊断成功",
        "exit_code": 0,
    }
    mock_send_wechat.return_value = True

    # 模拟 ~ 下有 ponyllm 目录
    fake_home = tmp_path / "fake_home"
    fake_proj = fake_home / "ponyllm"
    fake_proj.mkdir(parents=True)
    monkeypatch.setenv("HOME", str(fake_home))

    event = {
        "issue": {
            "title": "GatewayExhaustedError",
            "culprit": None,
            "project": "ponyllm",
            # 没有 project_path
        },
        "latest_event": {
            "stacktrace": "upstream 403",
        },
    }

    process_webhook_event(event)

    mock_run_dsh.assert_called_once()
    called_ws, called_info = mock_run_dsh.call_args[0]
    assert called_ws == str(fake_proj)
    assert called_info["workspace"] == str(fake_proj)
    assert called_info["project"] == "ponyllm"


@patch("scripts.dsh_dispatcher.send_wechat_work_notification")
@patch("scripts.dsh_dispatcher.run_dsh_diagnosis")
def test_process_webhook_event_infers_ponyllm_from_gateway_exhausted_error(mock_run_dsh, mock_send_wechat, tmp_path, monkeypatch):
    mock_run_dsh.return_value = {
        "session_id": "session-gw-001",
        "summary": "诊断成功",
        "exit_code": 0,
    }
    mock_send_wechat.return_value = True

    fake_home = tmp_path / "fake_home"
    fake_proj = fake_home / "ponyllm"
    fake_proj.mkdir(parents=True)
    monkeypatch.setenv("HOME", str(fake_home))

    # 没有 project，也没有 project_path，但 title 为 GatewayExhaustedError
    event = {
        "issue": {
            "title": "GatewayExhaustedError: Chat completions failed",
            "culprit": None,
        },
        "latest_event": {
            "stacktrace": "upstream 503",
        },
    }

    process_webhook_event(event)

    mock_run_dsh.assert_called_once()
    called_ws, called_info = mock_run_dsh.call_args[0]
    assert called_ws == str(fake_proj)
    assert called_info["workspace"] == str(fake_proj)
    assert called_info["project"] == "ponyllm"


@patch("scripts.dsh_dispatcher.subprocess.Popen")
@patch("scripts.dsh_dispatcher.attach_session_to_workspace")
def test_run_dsh_diagnosis_prompt_contains_workspace(mock_attach, mock_popen):
    from scripts.dsh_dispatcher import run_dsh_diagnosis

    mock_proc = MagicMock()
    mock_proc.stdout = ['{"type":"session","sessionId":"sess-123"}\n', '{"type":"final","text":"done"}\n']
    mock_proc.returncode = 0
    mock_proc.wait.return_value = 0
    mock_popen.return_value = mock_proc

    issue_info = {
        "title": "ReleaseProbe: post-deploy check",
        "culprit": "src/verify.rs in probe",
        "stacktrace": "test stack",
        "workspace": "/home/dm/job_copilot",
        "project": "job_copilot",
    }
    run_dsh_diagnosis("/home/dm/job_copilot", issue_info)

    called_cmd = mock_popen.call_args[0][0]
    prompt_arg = called_cmd[3]
    assert "工作区路径: /home/dm/job_copilot" in prompt_arg
    assert "所属项目: job_copilot" in prompt_arg


@patch("scripts.dsh_dispatcher.send_wechat_work_notification")
@patch("scripts.dsh_dispatcher.run_dsh_diagnosis")
def test_process_webhook_event_interrupted_auto_recovery(mock_run_dsh, mock_send_wechat, tmp_path):
    # 模拟第一次执行因工具中断或超时导致退出码异常 (exit_code=-1 或无 summary)，随后触发一次自动续跑恢复
    mock_run_dsh.side_effect = [
        {
            "session_id": "session-interrupted-001",
            "summary": "",
            "exit_code": -1,
        },
        {
            "session_id": "session-interrupted-001",
            "summary": "自愈成功：继续执行后定位到空指针错误",
            "exit_code": 0,
        },
    ]

    event = {
        "issue": {
            "title": "NullPointerPanic",
            "culprit": "handler.rs:42",
            "project_path": str(tmp_path),
        },
        "latest_event": {
            "stacktrace": "panic: unexpected null pointer",
        },
    }

    process_webhook_event(event)

    # 验证触发了 2 次 run_dsh_diagnosis 调用（初次启动 + 续跑恢复）
    assert mock_run_dsh.call_count == 2
    retry_call = mock_run_dsh.call_args_list[1]
    assert retry_call.kwargs.get("resume_session_id") == "session-interrupted-001"

    # 验证最终通知携带了自愈成功后的诊断摘要
    mock_send_wechat.assert_called_once()
    msg = mock_send_wechat.call_args[0][0]
    assert "session-interrupted-001" in msg
    assert "自愈成功" in msg
