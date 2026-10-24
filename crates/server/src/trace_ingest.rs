//! `PONY_TRACE_INGEST_ENABLED` 开关：tracing 停收的唯一入口。
//!
//! 契约（行为面）：
//! - 默认**开启**（未设置 / 空值 / 任何非关闭值）。只有 `0` / `false` / `off`
//!   （大小写不敏感、去首尾空白）关闭。
//! - 关闭后 `POST /api/v1/traces`、`GET /api/v1/traces`、`GET /api/v1/traces/:id`、
//!   `PATCH /api/v1/traces/:id` 一律 `410 Gone`，响应体固定为
//!   `{"error":"trace_ingest_disabled","message":"tracing moved to tempo+langfuse"}`。
//!   拦截发生在**处理器之前**（路由层中间件）：不写库、不递增 trace 指标。
//! - `GET /healthz`、`POST /api/v1/ingest`、`GET/PATCH /api/v1/issues*`、`GET /metrics`
//!   不受影响——Issue 采集与探针语义完全不变。
//!
//! 归属：开关本身不持有状态，经 `Extension` 之外的显式 state 注入（同 `TrustConfig`），
//! 因此进程内测试可以不经环境变量就构造「关闭」实例，避免并发改 env 的用例互扰。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tracing::info;

/// 环境变量键（唯一开关）。
pub const TRACE_INGEST_ENABLED_ENV: &str = "PONY_TRACE_INGEST_ENABLED";

/// `410 Gone` 响应体的 `error` 字段（客户端按此判定「tracing 已迁走」）。
pub const TRACE_INGEST_DISABLED_ERROR: &str = "trace_ingest_disabled";

/// `410 Gone` 响应体的 `message` 字段（人读指引）。
pub const TRACE_INGEST_DISABLED_MESSAGE: &str = "tracing moved to tempo+langfuse";

/// tracing 接收开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceIngestSwitch {
    enabled: bool,
}

impl Default for TraceIngestSwitch {
    /// 默认开启：未配置环境变量的进程行为与开关引入前一致。
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl TraceIngestSwitch {
    /// 从环境变量构造并打印一次启动日志（与 `TrustConfig::from_env` 同风格）。
    pub fn from_env() -> Self {
        let raw = std::env::var(TRACE_INGEST_ENABLED_ENV).ok();
        let switch = Self::from_value(raw.as_deref());
        if switch.enabled {
            info!(
                trace_ingest_enabled = true,
                env_key = TRACE_INGEST_ENABLED_ENV,
                "tracing 接收开启（POST/GET/PATCH /api/v1/traces 正常服务）"
            );
        } else {
            info!(
                trace_ingest_enabled = false,
                env_key = TRACE_INGEST_ENABLED_ENV,
                "tracing 接收已关闭：4 个 trace 路由返回 410；Issue 采集与探针不受影响"
            );
        }
        switch
    }

    /// [`Self::from_env`] 的纯函数内核（env 只读一次）：单独暴露给测试，避免并发改 env。
    ///
    /// - `None`（未设置）→ 开启
    /// - 空白串 → 开启（视同未设置，不因空值误停收）
    /// - `0` / `false` / `off`（去空白、大小写不敏感）→ 关闭
    /// - 其它任何值 → 开启（保守：只有显式关闭才关闭，避免拼写错误导致静默停收）
    pub fn from_value(raw: Option<&str>) -> Self {
        let enabled = match raw.map(str::trim) {
            None | Some("") => true,
            Some(value) => !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "off"),
        };
        Self { enabled }
    }

    /// 是否接收 tracing（`true` = 开关未关闭）。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 是否已停收（`true` = 4 个 trace 路由应返回 410）。
    pub fn disabled(&self) -> bool {
        !self.enabled
    }

    /// 停收响应：`410 Gone` + 固定 JSON 体。
    pub fn disabled_response() -> Response {
        (
            StatusCode::GONE,
            Json(json!({
                "error": TRACE_INGEST_DISABLED_ERROR,
                "message": TRACE_INGEST_DISABLED_MESSAGE,
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_keeps_ingest_enabled() {
        assert!(TraceIngestSwitch::from_value(None).enabled());
        assert_eq!(
            TraceIngestSwitch::default(),
            TraceIngestSwitch { enabled: true }
        );
    }

    #[test]
    fn empty_or_blank_values_keep_ingest_enabled() {
        for raw in ["", "   ", "\t\n"] {
            assert!(
                TraceIngestSwitch::from_value(Some(raw)).enabled(),
                "空白值 {raw:?} 不应停收 tracing"
            );
        }
    }

    #[test]
    fn off_values_disable_ingest() {
        for raw in ["0", "false", "off", "FALSE", "Off", "  0  ", "\toff\n"] {
            let switch = TraceIngestSwitch::from_value(Some(raw));
            assert!(switch.disabled(), "{raw:?} 应停收 tracing");
            assert!(!switch.enabled());
        }
    }

    #[test]
    fn unknown_values_keep_ingest_enabled() {
        for raw in ["1", "true", "on", "yes", "disabled", "garbage", "00"] {
            assert!(
                TraceIngestSwitch::from_value(Some(raw)).enabled(),
                "{raw:?} 不是显式关闭值，应保持开启"
            );
        }
    }

    #[test]
    fn disabled_response_is_410_with_fixed_body() {
        let response = TraceIngestSwitch::disabled_response();
        assert_eq!(response.status(), StatusCode::GONE);
    }
}
