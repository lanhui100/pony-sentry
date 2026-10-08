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
    let res = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.0"),
            Some("production"),
            None,
            json!({"trace": "stack info"}),
        )
        .await
        .expect("First report should succeed");

    let issue = res.issue;
    let event = res.event;
    assert!(res.is_new);
    assert!(!res.is_regression_trigger);

    assert_eq!(issue.status, IssueStatus::Unresolved);
    assert_eq!(issue.count, 1);
    assert_eq!(event.issue_id, issue.id);

    // 2. 重复上报：count = 2
    let res2 = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.0"),
            Some("production"),
            None,
            json!({"trace": "stack info 2"}),
        )
        .await
        .expect("Second report should succeed");

    let issue2 = res2.issue;
    assert!(!res2.is_new);
    assert!(!res2.is_regression_trigger);
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

    // 4. 新版本再度出现该错误：自动触发 Regression，且 is_regression_trigger = true
    let res3 = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.1"),
            Some("production"),
            None,
            json!({"trace": "stack info 3"}),
        )
        .await
        .expect("Regression report should succeed");

    let regressed = res3.issue;
    assert!(!res3.is_new);
    assert!(res3.is_regression_trigger, "初次回归必须触发 is_regression_trigger");
    assert_eq!(regressed.status, IssueStatus::Regression);
    assert_eq!(regressed.count, 3);
    assert_eq!(regressed.last_release.as_deref(), Some("v1.0.1"));

    // 4.1 再次上报：此时已处于 Regression，必须 is_regression_trigger = false（防止雪崩）
    let res4 = repo
        .record_event_and_upsert_issue(
            fp,
            title,
            Some("worker::run"),
            platform,
            Some("v1.0.1"),
            Some("production"),
            None,
            json!({"trace": "stack info 4"}),
        )
        .await
        .expect("Follow-up report in regression should succeed");
    assert!(!res4.is_new);
    assert!(!res4.is_regression_trigger, "已在回归状态的后续事件不得再次触发 regression 跃迁");
    assert_eq!(res4.issue.count, 4);

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

/// 项目维度：写入、按项目筛选、候选列表，以及首报归属不被后续上报覆盖。
#[tokio::test]
async fn test_project_dimension_persists_and_filters() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let repo_ref = &repo;
    let report = |fp: &'static str, project: &'static str| async move {
        repo_ref
            .record_event_and_upsert_issue(
                fp,
                "TypeError: cannot read token",
                Some("src/views/Dashboard.vue in mounted"),
                "vue",
                Some("v1.0.0"),
                Some("production"),
                Some(project),
                json!({ "project_path": format!("/home/dev/{project}") }),
            )
            .await
            .expect("Report should succeed")
            .issue
    };

    let shop = report("fp_shop", "shop-web").await;
    let blog = report("fp_blog", "blog-web").await;
    assert_eq!(shop.project.as_deref(), Some("shop-web"));
    assert_eq!(blog.project.as_deref(), Some("blog-web"));

    // 按项目筛选只命中该项目下的 issue
    let only_shop = repo
        .list_issues(IssueFilter {
            project: Some("shop-web".into()),
            ..Default::default()
        })
        .await
        .expect("Filter by project should succeed");
    assert_eq!(only_shop.len(), 1);
    assert_eq!(only_shop[0].id, shop.id);

    // 筛选下拉的候选集合：去重且不含 NULL 项目
    let projects = repo.list_projects().await.expect("List projects");
    assert_eq!(
        projects,
        vec!["blog-web".to_string(), "shop-web".to_string()]
    );

    // 首报归属：同指纹的后续上报不改写 issue 的项目标签
    let still_shop = report("fp_shop", "other-project").await;
    assert_eq!(
        still_shop.project.as_deref(),
        Some("shop-web"),
        "issue 的项目标签必须以首报工作区为准，否则筛选结果会随最后一次上报漂移"
    );

    // 未注入工作区的事件不污染项目候选集合
    let no_workspace = repo
        .record_event_and_upsert_issue(
            "fp_noname",
            "GenericError: boom",
            None,
            "python",
            Some("v1.0.0"),
            Some("production"),
            None,
            json!({}),
        )
        .await
        .expect("Report without workspace should succeed")
        .issue;
    assert_eq!(no_workspace.project, None);
    assert_eq!(
        repo.list_projects().await.expect("List projects"),
        vec!["blog-web".to_string(), "shop-web".to_string()]
    );
}
