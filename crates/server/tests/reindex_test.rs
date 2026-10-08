use pony_sentry_core::{
    create_pool, reindex::FingerprintOut, IssueFilter, IssueRepository, SqliteIssueRepository,
};
use serde_json::json;

async fn raw_report(repo: &SqliteIssueRepository, fp: &str, model: &str, kind: &str) {
    repo.record_event_and_upsert_issue(
        fp,
        "dummy-title",
        None,
        "rust",
        Some("0.2.49"),
        None,
        None,
        json!({
            "platform": "rust",
            "release": "0.2.49",
            "exception": {
                "error_type": "GatewayExhaustedError",
                "value": format!("Chat completions failed for model '{model}'"),
                "stacktrace": null
            },
            "tags": {
                "requested_model": model,
                "error_kind": kind
            },
            "extra": {}
        }),
    )
    .await
    .expect("Report should succeed");
}

/// 端到端重索引：模拟线上脏数据（同一 issue 混装多模型 + 旧指纹方案），
/// 用真实 compute_issue_fingerprint 闭包跑一遍 reindex，
/// 断言拆分、合并、复用与删除符合规划，且重复执行幂等。
#[tokio::test]
async fn test_reindex_fingerprints_end_to_end() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    // 与线上真实事件同构的指纹计算闭包
    let fingerprint_of = |payload: &serde_json::Value| -> Option<FingerprintOut> {
        let event: pony_sentry_ingest::RawEvent = serde_json::from_value(payload.clone()).ok()?;
        Some(pony_sentry_ingest::compute_issue_fingerprint(&event))
    };

    // 4 条事件以不同旧指纹写入（模拟历史拆分错误）：
    // p1 与 p3 属于同一模型同一失败原因（别名归一后应合并），p2 是不同模型（保持独立）。
    // p4 独立 issue（应 1:1 复用）。
    raw_report(
        &repo,
        "old-fp-1",
        "antigravity/gemini-3.8-flash",
        "QuotaExhausted",
    )
    .await;
    raw_report(
        &repo,
        "old-fp-2",
        "muse-spark-1.3-contributor-free[1m]",
        "RateLimitExceeded { retry_after: None }",
    )
    .await;
    raw_report(&repo, "old-fp-3", "gemini-3.8-flash", "QuotaExhausted").await;
    raw_report(
        &repo,
        "old-fp-4",
        "deepseek-v4-flash",
        "UpstreamUnavailable",
    )
    .await;

    // 第二轮上报与 p1 同组的内容（验证事件迁移后计数合并；该事件初始落在第 5 只旧 issue）
    raw_report(
        &repo,
        "old-fp-2b",
        "antigravity/gemini-3.8-flash",
        "QuotaExhausted",
    )
    .await;

    // 第一遍重索引
    let summary = repo
        .reindex_fingerprints(&fingerprint_of)
        .await
        .expect("Reindex should succeed");
    // 5 只旧 issue → 3 组：
    //   gemini 组合并（p1 + p3 + 第二轮，来自 3 只旧 issue）→ 新建 1
    //   muse 组（p2）→ 1:1 复用 1
    //   deepseek 组（p4）→ 1:1 复用 1
    //   old-fp-1 / old-fp-3 / old-fp-2b 三只旧 issue 删除
    assert_eq!(summary.issues_created, 1);
    assert_eq!(summary.issues_reused, 2);
    assert_eq!(summary.issues_deleted, 3);
    assert_eq!(summary.events_moved, 5);

    let all = repo
        .list_issues(IssueFilter::default())
        .await
        .expect("List issues");
    assert_eq!(all.len(), 3, "合并后应剩 3 只 issue");
    let mut counts: Vec<i64> = all.iter().map(|i| i.count).collect();
    counts.sort_unstable();
    assert_eq!(counts, vec![1, 1, 3], "gemini 组合并计数 3，另两组各 1");

    // 第二遍重索引幂等：不新建、不删除、归属不变
    let again = repo
        .reindex_fingerprints(&fingerprint_of)
        .await
        .expect("Second reindex should succeed");
    assert_eq!(again.issues_created, 0, "幂等：第二次不应新建 issue");
    assert_eq!(again.issues_reused, 3);
    assert_eq!(again.issues_deleted, 0);
    assert_eq!(again.events_moved, 5);
    let after = repo
        .list_issues(IssueFilter::default())
        .await
        .expect("List issues");
    assert_eq!(after.len(), 3);
    let mut counts2: Vec<i64> = after.iter().map(|i| i.count).collect();
    counts2.sort_unstable();
    assert_eq!(counts2, vec![1, 1, 3]);
}
