use crate::parser::{Frame, RawEvent, Stacktrace};
use pony_sentry_core::reindex::FingerprintOut;
use pony_sentry_fingerprint::FingerprintEngine;

/// 环境维度归一：trim + 小写，空白归一为 `unknown`（契约 R6.0）。
///
/// 与 v3 指纹内部归一保持一致，避免"仅大小写不同"的环境产生
/// 相同指纹却写成两条 issue 行的唯一约束冲突。
pub fn normalize_environment(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 由上报事件计算 v3 指纹及其派生字段（title / culprit / project / 多维维度）。
///
/// 这是**唯一**的指纹真相源：实时上报（routes.rs）与全量重索引（reindex）
/// 都调用本函数，保证两套路径产出的指纹完全一致，避免"重索引后新事件
/// 落不进正确 issue"的二次漂移。
pub fn compute_issue_fingerprint(event: &RawEvent) -> FingerprintOut {
    // 1. 提取核心信息：error_type / culprit（栈顶帧或兜底）/ title。
    //    一级 `error_type` 优先，缺省回退 `exception.error_type`（契约 R2）。
    let (error_type, mut culprit, title) = match event.exception {
        Some(ref ex) => {
            let frames: &[Frame] = match ex.stacktrace.as_ref() {
                Some(Stacktrace::Frames(frames)) => frames,
                _ => &[],
            };
            let cul = frames.first().and_then(frame_culprit);
            let resolved_type = event
                .error_type
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| ex.error_type.clone());
            (
                resolved_type.clone(),
                cul,
                format!("{}: {}", resolved_type, ex.value.as_deref().unwrap_or("")),
            )
        }
        None => {
            let resolved_type = event
                .error_type
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "GenericError".to_string());
            (
                resolved_type,
                None,
                event
                    .message
                    .clone()
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )
        }
    };

    // 1.1 若 stacktrace 缺失，尝试从 tags / extra / title 智能推导兜底 culprit
    if culprit.is_none() {
        culprit = extract_fallback_culprit(event, &title);
    }

    // 2. error_kind / model 判别器：无条件提取（不再受 culprit 有无影响）
    let error_kind = event
        .tags
        .as_ref()
        .and_then(|t| t.get("error_kind"))
        .cloned();
    let model = event
        .tags
        .as_ref()
        .and_then(|t| t.get("requested_model").or_else(|| t.get("model")).cloned())
        .or_else(|| {
            event
                .extra
                .as_ref()
                .and_then(|e| e.get("requested_model").or_else(|| e.get("model")))
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        });

    // 3. v3 指纹：v2 五元组 + environment 作用域（契约 R6）
    let environment = normalize_environment(event.environment.as_deref());
    let fingerprint = FingerprintEngine::compute_fingerprint_v3(
        &event.platform,
        &error_type,
        error_kind.as_deref(),
        model.as_deref(),
        culprit.as_deref(),
        &environment,
    );

    // 4. 项目维度：使用 infer_project_name 综合 tags、extra、title 与 culprit 智能识别
    let project = crate::project::infer_project_name(
        event.tags.as_ref(),
        event.extra.as_ref(),
        Some(&title),
        culprit.as_deref(),
    );

    FingerprintOut {
        fingerprint,
        title,
        culprit,
        platform: event.platform.clone(),
        project,
        environment,
        app: event.app.clone(),
        namespace: event.namespace.clone(),
        component: event.component.clone(),
        level: event
            .level
            .clone()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| "error".to_string()),
        source_kind: event.source_kind.clone(),
    }
}

/// 栈帧 → culprit（`file in func`），任一缺失时退化到可用部分。
fn frame_culprit(frame: &Frame) -> Option<String> {
    match (&frame.filename, &frame.function) {
        (Some(file), Some(func)) => Some(format!("{} in {}", file, func)),
        (Some(file), None) => Some(file.clone()),
        (None, Some(func)) => Some(func.clone()),
        _ => None,
    }
}

/// 当缺乏原生 stacktrace 时，从 tags、extra 或 title 提取位置指纹（如 command/route/module）作为兜底 culprit。
fn extract_fallback_culprit(event: &RawEvent, title: &str) -> Option<String> {
    // 1. 从 tags 读取
    if let Some(ref t) = event.tags {
        let cmd = t.get("command").or_else(|| t.get("cmd"));
        let source = t
            .get("source")
            .or_else(|| t.get("module"))
            .or_else(|| t.get("caller"));
        match (source, cmd) {
            (Some(s), Some(c)) => return Some(format!("{} in {}", s, c)),
            (None, Some(c)) => return Some(c.clone()),
            (Some(s), None) => return Some(s.clone()),
            _ => {}
        }
        if let Some(route) = t.get("route").or_else(|| t.get("endpoint")) {
            return Some(route.clone());
        }
    }

    // 2. 从 extra 读取
    if let Some(ref extra) = event.extra {
        if let Some(cmd) = extra.get("command").and_then(|v| v.as_str()) {
            return Some(cmd.to_string());
        }
    }

    // 3. 从 title / message 启发式提取 IPC command: `IPC command "xyz"`
    if let Some(start) = title.find("IPC command \"") {
        let remainder = &title[start + "IPC command \"".len()..];
        if let Some(end) = remainder.find('"') {
            let cmd_name = &remainder[..end];
            if !cmd_name.is_empty() {
                return Some(format!("ipc in {}", cmd_name));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Exception;
    use serde_json::json;

    /// 构造带默认维度的 RawEvent，测试只覆盖关心的字段。
    fn raw_event(platform: &str, environment: Option<&str>) -> RawEvent {
        RawEvent {
            platform: platform.to_string(),
            release: None,
            environment: environment.map(str::to_string),
            message: None,
            exception: None,
            tags: None,
            extra: None,
            breadcrumbs: None,
            event_id: None,
            level: None,
            namespace: None,
            app: None,
            component: None,
            service_name: None,
            error_type: None,
            source_kind: None,
            grafana_url: None,
            loki_query: None,
            timestamp: None,
        }
    }

    fn event_with_tags(tags: serde_json::Value) -> RawEvent {
        let mut event = raw_event("rust", None);
        event.exception = Some(Exception {
            error_type: "GatewayExhaustedError".to_string(),
            value: Some("chat failed".to_string()),
            stacktrace: None,
        });
        event.tags = Some(serde_json::from_value(tags).expect("tags must be a string map"));
        event
    }

    #[test]
    fn error_kind_and_model_always_participate() {
        let quota = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "gemini-3.8-flash",
            "error_kind": "QuotaExhausted"
        })));
        let upstream = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "gemini-3.8-flash",
            "error_kind": "UpstreamUnavailable"
        })));
        assert_ne!(quota.fingerprint, upstream.fingerprint);
    }

    #[test]
    fn model_alias_converges() {
        let prefixed = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "antigravity/gemini-3.8-flash",
            "error_kind": "QuotaExhausted"
        })));
        let bare = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "gemini-3.8-flash",
            "error_kind": "QuotaExhausted"
        })));
        assert_eq!(prefixed.fingerprint, bare.fingerprint);
    }

    #[test]
    fn rate_limit_debug_format_converges() {
        let a = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "muse-spark-1.3-contributor-free[1m]",
            "error_kind": "RateLimitExceeded { retry_after: None }"
        })));
        let b = compute_issue_fingerprint(&event_with_tags(json!({
            "requested_model": "muse-spark-1.3-contributor-free[1m]",
            "error_kind": "RateLimitExceeded { retry_after: Some(7) }"
        })));
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn environment_scopes_fingerprint() {
        let mut dev = event_with_tags(json!({"error_kind": "QuotaExhausted"}));
        dev.environment = Some("dev".to_string());
        let mut prod = event_with_tags(json!({"error_kind": "QuotaExhausted"}));
        prod.environment = Some("production".to_string());

        let a = compute_issue_fingerprint(&dev);
        let b = compute_issue_fingerprint(&prod);
        assert_ne!(a.fingerprint, b.fingerprint, "契约 R6.2");
        assert_eq!(a.environment, "dev");
        assert_eq!(b.environment, "production");
    }

    #[test]
    fn missing_environment_falls_back_to_unknown() {
        let out = compute_issue_fingerprint(&event_with_tags(json!({})));
        assert_eq!(out.environment, "unknown");
    }

    #[test]
    fn top_level_error_type_wins_over_exception() {
        let mut event = event_with_tags(json!({}));
        event.error_type = Some("ValidationError".to_string());
        let out = compute_issue_fingerprint(&event);
        assert!(out.title.starts_with("ValidationError:"));
    }

    #[test]
    fn title_and_project_surfaced() {
        let mut event = raw_event("vue", None);
        event.exception = Some(Exception {
            error_type: "ReferenceError".to_string(),
            value: Some("boom".to_string()),
            stacktrace: Some(Stacktrace::Frames(vec![Frame {
                filename: Some("src/main.ts".to_string()),
                function: Some("render".to_string()),
                lineno: Some(12),
                colno: None,
                in_app: Some(true),
            }])),
        });
        event.tags = Some(std::collections::HashMap::new());
        event.extra = Some(json!({ "project_path": "/home/dev/blog-web" }));

        let out = compute_issue_fingerprint(&event);
        assert_eq!(out.title, "ReferenceError: boom");
        assert_eq!(out.culprit.as_deref(), Some("src/main.ts in render"));
        assert_eq!(out.project.as_deref(), Some("blog-web"));
    }

    #[test]
    fn string_stacktrace_normalized_before_fingerprint() {
        let mut event = raw_event("python", Some("dev"));
        event.exception = Some(Exception {
            error_type: "RuntimeError".to_string(),
            value: Some("boom".to_string()),
            stacktrace: Some(Stacktrace::Text(
                "File \"api_v2/src/x.py\", line 7, in run\nRuntimeError: boom".to_string(),
            )),
        });
        let normalized = crate::parser::normalize_stacktrace(event);
        let out = compute_issue_fingerprint(&normalized);
        assert_eq!(out.culprit.as_deref(), Some("api_v2/src/x.py in run"));
    }

    #[test]
    fn synthesizes_culprit_from_command_or_endpoint_when_stacktrace_absent() {
        let mut event = raw_event("tauri", None);
        let mut tags = std::collections::HashMap::new();
        tags.insert("source".to_string(), "tauri_safe_invoke".to_string());
        tags.insert("command".to_string(), "ask_cancel".to_string());
        event.exception = Some(Exception {
            error_type: "IpcCommandError".to_string(),
            value: Some("command failed".to_string()),
            stacktrace: None,
        });
        event.tags = Some(tags);

        let out = compute_issue_fingerprint(&event);
        assert_eq!(
            out.culprit.as_deref(),
            Some("tauri_safe_invoke in ask_cancel")
        );
    }

    #[test]
    fn synthesizes_culprit_from_title_when_no_tags_available() {
        let mut event = raw_event("rust", None);
        event.exception = Some(Exception {
            error_type: "IpcCommandError".to_string(),
            value: Some(
                "IPC command \"ask_answer\" failed: control request cannot be consumed".to_string(),
            ),
            stacktrace: None,
        });
        let out = compute_issue_fingerprint(&event);
        assert_eq!(out.culprit.as_deref(), Some("ipc in ask_answer"));
        assert_eq!(out.project.as_deref(), None);
    }
}
