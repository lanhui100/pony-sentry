use crate::parser::RawEvent;
use pony_sentry_core::reindex::FingerprintOut;
use pony_sentry_fingerprint::FingerprintEngine;

/// 由上报事件计算 v2 指纹及其派生字段（title / culprit / project）。
///
/// 这是**唯一**的指纹真相源：实时上报（routes.rs）与全量重索引（reindex）
/// 都调用本函数，保证两套路径产出的指纹完全一致，避免“重索引后新事件
/// 落不进正确 issue”的二次漂移。
pub fn compute_issue_fingerprint(event: &RawEvent) -> FingerprintOut {
    // 1. 提取核心信息：error_type / culprit（栈顶帧或兜底）/ title
    let (error_type, mut culprit, title) = if let Some(ref ex) = event.exception {
        let cul = ex.stacktrace.as_ref().and_then(|frames| {
            frames
                .first()
                .and_then(|f| match (&f.filename, &f.function) {
                    (Some(file), Some(func)) => Some(format!("{} in {}", file, func)),
                    (Some(file), None) => Some(file.clone()),
                    (None, Some(func)) => Some(func.clone()),
                    _ => None,
                })
        });
        (
            ex.error_type.clone(),
            cul,
            format!("{}: {}", ex.error_type, ex.value.as_deref().unwrap_or("")),
        )
    } else {
        (
            "GenericError".to_string(),
            None,
            event
                .message
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string()),
        )
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

    // 3. v2 指纹：platform : error_type : error_kind : model : culprit
    let fingerprint = FingerprintEngine::compute_fingerprint_v2(
        &event.platform,
        &error_type,
        error_kind.as_deref(),
        model.as_deref(),
        culprit.as_deref(),
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
    }
}

/// 当缺乏原生 stacktrace 时，从 tags、extra 或 title 提取位置指纹（如 command/route/module）作为兜底 culprit。
fn extract_fallback_culprit(event: &RawEvent, title: &str) -> Option<String> {
    // 1. 从 tags 读取
    if let Some(ref t) = event.tags {
        let cmd = t.get("command").or_else(|| t.get("cmd"));
        let source = t.get("source").or_else(|| t.get("module")).or_else(|| t.get("caller"));
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

    fn event_with_tags(tags: serde_json::Value) -> RawEvent {
        RawEvent {
            platform: "rust".to_string(),
            release: None,
            environment: None,
            message: None,
            exception: Some(Exception {
                error_type: "GatewayExhaustedError".to_string(),
                value: Some("chat failed".to_string()),
                stacktrace: None,
            }),
            tags: Some(serde_json::from_value(tags).expect("tags must be a string map")),
            extra: None,
            breadcrumbs: None,
        }
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
    fn title_and_project_surfaced() {
        let out = compute_issue_fingerprint(&RawEvent {
            platform: "vue".to_string(),
            release: None,
            environment: None,
            message: None,
            exception: Some(Exception {
                error_type: "ReferenceError".to_string(),
                value: Some("boom".to_string()),
                stacktrace: Some(vec![crate::parser::Frame {
                    filename: Some("src/main.ts".to_string()),
                    function: Some("render".to_string()),
                    lineno: Some(12),
                    colno: None,
                    in_app: Some(true),
                }]),
            }),
            tags: Some(std::collections::HashMap::new()),
            extra: Some(json!({ "project_path": "/home/dev/blog-web" })),
            breadcrumbs: None,
        });
        assert_eq!(out.title, "ReferenceError: boom");
        assert_eq!(out.culprit.as_deref(), Some("src/main.ts in render"));
        assert_eq!(out.project.as_deref(), Some("blog-web"));
    }

    #[test]
    fn synthesizes_culprit_from_command_or_endpoint_when_stacktrace_absent() {
        let mut tags = std::collections::HashMap::new();
        tags.insert("source".to_string(), "tauri_safe_invoke".to_string());
        tags.insert("command".to_string(), "ask_cancel".to_string());

        let out = compute_issue_fingerprint(&RawEvent {
            platform: "tauri".to_string(),
            release: None,
            environment: None,
            message: None,
            exception: Some(Exception {
                error_type: "IpcCommandError".to_string(),
                value: Some("command failed".to_string()),
                stacktrace: None,
            }),
            tags: Some(tags),
            extra: None,
            breadcrumbs: None,
        });

        assert_eq!(out.culprit.as_deref(), Some("tauri_safe_invoke in ask_cancel"));
    }

    #[test]
    fn synthesizes_culprit_from_title_when_no_tags_available() {
        let out = compute_issue_fingerprint(&RawEvent {
            platform: "rust".to_string(),
            release: None,
            environment: None,
            message: None,
            exception: Some(Exception {
                error_type: "IpcCommandError".to_string(),
                value: Some("IPC command \"ask_answer\" failed: control request cannot be consumed".to_string()),
                stacktrace: None,
            }),
            tags: None,
            extra: None,
            breadcrumbs: None,
        });

        assert_eq!(out.culprit.as_deref(), Some("ipc in ask_answer"));
        assert_eq!(out.project.as_deref(), Some("job_copilot"));
    }
}
