#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PonySentry Web 控制台 UI/UX 验收测试（真实浏览器 E2E）。

契约    : .dev-team/web-ui-contract.md（冻结版）§1-§7；逐条 REQ 机器断言 = 本文件一个 test_ 方法。
被测服务 : http://127.0.0.1:3000（旧版红相 / 新版绿相），由 agent-browser 驱动真实 Chromium。
分级判定 : L2（组件与集成：跨前端/网络链路）+ 真实前端 E2E 硬门禁：
           a) 浏览器 Console Error == 0；b) 每个关键旅程（Issues 列表 / 详情抽屉 / Traces 视图）
           至少落盘一张物理截图到 /tmp/pony-web-audit/。
测试编写 : 遵循 test-expert skill —— 零假测试（每条断言有真实语义与明确失败消息）、
           显式条件轮询（_wait_for，禁止盲 sleep）、物理收据（截图 + Console 日志）。
红相预期 : 旧版 UI 上 §8 列明的全部新契约条目 FAIL（本文件即为断言校准依据）。

运行方式（任选其一）：
  python3 tests/web/test_web_ui_acceptance.py -v
  python3 -m pytest tests/web/test_web_ui_acceptance.py -v   # 若环境装有 pytest

环境变量 : PONY_WEB_PHASE=red|green（截图文件名阶段标记，默认 red）
          PONY_WEB_SESSION=ponyt2（独立浏览器会话，默认 ponyt2，避免与其他 agent 共用浏览器）

契约条目 → 测试方法映射：
  §1.1 T1.1 gateway-status testid/状态三态    → test_10_t1_1_gateway_status_testid
  §1.2 T1.2 load-error 错误横幅              → test_11_t1_2_load_error_banner
  §1.3 T1.3 rel-time 行级元素               → test_12_t1_3_rel_time_on_rows
  §2.1 T2.1 Issues 行键盘可操作              → test_20_t2_1_issues_rows_keyboard
  §2.2 T2.2 Traces 卡片键盘可操作            → test_21_t2_2_traces_cards_keyboard
  §2.3 T2.3 icon-only 按钮/select aria-label  → test_22_t2_3_aria_labels
  §2.4 T2.4 抽屉 role=dialog/aria-modal       → test_23_t2_4_drawer_role_dialog
  §3   T3   行内边距 ≥8px（动态）            → test_30_t3_trace_row_padding
  §3   T3   静态门禁 p-4.5 废除              → test_31_t3_static_no_p45_class
  §4.1 T4.1 相对时间文本格式                 → test_40_t4_1_rel_time_text_relative
  §4.2 T4.2 title 绝对时间 ISO + 一致性      → test_41_t4_2_rel_time_title_iso
  §5.1 T5.1 Issues 抽屉背景点击关闭          → test_50_t5_1_issues_drawer_bg_click_closes
  §5.2 T5.2 Esc 全局兜底恰好关闭一次         → test_51_t5_2_esc_closes_drawer_exactly_once
  §5.3 T5.3 面板内点击不关闭                 → test_52_t5_3_panel_click_keeps_drawer_open
  §6.1 T6.1 加载后 3s 内 GET /healthz        → test_60_t6_1_healthz_requested_on_load
  §6.2 T6.2 data-state 与真实响应一致         → test_61_t6_2_health_badge_state_matches
  §6.3 T6.3 初始 checking 中间态             → test_62_t6_3_initial_state_checking
  §6.4 T6.4 轮询间隔常数 15000（源码级）     → test_63_t6_4_healthz_poll_interval
  §6.5 T6.5 /healthz abort ≤3000ms（源码级） → test_64_t6_5_healthz_abort_timeout
  §6.6 T6.6 卸载清理 timer/listener（源码级）→ test_65_t6_6_unmount_cleanup_source
  §7   NFR 基准 JSON 物理校验                → test_70_t7_nfr_baseline_json
  硬门禁 a) Console Error == 0               → test_90_console_errors_zero_across_journeys
  硬门禁 b) 关键旅程截图物理收据             → test_91_physical_screenshots_landed
  关键旅程（截图载体）                       → test_00_journey_issues_list /
                                               test_01_journey_issues_drawer /
                                               test_02_journey_traces_view
"""

import json
import os
import re
import subprocess
import time
import unittest
from datetime import datetime, timezone

# --------------------------------------------------------------------------
# 环境与常量
# --------------------------------------------------------------------------

BASE_URL = "http://127.0.0.1:3000"
SESSION = os.environ.get("PONY_WEB_SESSION", "ponyt2")
PHASE = os.environ.get("PONY_WEB_PHASE", "red")
SHOT_DIR = "/tmp/pony-web-audit"
PROJECT_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
)
WEB_SRC_DIR = os.path.join(PROJECT_ROOT, "web", "src")
NFR_JSON = os.path.join(PROJECT_ROOT, ".dev-team", "nfr-baseline.json")
AB = ["agent-browser", "--session", SESSION]

# 已落盘的关键旅程截图登记（journey 函数写入，物理收据测试读取）
SHOT_REGISTRY = {}

# 已知空态文案（契约 §1.2：加载失败文案必须能与之区分）
EMPTY_STATE_TEXTS = ("无匹配的异常缺陷", "暂无匹配的 Agent Trace 遥测记录")

# 抽屉可见判定（新旧版通用：旧版 .fixed.inset-0 遮罩 / 新版 role=dialog 面板）
DRAWER_OPEN_JS = (
    "JSON.stringify(!!(document.querySelector('.fixed.inset-0')"
    " || document.querySelector('[role=\"dialog\"]')))"
)
DRAWER_CLOSED_JS = (
    "JSON.stringify(!(document.querySelector('.fixed.inset-0')"
    " || document.querySelector('[role=\"dialog\"]')))"
)
GATEWAY_COUNT_JS = (
    "JSON.stringify(document.querySelectorAll('[data-testid=\"gateway-status\"]').length)"
)

ISSUE_ROWS_JS = """(() => {
  const rows = Array.from(document.querySelectorAll('tbody tr'))
    .filter(r => r.querySelector('td[colspan]') === null
             && ((r.textContent || '').indexOf('无匹配') === -1));
  return JSON.stringify(rows.length > 0);
})()"""

ISSUE_ROW_ATTRS_JS = """(() => {
  const rows = Array.from(document.querySelectorAll('tbody tr'))
    .filter(r => r.querySelector('td[colspan]') === null
             && ((r.textContent || '').indexOf('无匹配') === -1));
  return JSON.stringify(rows.map(r => ({
    tabindex: r.getAttribute('tabindex'),
    role: r.getAttribute('role'),
    lastCellHasRelTime: (() => {
      const tds = r.querySelectorAll('td');
      if (!tds.length) return false;
      return tds[tds.length - 1].querySelector('[data-testid="rel-time"]') !== null;
    })()
  })));
})()"""

TRACE_ROW_COUNT_JS = "JSON.stringify(document.querySelectorAll('div.divide-y > div').length > 0)"

TRACE_ROW_ATTRS_JS = """(() => {
  const rows = document.querySelectorAll('div.divide-y > div');
  return JSON.stringify(Array.from(rows).map(r => ({
    tabindex: r.getAttribute('tabindex'),
    role: r.getAttribute('role'),
    hasRelTime: r.querySelector('[data-testid="rel-time"]') !== null
  })));
})()"""

# --------------------------------------------------------------------------
# agent-browser 底层封装（subprocess，独立会话）
# --------------------------------------------------------------------------


def _ab(*args, timeout=120, stdin=None):
    """执行一条 agent-browser 命令；非零退出立即抛错（物理收据：Exit Code 0）。"""
    proc = subprocess.run(
        AB + [str(a) for a in args],
        input=stdin,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            "agent-browser {} 失败 rc={} err={} out={}".format(
                " ".join(map(str, args)),
                proc.returncode,
                proc.stderr[-400:],
                proc.stdout[-400:],
            )
        )
    return proc.stdout


def _eval(js, timeout=60):
    """eval --stdin 执行任意 JS，返回 CLI 原始 stdout（JSON 编码）。"""
    return _ab("eval", "--stdin", stdin=js, timeout=timeout).strip()


def _eval_json(js, timeout=60):
    """eval 并对返回的 JSON 解码（CLI 输出是 JSON 字符串再编码）。"""
    raw = _eval(js, timeout=timeout)
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        raise RuntimeError("eval 输出非 JSON: {}".format(raw[:200]))
    if isinstance(payload, str):
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            return payload
    return payload


def _wait_for(js_condition, timeout, msg, interval=0.3):
    """显式条件轮询（test-expert：禁止盲 sleep；条件满足即返回）。超时抛 AssertionError。"""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            val = _eval_json(js_condition, timeout=min(20, timeout + 2))
            if val is True or val == 1 or str(val).lower() == "true":
                return
            last = val
        except Exception as exc:  # JS 异常 / CLI 抖动视为未满足
            last = "eval-error: {}".format(exc)
        time.sleep(interval)
    raise AssertionError("{}（{}s 内未满足，last={}）".format(msg, timeout, last))


def _screenshot(kind):
    """物理收据：关键状态截图落盘 /tmp/pony-web-audit/<kind>-<phase>-<ts>.png。"""
    ts = datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    path = os.path.join(SHOT_DIR, "{}-{}-{}.png".format(kind, PHASE, ts))
    _ab("screenshot", path, timeout=60)
    if not (os.path.isfile(path) and os.path.getsize(path) > 0):
        raise AssertionError("截图未落盘或为空: {}".format(path))
    return path


def _open():
    """打开被测页面（同源 127.0.0.1:3000）。"""
    _ab("open", BASE_URL, timeout=60)


def _reload():
    _ab("reload", timeout=60)


def _switch_to_traces():
    _eval_json(
        """(() => {
      const btns = Array.from(document.querySelectorAll('nav button'));
      const b = btns.find(x => (x.textContent || '').indexOf('Traces') !== -1) || btns[1];
      if (!b) return JSON.stringify(false);
      b.click();
      return JSON.stringify(true);
    })()"""
    )


def _open_issues_drawer():
    """点击首条 Issue 行打开详情抽屉（数据前置由调用方保证）。"""
    _eval("(() => { const r = document.querySelector('tbody tr'); r && r.click(); return 'ok'; })()")
    _wait_for(DRAWER_OPEN_JS, 3, "点击 Issue 行后详情抽屉未打开")


def _open_trace_drawer():
    _eval("(() => { const r = document.querySelector('div.divide-y > div'); r && r.click(); return 'ok'; })()")
    _wait_for(DRAWER_OPEN_JS, 3, "点击 Trace 行后详情抽屉未打开")


def _close_drawer_via_esc():
    _eval("(document.activeElement && document.activeElement.blur()); 'ok'")
    _ab("press", "Escape", timeout=30)


def _drawer_panel_rect():
    """抽屉面板 bounding rect（角色 dialog 优先，兼容旧版遮罩首子元素）。"""
    return _eval_json(
        """(() => {
      const panel = document.querySelector('[role="dialog"]')
        || document.querySelector('.fixed.inset-0 > div');
      if (!panel) return JSON.stringify(null);
      const r = panel.getBoundingClientRect();
      return JSON.stringify({left: r.left, top: r.top, right: r.right,
                             bottom: r.bottom, width: r.width, height: r.height});
    })()"""
    )


def _gateway_state():
    """读健康徽标 data-state；元素缺失返回 None。"""
    return _eval_json(
        """(() => {
      const el = document.querySelector('[data-testid="gateway-status"]');
      return JSON.stringify(el ? el.getAttribute('data-state') : null);
    })()"""
    )


def _healthz_request_count():
    """网络日志中 GET /healthz 请求数（物理网络证据）。"""
    out = _ab("network", "requests", "--json", timeout=60)
    data = json.loads(out)
    reqs = data.get("data", {}).get("requests", [])
    return sum(
        1
        for r in reqs
        if r.get("method") == "GET" and "/healthz" in r.get("url", "")
    )


def _console_error_messages():
    out = _ab("console", "--json", timeout=60)
    return json.loads(out).get("data", {}).get("messages", [])


def _page_errors():
    out = _ab("errors", "--json", timeout=60)
    return json.loads(out).get("data", {}).get("errors", [])


# --------------------------------------------------------------------------
# 关键旅程（每个旅程返回截图路径并登记，供截图物理收据测试断言）
# --------------------------------------------------------------------------


def journey_issues_list():
    """旅程 1：Issues 列表。"""
    _open()
    _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
    shot = _screenshot("issues-list")
    SHOT_REGISTRY["issues-list"] = shot
    return shot


def journey_issues_drawer():
    """旅程 2：点击 Issue 行抽出详情抽屉。"""
    _open()
    _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
    _open_issues_drawer()
    shot = _screenshot("issues-drawer")
    SHOT_REGISTRY["issues-drawer"] = shot
    _close_drawer_via_esc()
    return shot


def journey_traces_view():
    """旅程 3：Traces 视图。"""
    _open()
    _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
    _switch_to_traces()
    _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
    shot = _screenshot("traces-view")
    SHOT_REGISTRY["traces-view"] = shot
    return shot


# --------------------------------------------------------------------------
# 测试套件
# --------------------------------------------------------------------------


class TestWebUIAcceptance(unittest.TestCase):
    """PonySentry Web 控制台 UI 契约验收（agent-browser 真实浏览器 E2E）。"""

    @classmethod
    def setUpClass(cls):
        os.makedirs(SHOT_DIR, exist_ok=True)
        # 物理探针：被测服务必须健康（curl /healthz Exit Code 0）
        try:
            probe = subprocess.run(
                ["curl", "-fsS", "-o", "/dev/null", "-w", "%{http_code}", BASE_URL + "/healthz"],
                capture_output=True, text=True, timeout=10,
            )
        except Exception as exc:
            raise RuntimeError("被测服务 {} 不可达: {}".format(BASE_URL, exc))
        if probe.returncode != 0:
            raise RuntimeError(
                "被测服务 {}/healthz 探针失败 rc={} stderr={}".format(
                    BASE_URL, probe.returncode, probe.stderr[-300:]
                )
            )
        # 独立会话：先回收可能残留的旧浏览器，再固定视口
        try:
            _ab("close", timeout=30)
        except Exception:
            pass
        _ab("set", "viewport", "1440", "900", timeout=30)

    @classmethod
    def tearDownClass(cls):
        try:
            _ab("close", timeout=30)
        except Exception:
            pass

    # ------------------------------------------------------------------
    # 关键旅程（物理截图载体）
    # ------------------------------------------------------------------

    def test_00_journey_issues_list(self):
        """旅程 1：Issues 列表渲染 + 截图（关键旅程物理收据载体）；本旅程 Console Error == 0。"""
        shot = journey_issues_list()
        self.assertIsNotNone(shot, "Issues 列表截图未生成")
        self._assert_no_console_errors("Issues 列表旅程")

    def test_01_journey_issues_drawer(self):
        """旅程 2：抽出详情抽屉 + 截图；本旅程 Console Error == 0。"""
        shot = journey_issues_drawer()
        self.assertIsNotNone(shot, "详情抽屉截图未生成")
        self._assert_no_console_errors("详情抽屉旅程")

    def test_02_journey_traces_view(self):
        """旅程 3：Traces 视图 + 截图；本旅程 Console Error == 0。"""
        shot = journey_traces_view()
        self.assertIsNotNone(shot, "Traces 视图截图未生成")
        self._assert_no_console_errors("Traces 视图旅程")

    def _assert_no_console_errors(self, where):
        """硬门禁 a 的分步判定：读取会话 console 消息与页面异常，断言 0 错误。"""
        messages = _console_error_messages()
        errs = [
            m
            for m in messages
            if m.get("type") == "error" or str(m.get("level", "")).lower() == "error"
        ]
        page_errors = _page_errors()
        combined = "{}; {}".format(
            [e.get("text", e)[:200] for e in errs[:5]],
            [str(e)[:200] for e in page_errors[:5]],
        )
        self.assertEqual(
            len(errs) + len(page_errors), 0,
            "硬门禁 a（{}）：Console Error / 未捕获异常必须为 0，实际 console={} 条、page={} 条: {}"
            .format(where, len(errs), len(page_errors), combined),
        )

    # ------------------------------------------------------------------
    # §1 data-testid 清单
    # ------------------------------------------------------------------

    def test_10_t1_1_gateway_status_testid(self):
        """T1.1 REQ：gateway-status 恰好 1 个且 data-state ∈ {checking,ready,degraded}。"""
        _open()
        count = _eval_json(GATEWAY_COUNT_JS)
        self.assertEqual(
            count, 1,
            "契约 T1.1：健康徽标 [data-testid=\"gateway-status\"] 必须恰好 1 个，"
            "实际 {} 个（旧版硬编码徽标无 testid）".format(count),
        )
        state = _gateway_state()
        self.assertIn(
            state, ("checking", "ready", "degraded"),
            "契约 T1.1：data-state 必须 ∈ {checking, ready, degraded}，实际 {!r}".format(state),
        )

    def test_11_t1_2_load_error_banner(self):
        """T1.2 REQ：load-error 横幅存在；数据接口失败时可见且文案区分加载失败/无数据。"""
        _open()
        banner = _eval_json(
            """(() => {
          const el = document.querySelector('[data-testid="load-error"]');
          if (!el) return JSON.stringify(null);
          const cs = getComputedStyle(el);
          return JSON.stringify({visible: cs.display !== 'none' && !el.hasAttribute('hidden'),
                                 text: (el.textContent || '').trim()});
        })()"""
        )
        self.assertIsNotNone(
            banner,
            "契约 T1.2：错误横幅 [data-testid=\"load-error\"] 必须存在（旧版无错误横幅）",
        )
        # 模拟数据接口失败（网络日志拦截，物理证据：fetch 以 TypeError 拒绝）
        try:
            _ab("network", "route", BASE_URL + "/api/*", "--abort", timeout=30)
            _reload()
            _wait_for(
                """(() => {
              const el = document.querySelector('[data-testid="load-error"]');
              if (!el) return JSON.stringify(false);
              const cs = getComputedStyle(el);
              return JSON.stringify(cs.display !== 'none' && !el.hasAttribute('hidden'));
            })()""",
                6,
                "数据接口失败后 load-error 横幅应可见",
            )
            text = _eval_json(
                """(() => {
              const el = document.querySelector('[data-testid="load-error"]');
              return JSON.stringify(el ? (el.textContent || '').trim() : '');
            })()"""
            )
            self.assertTrue(
                text and text not in EMPTY_STATE_TEXTS,
                "契约 T1.2：失败横幅文案必须非空且与空态文案可区分（不得等于 {}），"
                "实际 {!r}".format(EMPTY_STATE_TEXTS, text),
            )
        finally:
            _ab("network", "unroute", timeout=30)

    def test_12_t1_3_rel_time_on_rows(self):
        """T1.3 REQ：Traces 每行含 [data-testid=\"rel-time\"]；Issues '最后上报' 列行同要求。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        issue_attrs = _eval_json(ISSUE_ROW_ATTRS_JS)
        self.assertGreater(
            len(issue_attrs), 0,
            "契约 T1.3：Issues 表必须存在可断言的数据行（数据前置）",
        )
        no_rel = [i for i, a in enumerate(issue_attrs) if not a["lastCellHasRelTime"]]
        self.assertEqual(
            no_rel, [],
            "契约 T1.3：Issues '最后上报' 单元格每行必须含 [data-testid=\"rel-time\"]，"
            "缺失行索引: {}（旧版为绝对时间无 rel-time 元素）".format(no_rel[:10]),
        )
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        trace_attrs = _eval_json(TRACE_ROW_ATTRS_JS)
        self.assertGreater(
            len(trace_attrs), 0,
            "契约 T1.3：Traces 列表必须存在可断言的数据行（数据前置）",
        )
        no_rel = [i for i, a in enumerate(trace_attrs) if not a["hasRelTime"]]
        self.assertEqual(
            no_rel, [],
            "契约 T1.3：Traces 每行必须含 [data-testid=\"rel-time\"]，缺失行索引: {}"
            .format(no_rel[:10]),
        )

    # ------------------------------------------------------------------
    # §2 a11y 契约
    # ------------------------------------------------------------------

    def test_20_t2_1_issues_rows_keyboard(self):
        """T2.1 REQ：Issues 可点击行 tabindex=0 + role=button；Enter/Space 触发打开抽屉。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        attrs = _eval_json(ISSUE_ROW_ATTRS_JS)
        self.assertGreater(len(attrs), 0, "契约 T2.1：无数据行可断言（数据前置）")
        bad = [
            "row#{} tabindex={!r} role={!r}".format(i, a["tabindex"], a["role"])
            for i, a in enumerate(attrs)
            if a["tabindex"] != "0" or a["role"] != "button"
        ]
        self.assertEqual(
            bad, [],
            "契约 T2.1：每个可点击 Issue 行必须 tabindex=\"0\" 且 role=\"button\"，"
            "违规行: {}".format(bad[:10]),
        )
        # Enter 打开抽屉
        _eval("(() => { const r = document.querySelector('tbody tr'); r && r.focus(); return 'ok'; })()")
        _ab("press", "Enter", timeout=30)
        _wait_for(DRAWER_OPEN_JS, 2, "契约 T2.1：聚焦首行后按 Enter 必须打开详情抽屉")
        _close_drawer_via_esc()
        _wait_for(DRAWER_CLOSED_JS, 2, "Esc 关闭抽屉失败（T2.1 前置）")
        # Space 打开抽屉
        _eval("(() => { const r = document.querySelector('tbody tr'); r && r.focus(); return 'ok'; })()")
        _ab("press", "Space", timeout=30)
        _wait_for(DRAWER_OPEN_JS, 2, "契约 T2.1：聚焦首行后按 Space 必须打开详情抽屉")

    def test_21_t2_2_traces_cards_keyboard(self):
        """T2.2 REQ：Traces 可点击卡片 tabindex=0 + role=button；Enter/Space 生效。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        attrs = _eval_json(TRACE_ROW_ATTRS_JS)
        self.assertGreater(len(attrs), 0, "契约 T2.2：无 Trace 数据行可断言（数据前置）")
        bad = [
            "row#{} tabindex={!r} role={!r}".format(i, a["tabindex"], a["role"])
            for i, a in enumerate(attrs)
            if a["tabindex"] != "0" or a["role"] != "button"
        ]
        self.assertEqual(
            bad, [],
            "契约 T2.2：每个可点击 Trace 卡片必须 tabindex=\"0\" 且 role=\"button\"，"
            "违规行: {}".format(bad[:10]),
        )
        _eval("(() => { const r = document.querySelector('div.divide-y > div'); r && r.focus(); return 'ok'; })()")
        _ab("press", "Enter", timeout=30)
        _wait_for(DRAWER_OPEN_JS, 2, "契约 T2.2：聚焦首行后按 Enter 必须打开 Trace 详情抽屉")
        _close_drawer_via_esc()
        _eval("(() => { const r = document.querySelector('div.divide-y > div'); r && r.focus(); return 'ok'; })()")
        _ab("press", "Space", timeout=30)
        _wait_for(DRAWER_OPEN_JS, 2, "契约 T2.2：聚焦首行后按 Space 必须打开 Trace 详情抽屉")

    def test_22_t2_3_aria_labels(self):
        """T2.3 REQ：icon-only 按钮（无可见文本/仅符号图标）与筛选 select 必须带非空 aria-label。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        violations = _eval_json(
            """(() => {
          const bad = [];
          document.querySelectorAll('select').forEach((s, i) => {
            if (!(s.getAttribute('aria-label') || '').trim()) bad.push('select#' + i);
          });
          document.querySelectorAll('button').forEach((b, i) => {
            const txt = (b.textContent || '').trim();
            const hasIcon = b.querySelector('svg, img') !== null;
            // icon-only 判定：仅含 svg/img，或可见文本为空，或文本全为符号字符（如 ×）
            const iconLike = hasIcon && txt === ''
                             || txt === ''
                             || (txt !== '' && !/[\u4e00-\u9fffA-Za-z0-9]/.test(txt));
            if (iconLike && !(b.getAttribute('aria-label') || '').trim()) {
              bad.push('button#' + i + ':' + b.outerHTML.slice(0, 90));
            }
          });
          return JSON.stringify(bad);
        })()"""
        )
        self.assertEqual(
            violations, [],
            "契约 T2.3：icon-only 按钮与 select 必须带非空 aria-label，违规项: {}"
            .format(violations[:10]),
        )

    def test_23_t2_4_drawer_role_dialog(self):
        """T2.4 REQ：抽屉面板 role=dialog + aria-modal=true + 非空 aria-label（Issues 与 Traces 双抽屉）。"""
        problems = []
        # Issues 抽屉
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _open_issues_drawer()
        dialog = _eval_json(
            """(() => {
          const el = document.querySelector('[role="dialog"]');
          if (!el) return JSON.stringify(null);
          return JSON.stringify({modal: el.getAttribute('aria-modal'),
                                 label: el.getAttribute('aria-label')});
        })()"""
        )
        if dialog is None:
            problems.append("Issues 抽屉缺少 role=\"dialog\"（旧版无 dialog 语义）")
        else:
            if dialog["modal"] != "true":
                problems.append("Issues 抽屉 aria-modal={!r}，须为 \"true\"".format(dialog["modal"]))
            if not (dialog["label"] or "").strip():
                problems.append("Issues 抽屉 aria-label 为空")
        _close_drawer_via_esc()
        # Traces 抽屉
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        _open_trace_drawer()
        dialog = _eval_json(
            """(() => {
          const el = document.querySelector('[role="dialog"]');
          if (!el) return JSON.stringify(null);
          return JSON.stringify({modal: el.getAttribute('aria-modal'),
                                 label: el.getAttribute('aria-label')});
        })()"""
        )
        if dialog is None:
            problems.append("Traces 抽屉缺少 role=\"dialog\"")
        else:
            if dialog["modal"] != "true":
                problems.append("Traces 抽屉 aria-modal={!r}，须为 \"true\"".format(dialog["modal"]))
            if not (dialog["label"] or "").strip():
                problems.append("Traces 抽屉 aria-label 为空")
        self.assertEqual(problems, [], "契约 T2.4：{}".format("；".join(problems)))

    # ------------------------------------------------------------------
    # §3 Trace 行内边距
    # ------------------------------------------------------------------

    def test_30_t3_trace_row_padding(self):
        """T3 REQ（动态主判据）：每条 Trace 行四轴 computed padding ≥ 8px（p-4.5 已废除）。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        pads = _eval_json(
            """(() => {
          const rows = document.querySelectorAll('div.divide-y > div');
          return JSON.stringify(Array.from(rows).map(r => {
            const cs = getComputedStyle(r);
            return {pt: parseFloat(cs.paddingTop), pr: parseFloat(cs.paddingRight),
                    pb: parseFloat(cs.paddingBottom), pl: parseFloat(cs.paddingLeft)};
          }));
        })()"""
        )
        self.assertGreater(len(pads), 0, "契约 T3：无 Trace 数据行可断言（数据前置）")
        bad = [
            "row#{} pt={} pr={} pb={} pl={}".format(i, p["pt"], p["pr"], p["pb"], p["pl"])
            for i, p in enumerate(pads)
            if p["pt"] < 8 or p["pr"] < 8 or p["pb"] < 8 or p["pl"] < 8
        ]
        self.assertEqual(
            bad, [],
            "契约 T3：Trace 行四轴内边距必须 ≥ 8px（p-4.5 非法类已废除，旧版 computed padding=0），"
            "违规行: {}".format(bad[:10]),
        )

    def test_31_t3_static_no_p45_class(self):
        """T3 静态门禁 @review：源码不含非法类 p-4.5（契约给定非零退出命令）。"""
        pattern = r"""(^|[[:space:]"'`])p-4\.5([[:space:]"'`]|$)"""
        grep = subprocess.run(
            ["grep", "-rn", "-E", pattern, "web/src"],
            cwd=PROJECT_ROOT, capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(
            grep.returncode, 0,
            "契约 T3 静态门禁：web/src 不得包含非法 Tailwind 类 p-4.5，命中行: {}"
            .format(grep.stdout[:400]),
        )

    # ------------------------------------------------------------------
    # §4 相对时间
    # ------------------------------------------------------------------

    def _rel_time_elements(self):
        return _eval_json(
            """(() => {
          const els = document.querySelectorAll('[data-testid="rel-time"]');
          return JSON.stringify(Array.from(els).map(el => ({
            text: (el.textContent || '').trim(),
            title: el.getAttribute('title')
          })));
        })()"""
        )

    def test_40_t4_1_rel_time_text_relative(self):
        """T4.1 REQ：rel-time 文本必须匹配 ^\\d+\\s*(分钟|小时|天)前$（不接受绝对时间/刚刚）。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        rels = self._rel_time_elements()
        self.assertGreater(
            len(rels), 0,
            "契约 T4.1：列表必须存在 [data-testid=\"rel-time\"] 元素（旧版为绝对时间戳）",
        )
        pattern = re.compile(r"^\d+\s*(分钟|小时|天)前$")
        bad = [r["text"] for r in rels if not pattern.match(r["text"])]
        self.assertEqual(
            bad, [],
            "契约 T4.1：相对时间文本必须匹配 ^\\d+\\s*(分钟|小时|天)前$，违规文本: {}"
            .format(bad[:10]),
        )

    def test_41_t4_2_rel_time_title_iso(self):
        """T4.2 REQ：rel-time 元素携带 title 绝对时间（ISO8601 可解析）且与相对值一致（±2min）。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        rels = self._rel_time_elements()
        self.assertGreater(
            len(rels), 0,
            "契约 T4.2：列表必须存在 [data-testid=\"rel-time\"] 元素（旧版无 rel-time）",
        )
        now = datetime.now(timezone.utc)
        pattern = re.compile(r"^(\d+)\s*(分钟|小时|天)前$")
        bad = []
        for r in rels:
            if not r["title"]:
                bad.append("缺 title 的元素: {!r}".format(r["text"]))
                continue
            try:
                dt = datetime.fromisoformat(r["title"].replace("Z", "+00:00"))
            except ValueError as exc:
                bad.append("title 非 ISO8601: {!r} ({})".format(r["title"], exc))
                continue
            m = pattern.match(r["text"])
            if not m:
                bad.append("相对时间文本无法解析: {!r}".format(r["text"]))
                continue
            unit = m.group(2)
            factor = {"分钟": 60, "小时": 3600, "天": 86400}[unit]
            rel_sec = int(m.group(1)) * factor
            drift = abs((now - dt).total_seconds() - rel_sec)
            if drift > 150:  # 契约 ±2 分钟容差（留少量执行余量）
                bad.append(
                    "title {!r} 与文本 {!r} 偏差 {:.0f}s 超 ±2min".format(r["title"], r["text"], drift)
                )
        self.assertEqual(
            bad, [],
            "契约 T4.2：rel-time title 必须为 ISO8601 绝对时间且与相对值一致: {}"
            .format(bad[:10]),
        )

    # ------------------------------------------------------------------
    # §5 抽屉交互
    # ------------------------------------------------------------------

    def test_50_t5_1_issues_drawer_bg_click_closes(self):
        """T5.1 REQ：Issues 抽屉点击面板外背景遮罩区域 → 1s 内关闭（@click.self 语义）。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _open_issues_drawer()
        rect = _drawer_panel_rect()
        self.assertIsNotNone(rect, "契约 T5.1：无法定位抽屉面板（前置失败）")
        # 点击视口内、面板 rect 之外的背景点（面板右侧 672px，左侧大片为背景遮罩）
        x = max(10, int(rect["left"] * 0.5))
        y = min(400, max(100, int(rect["top"] + rect["height"] * 0.4)))
        self.assertLess(x, rect["left"], "背景点击点必须位于面板左侧之外")
        _ab("mouse", "move", str(x), str(y), timeout=30)
        _ab("mouse", "down", "left", timeout=30)
        time.sleep(0.05)
        _ab("mouse", "up", "left", timeout=30)
        # 1s 内抽屉必须关闭（契约 T5.1）
        try:
            _wait_for(
                DRAWER_CLOSED_JS,
                1.5,
                "契约 T5.1：点击背景遮罩后抽屉应在 1s 内关闭（旧版 Issues 抽屉无 @click.self，红相必失败）",
            )
        except AssertionError:
            self.fail(
                "契约 T5.1：点击背景遮罩({},{}) 后抽屉未关闭——Issues 抽屉缺 @click.self 背景关闭"
                "（旧版红相必失败）".format(x, y)
            )

    def test_51_t5_2_esc_closes_drawer_exactly_once(self):
        """T5.2 REQ：无焦点时 Esc 关闭抽屉且恰好一次（Issues 与 Traces 同规则，统一全局兜底）。"""
        # Issues 抽屉
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _open_issues_drawer()
        _close_drawer_via_esc()
        _wait_for(DRAWER_CLOSED_JS, 1.5, "契约 T5.2：Issues 抽屉按 Esc 应在 1.5s 内关闭")
        time.sleep(0.6)  # 负断言观察窗：确保未因双触发重开
        self.assertFalse(
            _eval_json(DRAWER_OPEN_JS),
            "契约 T5.2：Issues 抽屉 Esc 关闭后不得重开（双触发）",
        )
        # Traces 抽屉
        _switch_to_traces()
        _wait_for(TRACE_ROW_COUNT_JS, 8, "Traces 视图数据行未渲染")
        _open_trace_drawer()
        _close_drawer_via_esc()
        _wait_for(DRAWER_CLOSED_JS, 1.5, "契约 T5.2：Traces 抽屉按 Esc 应在 1.5s 内关闭（旧版 Traces 抽屉无全局 Esc 兜底）")
        time.sleep(0.6)
        self.assertFalse(
            _eval_json(DRAWER_OPEN_JS),
            "契约 T5.2：Traces 抽屉 Esc 关闭后不得重开（双触发）",
        )

    def test_52_t5_3_panel_click_keeps_drawer_open(self):
        """T5.3 REQ：点击抽屉面板内部不得关闭抽屉（背景关闭仅对遮罩生效）。"""
        _open()
        _wait_for(ISSUE_ROWS_JS, 10, "Issues 列表数据行未渲染")
        _open_issues_drawer()
        rect = _drawer_panel_rect()
        self.assertIsNotNone(rect, "契约 T5.3：无法定位抽屉面板（前置失败）")
        x = int(rect["left"] + rect["width"] * 0.5)
        y = int(rect["top"] + rect["height"] * 0.5)
        _ab("mouse", "move", str(x), str(y), timeout=30)
        _ab("mouse", "down", "left", timeout=30)
        time.sleep(0.05)
        _ab("mouse", "up", "left", timeout=30)
        time.sleep(0.6)  # 负断言观察窗
        self.assertTrue(
            _eval_json(DRAWER_OPEN_JS),
            "契约 T5.3：点击面板内部({},{}) 后抽屉不应关闭".format(x, y),
        )

    # ------------------------------------------------------------------
    # §6 健康徽标
    # ------------------------------------------------------------------

    def test_60_t6_1_healthz_requested_on_load(self):
        """T6.1 REQ：页面加载后 3s 内网络日志出现 GET /healthz（同源）。"""
        _ab("network", "requests", "--clear", timeout=30)
        _open()
        deadline = time.time() + 3.5
        count = 0
        while time.time() < deadline:
            count = _healthz_request_count()
            if count >= 1:
                break
            time.sleep(0.3)
        self.assertGreaterEqual(
            count, 1,
            "契约 T6.1：加载后 3s 内必须发起 GET /healthz（旧版硬编码徽标无轮询，红相必失败）",
        )

    def test_61_t6_2_health_badge_state_matches(self):
        """T6.2 REQ：data-state 与真实响应一致——200→ready；网络错误(abort)→degraded。"""
        _open()
        count = _eval_json(GATEWAY_COUNT_JS)
        self.assertEqual(
            count, 1,
            "契约 T6.2：gateway-status 必须存在（旧版无此元素）",
        )
        # 真实 /healthz 返回 200 → 状态必须为 ready
        _wait_for(
            "JSON.stringify(((() => { const el = document.querySelector('[data-testid=\"gateway-status\"]');"
            " return el ? el.getAttribute('data-state') : null })()) === 'ready')",
            5,
            "契约 T6.2：/healthz 200 后 data-state 必须为 ready",
        )
        # 拦截 /healthz 使其网络错误（fetch TypeError）→ 状态必须为 degraded
        try:
            _ab("network", "route", BASE_URL + "/healthz", "--abort", timeout=30)
            _reload()
            _wait_for(
                "JSON.stringify(((() => { const el = document.querySelector('[data-testid=\"gateway-status\"]');"
                " return el ? el.getAttribute('data-state') : null })()) === 'degraded')",
                6,
                "契约 T6.2：/healthz 网络错误后 data-state 必须为 degraded",
            )
        finally:
            _ab("network", "unroute", timeout=30)

    def test_62_t6_3_initial_state_checking(self):
        """T6.3 REQ：初始渲染状态为 checking（灰），随后按响应迁移（中间态机器断言）。"""
        _open()
        count = _eval_json(GATEWAY_COUNT_JS)
        self.assertEqual(
            count, 1,
            "契约 T6.3：gateway-status 必须存在（旧版无此元素）",
        )
        # 延迟 /healthz 响应 3s，制造可观测的 checking 中间态（每个轮询周期开始时置 checking）
        _eval(
            """(() => {
          window.__origFetch = window.fetch;
          window.fetch = (url, opts) => {
            if (String(url).indexOf('/healthz') !== -1) {
              return new Promise((resolve, reject) => {
                setTimeout(() => { window.__origFetch(url, opts).then(resolve, reject); }, 3000);
              });
            }
            return window.__origFetch(url, opts);
          };
          return 'patched';
        })()"""
        )
        _wait_for(
            "JSON.stringify(((() => { const el = document.querySelector('[data-testid=\"gateway-status\"]');"
            " return el ? el.getAttribute('data-state') : null })()) === 'checking')",
            22,
            "契约 T6.3：轮询周期起始必须出现 checking 中间态（随后迁移）",
        )
        _wait_for(
            "JSON.stringify(((() => { const el = document.querySelector('[data-testid=\"gateway-status\"]');"
            " return el ? el.getAttribute('data-state') : null })()) === 'ready')",
            8,
            "契约 T6.3：checking 之后必须按真实响应迁移到 ready",
        )

    def test_63_t6_4_healthz_poll_interval(self):
        """T6.4 REQ：轮询间隔常数 15000ms（契约允许源码级 @review 核验其一即足）。"""
        src = os.path.join(WEB_SRC_DIR, "App.vue")
        self.assertTrue(os.path.isfile(src), "web/src/App.vue 不存在（源码级核验前置）")
        with open(src, "r", encoding="utf-8") as f:
            content = f.read()
        self.assertIn(
            "15000", content,
            "契约 T6.4：健康轮询间隔常数必须为 15000ms（setInterval(…,15000)）",
        )

    def test_64_t6_5_healthz_abort_timeout(self):
        """T6.5 REQ：/healthz fetch 挂 abort 信号且超时 ≤ 3000ms（源码级核验）。"""
        src = os.path.join(WEB_SRC_DIR, "App.vue")
        self.assertTrue(os.path.isfile(src), "web/src/App.vue 不存在（源码级核验前置）")
        with open(src, "r", encoding="utf-8") as f:
            content = f.read()
        ok = ("AbortSignal.timeout(3000)" in content) or (
            "AbortController" in content and "3000" in content
        )
        self.assertTrue(
            ok,
            "契约 T6.5：/healthz fetch 必须挂 AbortSignal（≤3000ms 超时，对齐 NFR "
            "external_call_timeout_ms=3000）",
        )

    def test_65_t6_6_unmount_cleanup_source(self):
        """T6.6 REQ @review：卸载时清理轮询 timer 与全局监听器（clearInterval/onUnmounted）。"""
        src = os.path.join(WEB_SRC_DIR, "App.vue")
        self.assertTrue(os.path.isfile(src), "web/src/App.vue 不存在（源码级核验前置）")
        with open(src, "r", encoding="utf-8") as f:
            content = f.read()
        self.assertIn("onUnmounted", content, "契约 T6.6：健康徽标实现必须含 onUnmounted 卸载清理")
        self.assertIn("clearInterval", content, "契约 T6.6：卸载时必须 clearInterval 轮询 timer")
        self.assertIn(
            "removeEventListener", content,
            "契约 T6.6：卸载时必须移除全局监听器（event listener 清理）",
        )

    # ------------------------------------------------------------------
    # §7 NFR 基准核对
    # ------------------------------------------------------------------

    def test_70_t7_nfr_baseline_json(self):
        """T7：NFR 基准 JSON 物理校验（Exit Code 0 + NFR_JSON_OK / NFR_SCHEMA_OK）。"""
        gate1 = subprocess.run(
            ["python3", "-m", "json.tool", NFR_JSON],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=30,
        )
        self.assertEqual(gate1.returncode, 0, "NFR JSON 非法: {}".format(gate1.stderr[-300:]))
        gate2_script = """import json, sys
EXPECT = {'external_call_timeout_ms': int, 'retry': dict,
          'concurrency_lock': str, 'logging': dict, 'resource_release': list}
d = json.load(open(sys.argv[1]))
assert set(d) == set(EXPECT), f'unknown/missing fields: {set(d) ^ set(EXPECT)}'
for k, t in EXPECT.items():
    assert isinstance(d[k], t), k
assert d['external_call_timeout_ms'] == 3000, 'frozen value drift'
assert d['retry'] == {'max_attempts': 3, 'backoff_ms': 500}
assert set(d['resource_release']) >= {'abort_controllers', 'event_listeners', 'background_timers'}
print('NFR_SCHEMA_OK')
"""
        gate2 = subprocess.run(
            ["python3", "-c", gate2_script, NFR_JSON],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(gate2.returncode, 0, "NFR Schema 校验失败: {}".format(gate2.stderr[-400:]))
        self.assertIn("NFR_SCHEMA_OK", gate2.stdout, "NFR Schema 校验应输出 NFR_SCHEMA_OK")

    # ------------------------------------------------------------------
    # 硬门禁 a)：Console Error == 0
    # ------------------------------------------------------------------

    def test_90_console_errors_zero_across_journeys(self):
        """硬门禁 a：会话全程（含全部关键旅程）Console Error == 0 的最终只读判定。

        三个关键旅程的每步控制台零错误已分别在 test_00/01/02 内即时判定；
        此处不做重放（避免长会话尾部浏览器无响应导致的伪失败），
        直接读取会话累积的 console/page 错误日志作为最终物理收据。
        """
        messages = _console_error_messages()
        errs = [
            m
            for m in messages
            if m.get("type") == "error" or str(m.get("level", "")).lower() == "error"
        ]
        self.assertEqual(
            errs, [],
            "硬门禁 a：会话全程必须 0 个 Console Error，实际 {} 条: {}".format(
                len(errs), [e.get("text", e)[:200] for e in errs[:5]]
            ),
        )
        page_errors = _page_errors()
        self.assertEqual(
            page_errors, [],
            "硬门禁 a：会话全程必须 0 个未捕获页面异常，实际 {} 条: {}".format(
                len(page_errors), [str(e)[:200] for e in page_errors[:5]]
            ),
        )

    # ------------------------------------------------------------------
    # 硬门禁 b)：关键旅程截图物理收据
    # ------------------------------------------------------------------

    def test_91_physical_screenshots_landed(self):
        """硬门禁 b：三个关键旅程（Issues 列表/详情抽屉/Traces 视图）各有一张非空物理截图。"""
        missing = []
        for kind in ("issues-list", "issues-drawer", "traces-view"):
            path = SHOT_REGISTRY.get(kind)
            if not path:
                missing.append(kind)
                continue
            if not (os.path.isfile(path) and os.path.getsize(path) > 0):
                missing.append(kind + "(空文件)")
        self.assertEqual(
            missing, [],
            "硬门禁 b：关键旅程截图必须全部落盘 /tmp/pony-web-audit/，缺失: {}"
            .format(missing),
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
