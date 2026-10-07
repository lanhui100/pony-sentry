use sha2::{Digest, Sha256};

pub struct FingerprintEngine;

impl FingerprintEngine {
    pub fn compute_fingerprint(
        error_type: &str,
        culprit: Option<&str>,
        platform: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(platform.as_bytes());
        hasher.update(b":");
        hasher.update(error_type.as_bytes());
        if let Some(c) = culprit {
            hasher.update(b":");
            hasher.update(c.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}
