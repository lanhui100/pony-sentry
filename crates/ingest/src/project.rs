use serde_json::Value;

/// `extra.project_path` 的契约键名：上报端注入的工作区绝对路径。
const PROJECT_PATH_KEY: &str = "project_path";

/// 从上报事件推断项目名称（支持显式标签、工作区路径、以及常见服务指纹特征推断）。
pub fn infer_project_name(
    tags: Option<&std::collections::HashMap<String, String>>,
    extra: Option<&Value>,
    title: Option<&str>,
    culprit: Option<&str>,
) -> Option<String> {
    // 1. 优先读取 tags.project
    if let Some(t) = tags {
        if let Some(p) = t.get("project") {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 2. 其次从 extra 中读取显式 project / project_name / project_path
    if let Some(p) = project_name_from_extra(extra) {
        return Some(p);
    }

    // 3. 基于错误特征指纹推导项目：
    // 若标题或 culprit 包含明显网关特征（GatewayExhaustedError, AuthInvalid, GatewayConnectionError）
    // 且带 /v1/chat/completions 或 /v1/models 路由，则归属于网关项目 "ponyllm"
    let title_str = title.unwrap_or("");
    let culprit_str = culprit.unwrap_or("");

    if title_str.contains("GatewayExhaustedError")
        || title_str.contains("GatewayConnectionError")
        || title_str.contains("AuthInvalid")
        || culprit_str.contains("ponyllm")
    {
        return Some("ponyllm".to_string());
    }

    None
}

/// 上报事件的工作区 → 项目名（路径末段）。
///
/// 之所以取「末段」而非整条路径：脱敏管道会把用户目录前缀替换成 `[USER_HOME]`
/// （见 `SanitizationPipeline::sanitize_path`），前缀本就不可读也不可用；
/// 而末段（项目目录名）在脱敏后仍然完整保留，既是用户口中的「项目名称」，
/// 也让 Web 端的展示与筛选不必依赖会随机器而变的绝对路径。
///
/// 返回 `None` 的情形：未注入 `extra.project_path`、值非字符串、或末段为空
/// （例如工作区就是根目录 `/`）。
pub fn project_name_from_extra(extra: Option<&Value>) -> Option<String> {
    // 1. 优先读取显式 project / project_name
    if let Some(extra_val) = extra {
        if let Some(p) = extra_val
            .get("project")
            .or_else(|| extra_val.get("project_name"))
            .and_then(|v| v.as_str())
        {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    let path = extra?.get(PROJECT_PATH_KEY)?.as_str()?.trim();
    if path.is_empty() {
        return None;
    }

    // 同时兼容 POSIX 的 `/` 与 Windows 的 `\`，并剥掉尾部与首部分隔符。
    let name = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .filter(|seg| *seg != "[USER_HOME]")?;

    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_explicit_project_or_project_name() {
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project": "ponyllm" }))).as_deref(),
            Some("ponyllm")
        );
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_name": "job_copilot" }))).as_deref(),
            Some("job_copilot")
        );
    }

    #[test]
    fn extracts_basename_of_posix_workspace() {
        let extra = json!({ "project_path": "/home/dm/pony-sentry" });
        assert_eq!(
            project_name_from_extra(Some(&extra)).as_deref(),
            Some("pony-sentry")
        );
    }

    #[test]
    fn survives_sanitization_masking_the_home_prefix() {
        let extra = json!({ "project_path": "[USER_HOME]/pony-sentry" });
        assert_eq!(
            project_name_from_extra(Some(&extra)).as_deref(),
            Some("pony-sentry")
        );
    }

    #[test]
    fn handles_trailing_separators_and_windows_paths() {
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": "/srv/app/" }))).as_deref(),
            Some("app")
        );
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": "C:\\Users\\dm\\my-app\\" })))
                .as_deref(),
            Some("my-app")
        );
    }

    #[test]
    fn returns_none_when_workspace_is_absent_or_degenerate() {
        assert_eq!(project_name_from_extra(None), None);
        assert_eq!(project_name_from_extra(Some(&json!({}))), None);
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": 42 }))),
            None
        );
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": "" }))),
            None
        );
        // 工作区就是根目录，没有可用的项目名
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": "/" }))),
            None
        );
        // 脱敏把整条路径吃光时，不可回退成占位符
        assert_eq!(
            project_name_from_extra(Some(&json!({ "project_path": "[USER_HOME]" }))),
            None
        );
    }
}
