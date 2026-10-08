use sha2::{Digest, Sha256};

pub struct FingerprintEngine;

impl FingerprintEngine {
    pub fn compute_fingerprint(error_type: &str, culprit: Option<&str>, platform: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(platform.trim().to_lowercase().as_bytes());
        hasher.update(b":");
        hasher.update(error_type.trim().as_bytes());
        if let Some(c) = culprit {
            // 清理行号与列号等抖动信息，只保留文件名与函数名
            let clean_culprit = Self::clean_culprit(c);
            hasher.update(b":");
            hasher.update(clean_culprit.as_bytes());
        }
        hex::encode(hasher.finalize())
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
