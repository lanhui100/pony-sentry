//! 读侧 `trace_id` 透出的数据面契约：`IssueRepository::latest_event_payloads`。
//!
//! 「读取时从已存 payload 派生」是本次 trace_id 透出的唯一真源，因此必须机械可查：
//! ①同 issue 多事件 → 只取最近一条；②从未上报事件的 issue id → 不出现；
//! ③空入参短路；④两种方言（SQLite / PG）同构。
//!
//! PG 部分需要 `PONY_TEST_PG_URL` 指向**可写的隔离测试库**；未设置时打印 `SKIP` 并返回
//! （`scripts/test-all.sh` 会因出现 `SKIP` 而整体非零退出，避免假绿）。

use chrono::{DateTime, TimeZone, Utc};
use pony_sentry_core::{
    create_pool, IssueRepository, NewEventRecord, PgIssueRepository, SqliteIssueRepository,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn record<'a>(
    fingerprint: &'a str,
    payload: Value,
    occurred_at: DateTime<Utc>,
) -> NewEventRecord<'a> {
    NewEventRecord {
        fingerprint,
        title: "TypeError: trace_id read",
        culprit: None,
        platform: "python",
        release: None,
        environment: Some("production"),
        project: None,
        app: None,
        namespace: None,
        component: None,
        level: "error",
        source_kind: None,
        event_key: None,
        payload,
        occurred_at: Some(occurred_at),
    }
}

/// 两种方言共用的断言集（只依赖 `&dyn IssueRepository`，避免方言各写一套语义）。
async fn assert_latest_payloads_contract(repo: &dyn IssueRepository) {
    let t0 = Utc.with_ymd_and_hms(2026, 10, 27, 1, 0, 0).unwrap();
    let t1 = t0 + chrono::Duration::seconds(60);

    // issue A：两条事件，后一条带一级 `trace_id`（新形态）
    let first = repo
        .record_event_and_upsert_issue(record(
            "fp-read-a",
            json!({"extra": {"trace_id": "aaaa1111aaaa1111aaaa1111aaaa1111"}}),
            t0,
        ))
        .await
        .expect("首次写入 issue A 失败");
    let second = repo
        .record_event_and_upsert_issue(record(
            "fp-read-a",
            json!({"trace_id": "bbbb2222bbbb2222bbbb2222bbbb2222"}),
            t1,
        ))
        .await
        .expect("第二条事件写入 issue A 失败");
    assert_eq!(
        first.issue.id, second.issue.id,
        "同指纹必须归并到同一 issue（否则本用例的「取最新」语义无意义）"
    );

    // issue B：只有一条历史形态事件（`extra.trace_id`）
    let other = repo
        .record_event_and_upsert_issue(record(
            "fp-read-b",
            json!({"extra": {"trace_id": "cccc3333cccc3333cccc3333cccc3333"}}),
            t0,
        ))
        .await
        .expect("写入 issue B 失败");

    let ids = vec![
        first.issue.id.clone(),
        other.issue.id.clone(),
        "ghost-issue-never-ingested".to_string(),
    ];
    let payloads = repo
        .latest_event_payloads(&ids)
        .await
        .expect("批量读取最近事件载荷失败");
    let by_issue: BTreeMap<String, Value> = payloads.into_iter().collect();

    assert_eq!(
        by_issue.len(),
        2,
        "只应返回真实有事件的 issue（幽灵 id 不得出现）：{:?}",
        by_issue.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        by_issue[&first.issue.id]["trace_id"],
        json!("bbbb2222bbbb2222bbbb2222bbbb2222"),
        "必须取该 issue 最近一条事件的载荷"
    );
    assert_eq!(
        by_issue[&other.issue.id]["extra"]["trace_id"],
        json!("cccc3333cccc3333cccc3333cccc3333"),
        "历史形态（extra.trace_id）必须原样可取"
    );
    assert!(
        !by_issue.contains_key("ghost-issue-never-ingested"),
        "不存在的 issue id 不得出现在结果里"
    );

    assert!(
        repo.latest_event_payloads(&[])
            .await
            .expect("空入参不得报错")
            .is_empty(),
        "空入参必须短路为空集（不得拼出非法 SQL）"
    );
}

#[tokio::test]
async fn latest_event_payloads_on_sqlite() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.unwrap();
    assert_latest_payloads_contract(&repo).await;
}

/// PG 用例：每个用例独占 schema，避免并行用例互相污染（与 `pg_repository_test.rs` 同策略）。
#[tokio::test]
async fn latest_event_payloads_on_pg() {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let Ok(url) = std::env::var("PONY_TEST_PG_URL") else {
        eprintln!("SKIP: PONY_TEST_PG_URL 未设置，跳过 PG 用例（latest_event_payloads_on_pg）");
        return;
    };
    if url.trim().is_empty() {
        eprintln!("SKIP: PONY_TEST_PG_URL 为空，跳过 PG 用例（latest_event_payloads_on_pg）");
        return;
    }
    let schema = "ponysentry_test_trace_id_read";
    let base: PgConnectOptions = url.parse().expect("PONY_TEST_PG_URL 不是合法 PG 连接串");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(base.clone())
        .await
        .expect("连接 PONY_TEST_PG_URL 失败");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin_pool)
        .await
        .expect("DROP SCHEMA 失败");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin_pool)
        .await
        .expect("CREATE SCHEMA 失败");
    let scoped = base.options([("search_path", schema.to_string())]);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(scoped)
        .await
        .expect("连接隔离 schema 失败");
    let repo = PgIssueRepository::new(pool);
    repo.migrate().await.expect("migrate 失败");
    assert_latest_payloads_contract(&repo).await;
}
