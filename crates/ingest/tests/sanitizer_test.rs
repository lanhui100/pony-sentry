use pony_sentry_ingest::{parser::Exception, Frame, RawEvent, SanitizationPipeline};
use serde_json::json;

#[test]
fn test_deep_zero_trust_sanitization() {
    let mut tags = std::collections::HashMap::new();
    tags.insert("api_key".into(), "sk-live-1234567890abcdef".into());
    tags.insert("env".into(), "production".into());

    let event = RawEvent {
        platform: "rust".into(),
        release: Some("1.0.0".into()),
        environment: Some("prod".into()),
        message: Some("Failed to connect with Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.do_not_leak to /home/john_doe/secret.key".into()),
        exception: Some(Exception {
            error_type: "AuthError".into(),
            value: Some("password=super_secret_pass".into()),
            stacktrace: Some(vec![Frame {
                filename: Some("C:\\Users\\alice\\projects\\pony\\src\\main.rs".into()),
                function: Some("login_with_token".into()),
                lineno: Some(42),
                colno: None,
                in_app: Some(true),
            }]),
        }),
        tags: Some(tags),
        extra: Some(json!({
            "user_credentials": {
                "access_token": "secret_access_token_123",
                "normal_field": "hello world from /root/config.toml"
            }
        })),
        breadcrumbs: Some(vec![json!({
            "category": "http",
            "data": {
                "authorization": "Basic YWRtaW46cGFzc3dvcmQ=",
                "url": "https://api.example.com/login"
            }
        })]),
    };

    let sanitized = SanitizationPipeline::sanitize_event(event);

    // 1. Message & Exception 脱敏
    let msg = sanitized.message.unwrap();
    assert!(msg.contains("[REDACTED_SECRET]"));
    assert!(msg.contains("[USER_HOME]/secret.key"));
    assert!(!msg.contains("john_doe"));

    let ex = sanitized.exception.unwrap();
    assert!(ex.value.unwrap().contains("[REDACTED_SECRET]"));
    assert_eq!(
        ex.stacktrace.unwrap()[0].filename.as_deref(),
        Some("[USER_HOME]\\projects\\pony\\src\\main.rs")
    );

    // 2. Tags 脱敏
    let tags_out = sanitized.tags.unwrap();
    assert_eq!(tags_out.get("api_key").unwrap(), "[REDACTED_SECRET]");
    assert_eq!(tags_out.get("env").unwrap(), "production");

    // 3. Extra 递归脱敏
    let extra_out = sanitized.extra.unwrap();
    assert_eq!(
        extra_out["user_credentials"]["access_token"],
        "[REDACTED_SECRET]"
    );
    assert_eq!(
        extra_out["user_credentials"]["normal_field"],
        "hello world from [USER_HOME]/config.toml"
    );

    // 4. Breadcrumbs 递归脱敏
    let crumbs_out = sanitized.breadcrumbs.unwrap();
    assert_eq!(
        crumbs_out[0]["data"]["authorization"],
        "[REDACTED_SECRET]"
    );
}
