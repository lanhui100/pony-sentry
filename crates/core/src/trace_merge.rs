//! Trace payload 增量合并（v2 兼容层）。
//!
//! 语义**唯一来源**是 [`crate::trace_v2`]（契约 `.dev-team/trace-fix-contract.md` C2）：
//! 终态优先（禁止回退）→ 字段级深合并 → `tool_calls` 按 `tool_call_id` 合并。
//! 本模块只保留「payload → payload」的纯函数入口以兼容既有调用方，
//! **不再保留旧的"重复轮次以旧为准"语义**（W-01 已修）。
//!
//! 存储层（`record_trace`）以 `trace_turns` 表为权威轮次存储，运行时不再调用本函数；
//! 调序与写路径一致：`aggregate_session(&merged_turns, &merge_top_level(old, new))`。

use serde_json::Value;

/// 合并新旧 trace payload（v2 语义），返回合并后的 payload。
///
/// 轮次：按 `turn_id`（缺失时按整对象哈希的 `turn_srv_*`）对齐，旧轮保留原序、同 id 走
/// [`crate::trace_v2::merge_turn`]、新 id 追加；顶层：`extra`/`tags` 深合并、移除 `eval_status`、
/// `total_*` 一律按 turns 求和（单一真相）。
pub fn merge_trace_payloads(old: &Value, new: &Value) -> Value {
    let mut merged_turns = turns_of(old);
    for turn in turns_of(new) {
        let key = crate::trace_v2::turn_key_of(&turn);
        match merged_turns
            .iter()
            .position(|existing| crate::trace_v2::turn_key_of(existing) == key)
        {
            Some(index) => {
                merged_turns[index] = crate::trace_v2::merge_turn(&merged_turns[index], &turn);
            }
            None => merged_turns.push(turn),
        }
    }
    let top = crate::trace_v2::merge_top_level(old, new);
    crate::trace_v2::aggregate_session(&merged_turns, &top)
}

fn turns_of(payload: &Value) -> Vec<Value> {
    payload
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_turn_id_is_merged_not_duplicated_and_new_ids_are_appended() {
        let old = json!({
            "turns": [
                {"turn_id": "t1", "status": "succeeded", "output": "a"},
                {"turn_id": "t2", "status": "running", "output": "b"}
            ]
        });
        let new = json!({
            "turns": [
                {"turn_id": "t2", "status": "failed", "error": {"message": "boom"}},
                {"turn_id": "t3", "status": "succeeded"}
            ]
        });
        let merged = merge_trace_payloads(&old, &new);
        let turns = merged["turns"].as_array().expect("turns array");
        let ids: Vec<&str> = turns
            .iter()
            .map(|t| t["turn_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"], "同 id 合并、新 id 追加");
        // C2.1：running → failed 必须由终态覆盖，且迟到的 error 保留
        assert_eq!(turns[1]["status"], "failed");
        assert_eq!(turns[1]["error"]["message"], "boom");
        assert_eq!(turns[1]["output"], "b", "字段级合并不得丢掉旧字段");
    }

    #[test]
    fn merge_without_turn_id_aligns_by_content_hash() {
        let old = json!({ "turns": [{ "step": "a" }, { "step": "b" }] });
        let new = json!({ "turns": [{ "step": "b" }, { "step": "c" }] });
        let merged = merge_trace_payloads(&old, &new);
        let turns = merged["turns"].as_array().expect("turns array");
        let steps: Vec<&str> = turns.iter().map(|t| t["step"].as_str().unwrap()).collect();
        assert_eq!(steps, vec!["a", "b", "c"], "无 turn_id 时按内容哈希对齐");
    }

    #[test]
    fn new_payload_without_turns_keeps_old_turns() {
        let old = json!({ "turns": [{ "turn_id": "t1" }], "tags": { "k": "v" } });
        let new = json!({ "tags": { "k": "v2" } });
        let merged = merge_trace_payloads(&old, &new);
        assert_eq!(merged["tags"]["k"], "v2");
        assert_eq!(
            merged["turns"][0]["turn_id"], "t1",
            "无轮次的新上报不得吞掉旧轮"
        );
    }

    #[test]
    fn old_payload_without_turns_takes_new() {
        let old = json!({ "foo": 1 });
        let new = json!({ "turns": [{ "turn_id": "t1" }] });
        let merged = merge_trace_payloads(&old, &new);
        assert_eq!(merged["turns"][0]["turn_id"], "t1");
    }

    #[test]
    fn totals_are_recomputed_from_turns_and_eval_status_is_stripped() {
        let old = json!({
            "eval_status": "triage_good",
            "turns": [{"turn_id": "t1", "status": "succeeded", "input_tokens": 10}]
        });
        let new = json!({
            "total_input_tokens": 99999,
            "turns": [{"turn_id": "t2", "status": "failed", "input_tokens": 20}]
        });
        let merged = merge_trace_payloads(&old, &new);
        assert_eq!(
            merged["total_input_tokens"], 30,
            "total 必须按 turns 求和（拒绝客户端直传值）"
        );
        assert!(
            merged.get("eval_status").is_none(),
            "payload.eval_status 必须移除"
        );
    }
}
