use pony_sentry_ingest::{parser::Exception, Frame, RawEvent, SanitizationPipeline};

#[test]
fn test_token_and_path_sanitization() {
    let event = RawEvent {
        platform: "rust".into(),
        release: Some("1.0.0".into()),
        environment: Some("prod".into()),
        message: Some("Failed to connect with Bearer abcdef1234567890 to /home/john_doe/secret.key".into()),
        exception: Some(Exception {
            error_type: "AuthError".into(),
            value: Some("password=super_secret_pass".into()),
            stacktrace: Some(vec![Frame {
                filename: Some("/Users/alice/projects/pony/src/main.rs".into()),
                function: Some("login".into()),
                lineno: Some(42),
                colno: None,
                in_app: Some(true),
            }]),
        }),
        tags: None,
        extra: None,
        breadcrumbs: None,
    };

    let sanitized = SanitizationPipeline::sanitize_event(event);

    let msg = sanitized.message.unwrap();
    assert!(msg.contains("[REDACTED_SECRET]"));
    assert!(msg.contains("[USER_HOME]/secret.key"));
    assert!(!msg.contains("abcdef1234567890"));
    assert!(!msg.contains("john_doe"));

    let ex = sanitized.exception.unwrap();
    let val = ex.value.unwrap();
    assert!(val.contains("[REDACTED_SECRET]"));
    assert!(!val.contains("super_secret_pass"));

    let frame = &ex.stacktrace.unwrap()[0];
    assert_eq!(
        frame.filename.as_deref(),
        Some("[USER_HOME]/projects/pony/src/main.rs")
    );
}
