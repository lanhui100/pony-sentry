//! PonySentry 鉴权矩阵契约级黑盒验收测试（红相）。
//!
//! 断言唯一依据：`.dev-team/contracts-pony-sentry-migration.md` 冻结 v1.1
//! - R10 鉴权矩阵：ingest / 读接口 / PATCH 在无 token 时必须 401
//! - R10.1 CLIENT_TOKEN 与 ADMIN_TOKEN 均未配置时 fail-closed 503
//! - R11.2 `/healthz` 必须报告 db 探活结果

#[path = "harness.rs"]
mod harness;

use harness::{full_v1_ingest_payload, Resp, Server};

fn assert_status(resp: &Resp, expected: u16, contract_clause: &str) {
    assert_eq!(
        resp.status,
        expected,
        "[{contract_clause}] 期望 HTTP {expected}，实际 {}",
        resp.describe()
    );
}

/// R10：无 token 的读接口与 PATCH 必须 401；带 token 必须 2xx 且变更生效。
#[test]
fn r10_admin_surfaces_require_token() {
    // Arrange：先造一条真实 issue 作为 PATCH 目标
    let server = Server::start();
    let payload = full_v1_ingest_payload("evt-contract-auth-0001", "preprod", "PermissionError: 鉴权矩阵");
    let created = server.post_ingest_client(&payload);
    assert_status(&created, 201, "R10 ingest 带 CLIENT_TOKEN 必须 201");
    let issue_id = created.str_field("issue_id");
    let issue_path = format!("/api/v1/issues/{issue_id}");

    // Act 1 / Assert 1（R14.5）：无 token 读 → 401
    let anon_list = server.get_anon("/api/v1/issues");
    assert_status(&anon_list, 401, "R10 无 token GET /api/v1/issues");

    // Act 2 / Assert 2（R14.5）：无 token PATCH → 401
    let anon_patch = server.patch_anon(&issue_path, &serde_json::json!({"status": "resolved"}));
    assert_status(&anon_patch, 401, "R10 无 token PATCH /api/v1/issues/:id");

    // Assert 3（R14.5）：401 的变更 MUST NOT 生效
    let after_anon_patch = server.get_admin(&issue_path);
    assert_status(&after_anon_patch, 200, "R10 带 admin token GET issue");
    assert_eq!(
        after_anon_patch.str_field("status"),
        created.str_field("status"),
        "[R10] 无 token PATCH 不得改变 issue 状态：{}",
        after_anon_patch.describe()
    );

    // Act 3 / Assert 4（R10 矩阵）：CLIENT_TOKEN 与 ADMIN_TOKEN 均可读
    let client_list = server.get_client("/api/v1/issues");
    assert_status(&client_list, 200, "R10 带 CLIENT_TOKEN GET /api/v1/issues");
    let admin_list = server.get_admin("/api/v1/issues");
    assert_status(&admin_list, 200, "R10 带 ADMIN_TOKEN GET /api/v1/issues");

    // Act 4 / Assert 5：带 admin token PATCH → 2xx 且状态生效
    let admin_patch = server.patch_admin(&issue_path, &serde_json::json!({"status": "resolved"}));
    assert!(
        (200..300).contains(&admin_patch.status),
        "[R10] 带 ADMIN_TOKEN PATCH 必须 2xx，实际 {}",
        admin_patch.describe()
    );
    let after_admin_patch = server.get_admin(&issue_path);
    assert_status(&after_admin_patch, 200, "R10 带 admin token GET issue（PATCH 后）");
    assert_eq!(
        after_admin_patch.str_field("status"),
        "resolved",
        "[R10] 带 token PATCH 必须生效：{}",
        after_admin_patch.describe()
    );

    // Act 5 / Assert 6（R10）：无 token ingest → 401
    let anon_ingest = server.post_ingest_anon(&full_v1_ingest_payload(
        "evt-contract-auth-0002",
        "preprod",
        "PermissionError: 匿名 ingest",
    ));
    assert_status(&anon_ingest, 401, "R10 无 token POST /api/v1/ingest");
}

/// R10.1：两个 token 都未配置时，需鉴权的读写必须 fail-closed 503。
#[test]
fn r10_fail_closed_when_no_tokens_configured() {
    // Arrange：不注入 CLIENT_TOKEN / ADMIN_TOKEN
    let server = Server::start_with(&[]);

    // Act
    let list = server.get_anon("/api/v1/issues");

    // Assert（R10.1）
    assert_status(&list, 503, "R10.1 未配置任何 token 时读接口 fail-closed");
    assert_eq!(
        list.str_field("error"),
        "admin_token_not_configured",
        "[R10.1] 503 响应必须携带 error=admin_token_not_configured：{}",
        list.describe()
    );
}

/// R11.2：`/healthz` 必须报告 db 可达性（不只是常量 OK）。
#[test]
fn r11_2_healthz_reports_db_status() {
    // Arrange
    let server = Server::start();

    // Act
    let health = server.healthz();

    // Assert（R11.2 / NFR health_probe）
    assert_status(&health, 200, "R11.2 DB 可达时 /healthz 必须 200");
    assert_eq!(
        health.str_field("status"),
        "ok",
        "[R11.2] /healthz 必须返回 status=ok：{}",
        health.describe()
    );
    let db = health.str_field("db");
    assert!(
        db == "up" || db == "ok",
        "[R11.2] /healthz 必须报告 db 探活结果（up/ok），实际 db={db}：{}",
        health.describe()
    );
}
