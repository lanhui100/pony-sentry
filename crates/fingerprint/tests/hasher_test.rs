use pony_sentry_fingerprint::FingerprintEngine;

#[test]
fn test_fingerprint_stability() {
    let fp1 = FingerprintEngine::compute_fingerprint(
        "PanicException",
        Some("crates/core/src/worker.rs:42:10"),
        "rust",
    );

    let fp2 = FingerprintEngine::compute_fingerprint(
        "PanicException",
        Some("crates/core/src/worker.rs:99:1"),
        "rust",
    );

    // 行号变化不应导致指纹发生变化
    assert_eq!(fp1, fp2);

    let fp_diff = FingerprintEngine::compute_fingerprint(
        "PanicException",
        Some("crates/core/src/other.rs:42:10"),
        "rust",
    );
    assert_ne!(fp1, fp_diff);

    // 当 culprit 为 None 时，支持 discriminator（如 requested_model）进行区分
    let fp_model_a = FingerprintEngine::compute_fingerprint_with_discriminator(
        "GatewayExhaustedError",
        None,
        "rust",
        Some("muse-spark-1.3"),
    );
    let fp_model_b = FingerprintEngine::compute_fingerprint_with_discriminator(
        "GatewayExhaustedError",
        None,
        "rust",
        Some("antigravity/gemini-3.8-flash"),
    );
    assert_ne!(fp_model_a, fp_model_b);
}
