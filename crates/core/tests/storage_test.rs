use pony_sentry_core::{
    create_pool, models::IssueStatus, IssueFilter, IssueRepository, SqliteIssueRepository,
};
use serde_json::json;

#[tokio::test]
async fn test_issue_lifecycle_and_regression_flow() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let fp = "hash_error_null_pointer_001";
    let title = "NullPointerException in worker thread";
    let platform = "rust";

    // 1. 首次上报：状态为 unresolved, count = 1
    let (issue, event) = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.0"),
            Some("production"),
            json!({"trace": "stack info"}),
        )
        .await
        .expect("First report should succeed");

    assert_eq!(issue.status, IssueStatus::Unresolved);
    assert_eq!(issue.count, 1);
    assert_eq!(event.issue_id, issue.id);

    // 2. 重复上报：count = 2
    let (issue2, _) = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.0"),
            Some("production"),
            json!({"trace": "stack info 2"}),
        )
        .await
        .expect("Second report should succeed");

    assert_eq!(issue2.id, issue.id);
    assert_eq!(issue2.count, 2);
    assert_eq!(issue2.status, IssueStatus::Unresolved);

    // 3. Agent 认领并解决：in_progress -> resolved
    let claimed = repo
        .update_issue_status(&issue.id, IssueStatus::InProgress, Some("agent-007".into()))
        .await
        .expect("Claim should succeed");
    assert_eq!(claimed.status, IssueStatus::InProgress);
    assert_eq!(claimed.assigned_to.as_deref(), Some("agent-007"));

    let resolved = repo
        .update_issue_status(&issue.id, IssueStatus::Resolved, None)
        .await
        .expect("Resolve should succeed");
    assert_eq!(resolved.status, IssueStatus::Resolved);

    // 4. 新版本再度出现该错误：自动触发 Regression
    let (regressed, _) = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.1"),
            Some("production"),
            json!({"trace": "stack info 3"}),
        )
        .await
        .expect("Regression report should succeed");

    assert_eq!(regressed.status, IssueStatus::Regression);
    assert_eq!(regressed.count, 3);
    assert_eq!(regressed.last_release.as_deref(), Some("v1.0.1"));

    // 5. 过滤查询
    let list = repo
        .list_issues(IssueFilter {
            status: Some(IssueStatus::Regression),
            ..Default::default()
        })
        .await
        .expect("List issues should succeed");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, issue.id);
}
