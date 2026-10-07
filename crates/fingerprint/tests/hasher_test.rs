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
}
