//! Trace payload 增量合并：同 session_id 的二次上报按轮次去重合并。
//!
//! 契约（T2 决策）：旧轮保留、新轮按序追加；重复轮次以旧为准。
//! - 双方都有 `turns` 数组 → 按 `turn_id`（缺失时按整对象 JSON 序列化键）去重合并；
//! - 仅旧 payload 有 `turns` → 保留旧轮，防止无轮次更新覆盖已入库轮次；
//! - 其余情况以新 payload 为准（含仅新 payload 有 `turns` 的场景）。

use serde_json::Value;
use std::collections::HashSet;

/// 合并新旧 trace payload，返回写回存储的合并结果。
pub fn merge_trace_payloads(old: &Value, new: &Value) -> Value {
    let mut merged = new.clone();
    match (
        old.get("turns").and_then(|t| t.as_array()),
        new.get("turns").and_then(|t| t.as_array()),
    ) {
        (Some(old_turns), Some(new_turns)) => {
            merged["turns"] = Value::Array(merge_turns(old_turns, new_turns));
        }
        (Some(old_turns), None) => {
            // 新上报未携带轮次：保留旧轮，避免增量覆盖吞掉历史。
            merged["turns"] = Value::Array(old_turns.clone());
        }
        _ => {}
    }
    merged
}

/// 轮次去重键：优先 `turn_id`，缺失时退化为整对象 JSON 序列化键。
fn turn_key(turn: &Value) -> String {
    match turn.get("turn_id").and_then(|v| v.as_str()) {
        Some(id) => format!("turn_id:{id}"),
        None => format!("json:{}", serde_json::to_string(turn).unwrap_or_default()),
    }
}

fn merge_turns(old: &[Value], new: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(old.len() + new.len());
    for t in old {
        if seen.insert(turn_key(t)) {
            merged.push(t.clone());
        }
    }
    for t in new {
        if seen.insert(turn_key(t)) {
            merged.push(t.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_appends_new_turns_after_old_and_keeps_old_duplicates() {
        let old = json!({ "turns": [{ "turn_id": "t1" }, { "turn_id": "t2" }] });
        let new = json!({ "turns": [{ "turn_id": "t2" }, { "turn_id": "t3" }] });
        let merged = merge_trace_payloads(&old, &new);
        let turns = merged["turns"].as_array().expect("turns array");
        let ids: Vec<&str> = turns
            .iter()
            .map(|t| t["turn_id"].as_str().unwrap())
            .collect();
        // t2 以旧为准保留原位，t3 追加；旧轮不丢、新轮按序追加
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn merge_falls_back_to_json_key_without_turn_id() {
        let old = json!({ "turns": [{ "step": "a" }, { "step": "b" }] });
        let new = json!({ "turns": [{ "step": "b" }, { "step": "c" }] });
        let merged = merge_trace_payloads(&old, &new);
        let turns = merged["turns"].as_array().expect("turns array");
        let steps: Vec<&str> = turns
            .iter()
            .map(|t| t["step"].as_str().unwrap())
            .collect();
        assert_eq!(steps, vec!["a", "b", "c"]);
    }

    #[test]
    fn new_payload_without_turns_keeps_old_turns() {
        let old = json!({ "turns": [{ "turn_id": "t1" }], "tags": { "k": "v" } });
        let new = json!({ "tags": { "k": "v2" } });
        let merged = merge_trace_payloads(&old, &new);
        assert_eq!(merged["tags"]["k"], "v2");
        assert_eq!(
            merged["turns"][0]["turn_id"],
            "t1",
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
}
