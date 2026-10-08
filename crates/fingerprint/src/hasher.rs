use sha2::{Digest, Sha256};

/// 组件间的分隔符：0x1F (Unit Separator)。
/// 各组件在拼接前已规范化且不含该字节，可安全区分边界。
const SEP: u8 = 0x1F;

pub struct FingerprintEngine;

impl FingerprintEngine {
    /// 兼容旧调用：仅 platform + error_type + culprit（无 error_kind/model 判别器）。
    /// 新代码应使用 [compute_fingerprint_v2]。
    pub fn compute_fingerprint(error_type: &str, culprit: Option<&str>, platform: &str) -> String {
        Self::compute_fingerprint_with_discriminator(error_type, culprit, platform, None)
    }

    /// 兼容旧调用：culprit 为 None 时才使用 discriminator。
    /// 新代码应使用 [compute_fingerprint_v2]。
    pub fn compute_fingerprint_with_discriminator(
        error_type: &str,
        culprit: Option<&str>,
        platform: &str,
        discriminator: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(platform.trim().to_lowercase().as_bytes());
        hasher.update([SEP]);
        hasher.update(error_type.trim().as_bytes());
        if let Some(c) = culprit {
            // 清理行号与列号等抖动信息，只保留文件名与函数名
            let clean_culprit = Self::clean_culprit(c);
            hasher.update([SEP]);
            hasher.update(clean_culprit.as_bytes());
        } else if let Some(disc) = discriminator {
            let clean_disc = disc.trim();
            if !clean_disc.is_empty() {
                hasher.update([SEP]);
                hasher.update(clean_disc.as_bytes());
            }
        }
        hex::encode(hasher.finalize())
    }

    /// v2 指纹：五元组 `platform : error_type : error_kind : model : culprit`。
    ///
    /// 相比旧方案的两处关键修复：
    /// 1. **判别器无条件参与**：error_kind 与 model 不再受 culprit 有无影响——
    ///    带/不带堆栈的上报不会再因判别器被忽略而拆成两只 issue；
    /// 2. **error_kind / model 规范化后入指纹**：同一模型的 QuotaExhausted /
    ///    UpstreamUnavailable 等不同失败原因正确拆分；`RateLimitExceeded { retry_after: ... }`
    ///    这类含抖动字段的 Rust Debug 串收敛为变体名；`provider/model` 别名归一为 `model`。
    ///
    /// 各组件可选（None/空串则跳过），顺序固定，组件间以 0x1F 分隔。
    pub fn compute_fingerprint_v2(
        platform: &str,
        error_type: &str,
        error_kind: Option<&str>,
        model: Option<&str>,
        culprit: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(platform.trim().to_lowercase().as_bytes());
        hasher.update([SEP]);
        hasher.update(error_type.trim().as_bytes());

        if let Some(kind) = Self::normalize_error_kind(error_kind) {
            hasher.update([SEP]);
            hasher.update(kind.as_bytes());
        }
        if let Some(model) = Self::normalize_model(model) {
            hasher.update([SEP]);
            hasher.update(model.as_bytes());
        }
        if let Some(c) = culprit {
            let clean_culprit = Self::clean_culprit(c);
            if !clean_culprit.is_empty() {
                hasher.update([SEP]);
                hasher.update(clean_culprit.as_bytes());
            }
        }
        hex::encode(hasher.finalize())
    }

    /// 规范化 error_kind：`RateLimitExceeded { retry_after: None }` → `RateLimitExceeded`；
    /// 只保留 Rust 枚举变体名，剥掉 `{ ... }` 中的抖动字段。
    pub fn normalize_error_kind(error_kind: Option<&str>) -> Option<String> {
        let raw = error_kind?.trim();
        if raw.is_empty() {
            return None;
        }
        let variant = raw.split('{').next().unwrap_or(raw).trim();
        if variant.is_empty() {
            None
        } else {
            Some(variant.to_string())
        }
    }

    /// 规范化模型名：剥掉 provider 前缀（`antigravity/gemini-3.8-flash` → `gemini-3.8-flash`），
    /// 收敛客户端在 requested_model 上报时带/不带 provider 前缀导致的别名拆分。
    pub fn normalize_model(model: Option<&str>) -> Option<String> {
        let raw = model?.trim();
        if raw.is_empty() {
            return None;
        }
        let name = raw.rsplit('/').next().unwrap_or(raw).trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    fn clean_culprit(culprit: &str) -> String {
        // 去除常见的 :123:45 形式的行号抖动
        if let Some((base, _)) = culprit.rsplit_once(':') {
            if base.rsplit_once(':').is_some() {
                // 如果有两个冒号（如 file.rs:12:34），剥离后两截
                if let Some((f, _)) = base.rsplit_once(':') {
                    return f.to_string();
                }
            }
            return base.to_string();
        }
        culprit.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_line_number_noise_does_not_change_fingerprint() {
        let a = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "PanicException",
            None,
            None,
            Some("crates/core/src/worker.rs:42:10"),
        );
        let b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "PanicException",
            None,
            None,
            Some("crates/core/src/worker.rs:99:1"),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn v2_different_file_changes_fingerprint() {
        let a = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "PanicException",
            None,
            None,
            Some("crates/core/src/worker.rs:42:10"),
        );
        let b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "PanicException",
            None,
            None,
            Some("crates/core/src/other.rs:42:10"),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn v2_error_kind_differentiates_failure_reasons() {
        let quota = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("QuotaExhausted"),
            Some("gemini-3.8-flash"),
            None,
        );
        let upstream = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("UpstreamUnavailable"),
            Some("gemini-3.8-flash"),
            None,
        );
        assert_ne!(quota, upstream);
    }

    #[test]
    fn v2_rate_limit_debug_format_converges_to_variant_name() {
        let a = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("RateLimitExceeded { retry_after: None }"),
            Some("muse-spark-1.3-contributor-free[1m]"),
            None,
        );
        let b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("RateLimitExceeded { retry_after: Some(5) }"),
            Some("muse-spark-1.3-contributor-free[1m]"),
            None,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn v2_provider_prefix_model_alias_converges() {
        let prefixed = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("QuotaExhausted"),
            Some("antigravity/gemini-3.8-flash"),
            None,
        );
        let bare = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("QuotaExhausted"),
            Some("gemini-3.8-flash"),
            None,
        );
        assert_eq!(prefixed, bare);
    }

    #[test]
    fn v2_discriminator_participates_even_with_culprit() {
        // 旧方案：culprit 存在时 model 被忽略 → 不同模型同栈帧会误并。
        // v2：model 无条件参与。
        let model_a = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("QuotaExhausted"),
            Some("gemini-3.8-flash"),
            Some("crates/ponyllm-server/src/streaming.rs in emit_failure"),
        );
        let model_b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GatewayExhaustedError",
            Some("QuotaExhausted"),
            Some("deepseek-v4-flash"),
            Some("crates/ponyllm-server/src/streaming.rs in emit_failure"),
        );
        assert_ne!(model_a, model_b);
    }

    #[test]
    fn v2_platform_case_insensitive() {
        let a = FingerprintEngine::compute_fingerprint_v2(
            "Rust",
            "PanicException",
            None,
            None,
            Some("a.rs:1:1"),
        );
        let b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "PanicException",
            None,
            None,
            Some("a.rs:2:2"),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn v2_missing_optional_components_stable() {
        let a = FingerprintEngine::compute_fingerprint_v2("rust", "GenericError", None, None, None);
        let b = FingerprintEngine::compute_fingerprint_v2(
            "rust",
            "GenericError",
            Some(""),
            Some("  "),
            None,
        );
        assert_eq!(a, b);
    }
}
