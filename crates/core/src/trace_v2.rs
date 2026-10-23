//! trace v2 合并、诊断标注与会话聚合（契约 `.dev-team/trace-fix-contract.md` FROZEN v1）。
//!
//! 本文件实现 C2（合并语义 v2）与 C4（服务端诊断标注）的纯函数层，不做任何 I/O。
//!
//! 契约锚点：
//! - `C1.1` `trace_turns` 行模型（`turn_id` / `seq` / `status` / `payload`）→ [`MergedTurn`]；
//! - `C2` 合并语义 v2（终态优先 → 字段级深合并 → `tool_calls` 按 id 合并）→ [`merge_turn`]、
//!   [`is_terminal_status`]、[`aggregate_session`]、[`merge_top_level`]；
//! - `C4` 服务端诊断标注（`error_contract` / `status_conflict` / `error_missing`）→
//!   [`TurnDiagnostics`]、[`annotate_turn`]；
//! - `C1.1` `status` 归一（缺失 → `unknown`）→ [`turn_status_of`]。

use serde_json::{Map, Value};
use std::collections::HashSet;
use sha2::{Digest, Sha256};

/// `C2.1` 终态集合：一旦到达即不再回退的轮次状态。
const TERMINAL_STATUSES: [&str; 9] = [
    "completed",
    "succeeded",
    "failed",
    "error",
    "cancelled",
    "interrupted",
    "timed_out",
    "max_tokens",
    "aborted",
];

/// `C2.1`（amend v1.4-4）success 类终态：覆盖旧终态时清空旧错误残留。
const SUCCESS_STATUSES: [&str; 2] = ["completed", "succeeded"];

/// `C4` 第 1 条：需要校验「错误详情是否上报」的轮次状态。
const ERROR_CONTRACT_STATUSES: [&str; 4] = ["failed", "error", "cancelled", "timed_out"];

/// `C4` 第 2 条：`tool_call.output` 中命中即视为「成功状态 + 失败文案」的特征词（大小写不敏感）。
const FAILURE_MARKERS: [&str; 5] = ["失败", "异常", "解析失败", "unable", "空结果"];

/// `C1.1`：`turn.status` 缺失时的归一值。
const UNKNOWN_STATUS: &str = "unknown";

/// 服务端生成的 turn 诊断标注（契约 `C4`）。
///
/// 仅在检出问题时由 [`annotate_turn`] 返回；无问题时返回 `None`。
/// 该对象最终写入 turn 的 `diagnostics` 字段，故需要 serde 派生（`C4` / `C2.4`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnDiagnostics {
    /// `Some("missing")`：`status ∈ {failed,error,cancelled,timed_out}` 且 turn 与全部
    /// `tool_calls` 均未上报错误详情（`C4` 第 1 条）。
    pub error_contract: Option<String>,
    /// `true`：某 `tool_call.status == "success"` 但 `output` 命中失败特征（`C4` 第 2 条）。
    pub status_conflict: bool,
    /// `true`：某 `tool_call.status == "error"` 且 `error` 缺失/null（`C4` 第 3 条）。
    pub error_missing: bool,
}

/// `trace_turns` 表中的一行（契约 `C1.1`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MergedTurn {
    /// 轮次键；缺失时由服务端生成 `turn_srv_<sha256(payload)[:16]>`（`C1.1`）。
    pub turn_id: String,
    /// 会话内首次出现序号，从 0 起单调（`C1.1` / `C2.5`）。
    pub seq: i64,
    /// 归一后的轮次状态；缺失为 `unknown`（`C1.1`）。
    pub status: String,
    /// 该 turn 的完整 JSON（含合并后的 `tool_calls` 与 `diagnostics`）。
    pub payload: Value,
}

/// 为 turn 生成诊断标注（`C4`）；无问题时返回 `None`。
///
/// 实现要求：只读原始数据、不改写事实字段；标注结果写入 `turn["diagnostics"]`。
/// 无问题时（含上一个版本留下的 `diagnostics`）会移除该字段，保证「仅在有问题时出现」。
pub fn annotate_turn(turn: &mut Value) -> Option<TurnDiagnostics> {
    let status = turn_status_of(turn);
    let tool_calls = turn.get("tool_calls").and_then(Value::as_array);
    let tool_calls: &[Value] = tool_calls.map_or(&[], Vec::as_slice);
    let turn_has_error = has_error_detail(turn.get("error"))
        || has_error_detail(turn.get("error_type"));
    let calls_have_error = tool_calls
        .iter()
        .any(|call| has_error_detail(call.get("error")));
    let error_contract = if is_error_contract_status(&status) && !turn_has_error && !calls_have_error
    {
        Some("missing".to_string())
    } else {
        None
    };
    let status_conflict = tool_calls.iter().any(tool_call_has_status_conflict);
    let error_missing = tool_calls
        .iter()
        .any(|call| status_matches(call, "error") && !has_error_detail(call.get("error")));

    let mut diagnostics = Map::new();
    if let Some(contract) = &error_contract {
        diagnostics.insert("error_contract".to_string(), Value::String(contract.clone()));
    }
    if status_conflict {
        diagnostics.insert("status_conflict".to_string(), Value::Bool(true));
    }
    if error_missing {
        diagnostics.insert("error_missing".to_string(), Value::Bool(true));
    }

    if let Some(object) = turn.as_object_mut() {
        if diagnostics.is_empty() {
            object.remove("diagnostics");
        } else {
            object.insert("diagnostics".to_string(), Value::Object(diagnostics));
        }
    }

    if error_contract.is_none() && !status_conflict && !error_missing {
        return None;
    }
    Some(TurnDiagnostics {
        error_contract,
        status_conflict,
        error_missing,
    })
}

/// 合并同一 `(session_id, turn_id)` 的旧 turn 与新 turn，返回合并结果（`C2`）。
///
/// 规则顺序：终态优先（禁止回退）→ 字段级深合并 → `tool_calls` 按 `tool_call_id` 合并
/// → `diagnostics` 取合集。
pub fn merge_turn(existing: &Value, incoming: &Value) -> Value {
    let old_status = turn_status_of(existing);
    let new_status = turn_status_of(incoming);
    // C2.1（amend v1.4-4）：终态 → 终态且新终态为 success 类 ⇒ 清空旧错误残留，
    // 避免出现"成功状态 + 残留 error/error_type/diagnostics"的自相矛盾数据。
    let clears_error_residue = is_terminal_status(&old_status) && is_success_status(&new_status);
    // `C2.1`：终态单调。旧终态 + 新非终态 → 保留旧；新终态 → 新覆盖；其余交给字段级深合并。
    let monotonic_status = if is_terminal_status(&old_status) && !is_terminal_status(&new_status) {
        Some(old_status)
    } else if is_terminal_status(&new_status) {
        Some(new_status)
    } else {
        None
    };

    let mut merged = merge_values(existing, incoming);
    if let Some(object) = merged.as_object_mut() {
        if let Some(status) = monotonic_status {
            object.insert("status".to_string(), Value::String(status));
        }
        // `C2.3`：tool_calls 按 tool_call_id 合并（旧条目保留 + 新 id 追加）。
        if let (Some(old_calls), Some(new_calls)) = (
            existing.get("tool_calls").and_then(Value::as_array),
            incoming.get("tool_calls").and_then(Value::as_array),
        ) {
            object.insert(
                "tool_calls".to_string(),
                Value::Array(merge_tool_calls(old_calls, new_calls)),
            );
        }
        // `C2.4`：diagnostics 由服务端生成，合并时保留双方合集。
        if let (Some(old_diagnostics), Some(new_diagnostics)) = (
            existing.get("diagnostics"),
            incoming.get("diagnostics"),
        ) {
            object.insert(
                "diagnostics".to_string(),
                union_values(old_diagnostics, new_diagnostics),
            );
        }
        if clears_error_residue {
            for key in ["error", "error_type", "diagnostics"] {
                object.remove(key);
            }
        }
    }
    merged
}

/// 会话级聚合：按 `seq` 排序的 turns 求和生成写回 `traces.payload` 的新载荷（`C2` 会话级聚合）。
///
/// `total_input_tokens` / `total_output_tokens` / `total_duration_ms` 必须是 turns 求和这一
/// 单一真相（无任何 turn 含该字段时为 `null`）；移除 `payload.eval_status`；顶层其余字段取自
/// `incoming_top`（保留旧值缺失的新值，见 [`merge_top_level`]）。
///
/// 调用序（`extra` / `tags` 的深合并发生在 [`merge_top_level`]）：
/// ```text
/// let top = merge_top_level(&old_payload, &incoming_payload);
/// let payload = aggregate_session(&merged_turns, &top);
/// ```
pub fn aggregate_session(turns: &[Value], incoming_top: &Value) -> Value {
    let mut top = match incoming_top {
        Value::Object(object) => object.clone(),
        _ => Map::new(),
    };
    let ordered = ordered_turns(turns);
    // `C2`（amend v1.4-9 / L3A-F9）：求和用 `checked_add`，溢出时跳过该 total 并标记
    // `stats_incomplete: true`（绝不 panic / 回绕）。`stats_incomplete` 是服务端派生字段，
    // 每次聚合都重算（不继承旧上报值），避免标记残留。
    let mut incomplete = false;
    for (total_field, turn_field) in [
        ("total_input_tokens", "input_tokens"),
        ("total_output_tokens", "output_tokens"),
        ("total_duration_ms", "duration_ms"),
    ] {
        let (total, overflow) = sum_turn_field(&ordered, turn_field);
        if overflow {
            incomplete = true;
        }
        top.insert(total_field.to_string(), total.map_or(Value::Null, Value::from));
    }
    top.remove("stats_incomplete");
    if incomplete {
        top.insert("stats_incomplete".to_string(), Value::Bool(true));
    }
    // `C2`：列是 eval_status 的唯一真源，payload 不保留副本（修 W-10）。
    top.remove("eval_status");
    top.insert("turns".to_string(), Value::Array(ordered));
    Value::Object(top)
}

/// 会话级顶层合并（`C2` 会话级聚合的前置步骤）：`extra` / `tags` 深合并（新非空覆盖、
/// 缺失保留旧 —— 修 W-09），其余顶层字段以本次上报值优先、旧值兜底；移除 `eval_status`。
///
/// 由于 [`aggregate_session`] 的签名只含本次上报的顶层，旧顶层的 `extra` / `tags` 必须在
/// 调用聚合前先经本函数合入，二者构成 C2 聚合的完整口径。
pub fn merge_top_level(existing_top: &Value, incoming_top: &Value) -> Value {
    let mut merged = merge_values(existing_top, incoming_top);
    if let Some(object) = merged.as_object_mut() {
        object.remove("eval_status");
    }
    merged
}

/// 取 turn 的归一状态；缺失/非法 → `"unknown"`（`C1.1`）。
pub fn turn_status_of(turn: &Value) -> String {
    turn.get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .unwrap_or(UNKNOWN_STATUS)
        .to_string()
}

/// 判定是否为终态（`C2.1` 的 `terminal` 集合）。
pub fn is_terminal_status(status: &str) -> bool {
    let status = status.trim();
    TERMINAL_STATUSES
        .iter()
        .any(|terminal| terminal.eq_ignore_ascii_case(status))
}

/// `C1.1`（amend v1.4-2）：服务端生成 turn_id 的前缀（`turn_srv_`，避免客户端冒充）。
const SERVER_TURN_PREFIX: &str = "turn_srv_";

/// `C1.1`：turn 的稳定键——显式 `turn_id` 优先；缺失时按整对象 JSON 的 sha256 前 16 位生成
/// `turn_srv_<hex16>`。同对象 ⇒ 同键，保证重复上报的幂等性（`trace_turns` 主键）。
/// 前缀 `turn_srv_` 标记服务端生成，避免与客户端 turn_id 混淆（契约 amend v1.4-2）。
pub fn turn_key_of(turn: &Value) -> String {
    if let Some(turn_id) = explicit_turn_id(turn) {
        return turn_id;
    }
    // serde_json 的 Object 默认按 key 排序（BTreeMap），序列化结果对同一 Value 稳定。
    let encoded = serde_json::to_vec(turn).unwrap_or_default();
    let digest = Sha256::digest(&encoded);
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{SERVER_TURN_PREFIX}{hex}")
}

/// 批量生成轮次键（回填/hydration 用）：显式 `turn_id` 原样保留；服务端生成的键在
/// **同一批内**出现内容碰撞时追加 `_2`/`_3` 后缀，保证「缺 turn_id 的两条不同时刻 legacy
/// turn」不会因哈希相同而塌成一行（同名同内容者仍按同键幂等）。顺序确定 ⇒ 重复执行键一致。
pub fn turn_keys_for(turns: &[Value]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut keys = Vec::with_capacity(turns.len());
    for turn in turns {
        if let Some(turn_id) = explicit_turn_id(turn) {
            keys.push(turn_id);
            continue;
        }
        let base = turn_key_of(turn);
        let mut candidate = base.clone();
        let mut suffix = 2;
        while !used.insert(candidate.clone()) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        keys.push(candidate);
    }
    keys
}

/// 取出显式 `turn_id`（trim 后非空）。
fn explicit_turn_id(turn: &Value) -> Option<String> {
    turn.get("turn_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string)
}

/// 把上报的 turn 归一为可入库的 JSON 对象（`C1.1`）：对象原样返回，非对象元素包一层 `value`
/// 以免静默丢数据（服务端强校验由 ingest 层负责，仓储层只保证不丢）。
pub fn normalize_incoming_turn(turn: &Value) -> Value {
    match turn {
        Value::Object(_) => turn.clone(),
        other => {
            let mut object = Map::new();
            object.insert("value".to_string(), other.clone());
            Value::Object(object)
        }
    }
}

/// `C5`：主键来源标注——`payload.session_id_source` 已由上报方给出时原样保留，
/// 否则按「payload 带非空 `session_id` ⇒ `session_id`，否则回退 `thread_id`」推断。
pub fn session_id_source_of(payload: &Value) -> String {
    if let Some(source) = payload
        .get("session_id_source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
    {
        return source.to_string();
    }
    let has_session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|session_id| !session_id.is_empty());
    if has_session_id {
        "session_id".to_string()
    } else {
        "thread_id".to_string()
    }
}

// ── 内部工具 ────────────────────────────────────────────────────────────────

/// `C2.2`：新值「非 null、非空字符串、非空数组/对象」即视为有效覆盖值。
fn is_meaningful(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(entries) => !entries.is_empty(),
        _ => true,
    }
}

/// `C2.2` 字段级深合并：对象递归合并、数组整体覆盖（非空时）、标量以有效新值覆盖。
fn merge_values(existing: &Value, incoming: &Value) -> Value {
    match (existing, incoming) {
        (Value::Object(old), Value::Object(new)) => {
            let mut merged = old.clone();
            for (key, new_value) in new {
                match merged.get(key) {
                    Some(old_value) => {
                        let value = merge_values(old_value, new_value);
                        merged.insert(key.clone(), value);
                    }
                    None if is_meaningful(new_value) => {
                        merged.insert(key.clone(), new_value.clone());
                    }
                    None => {}
                }
            }
            Value::Object(merged)
        }
        _ => {
            if is_meaningful(incoming) {
                incoming.clone()
            } else {
                existing.clone()
            }
        }
    }
}

/// `C2.4` 合集语义：布尔取或、对象递归、其余以有效新值覆盖。
fn union_values(existing: &Value, incoming: &Value) -> Value {
    match (existing, incoming) {
        (Value::Bool(old), Value::Bool(new)) => Value::Bool(*old || *new),
        (Value::Object(old), Value::Object(new)) => {
            let mut merged = old.clone();
            for (key, new_value) in new {
                match merged.get(key) {
                    Some(old_value) => {
                        let value = union_values(old_value, new_value);
                        merged.insert(key.clone(), value);
                    }
                    None if is_meaningful(new_value) => {
                        merged.insert(key.clone(), new_value.clone());
                    }
                    None => {}
                }
            }
            Value::Object(merged)
        }
        _ => {
            if is_meaningful(incoming) {
                incoming.clone()
            } else {
                existing.clone()
            }
        }
    }
}

/// `C2.3`（amend v1.4-3）：旧条目按原序保留；同键以字段级合并覆盖；新键追加。
///
/// 合并键：`tool_call_id` 优先；缺失时退回 `(tool_name, arguments)`（`arguments` 兼容
/// `arguments_summary` 字符串形态），避免无 id 的调用在每次重报时无界追加。
fn merge_tool_calls(existing: &[Value], incoming: &[Value]) -> Vec<Value> {
    let mut merged = existing.to_vec();
    for call in incoming {
        let key = tool_call_key(call);
        let position = key.as_ref().and_then(|key| {
            merged
                .iter()
                .position(|old_call| tool_call_key(old_call).as_ref() == Some(key))
        });
        match position {
            Some(index) => {
                merged[index] = merge_values(&merged[index], call);
            }
            None => merged.push(call.clone()),
        }
    }
    merged
}

/// `C2.3` 合并键：显式 `tool_call_id` → `id:<id>`；
/// 否则 `sig:<tool_name>|<arguments 规范化 JSON>`（二者皆缺失时退化为 `sig:<tool_name>|`）。
fn tool_call_key(call: &Value) -> Option<String> {
    if let Some(call_id) = call
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|call_id| !call_id.is_empty())
    {
        return Some(format!("id:{call_id}"));
    }
    let tool_name = call
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let arguments = call
        .get("arguments")
        .or_else(|| call.get("arguments_summary"));
    let encoded = arguments
        .map(|value| serde_json::to_string(value).unwrap_or_default())
        .unwrap_or_default();
    Some(format!("sig:{tool_name}|{encoded}"))
}

/// `C2.1` success 类终态集合（amend v1.4-4：这类终态覆盖旧终态时清空错误残留）。
fn is_success_status(status: &str) -> bool {
    let status = status.trim();
    SUCCESS_STATUSES
        .iter()
        .any(|success| success.eq_ignore_ascii_case(status))
}

/// 按 `seq` 排序；全部 turn 都带 `seq` 时才重排（调用方通常已按 seq 读出）。
fn ordered_turns(turns: &[Value]) -> Vec<Value> {
    let mut ordered = turns.to_vec();
    let all_sequenced = ordered
        .iter()
        .all(|turn| turn.get("seq").and_then(Value::as_i64).is_some());
    if all_sequenced && ordered.len() > 1 {
        ordered.sort_by_key(|turn| turn.get("seq").and_then(Value::as_i64).unwrap_or_default());
    }
    ordered
}

/// 按 turn 级字段求和；无任何 turn 含该字段时返回 `(None, false)`（`C2` 单一真相）。
///
/// 第二个返回值表示**是否发生 i64 溢出**（`amend v1.4-9` / L3A-F9）：溢出时返回
/// `(None, true)`，由调用方跳过该 total 并标记 `stats_incomplete`，不做裸 `+`（debug 下会 panic）。
fn sum_turn_field(turns: &[Value], field: &str) -> (Option<i64>, bool) {
    let mut total: i64 = 0;
    let mut seen = false;
    for value in turns.iter().filter_map(|turn| turn.get(field).and_then(Value::as_i64)) {
        match total.checked_add(value) {
            Some(next) => {
                total = next;
                seen = true;
            }
            None => return (None, true),
        }
    }
    (if seen { Some(total) } else { None }, false)
}

/// `C4` 第 1 条的状态集合判定（大小写不敏感）。
fn is_error_contract_status(status: &str) -> bool {
    let status = status.trim();
    ERROR_CONTRACT_STATUSES
        .iter()
        .any(|target| target.eq_ignore_ascii_case(status))
}

/// 字段是否携带可用的错误详情（非 null / 非空字符串 / 非空对象/数组）。
fn has_error_detail(value: Option<&Value>) -> bool {
    value.is_some_and(is_meaningful)
}

/// `tool_call.status` 是否等于给定状态（大小写不敏感）。
fn status_matches(call: &Value, expected: &str) -> bool {
    call.get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.trim().eq_ignore_ascii_case(expected))
}

/// `C4` 第 2 条：`status == "success"` 但 `output` 命中失败特征。
fn tool_call_has_status_conflict(call: &Value) -> bool {
    if !status_matches(call, "success") {
        return false;
    }
    let Some(output) = call.get("output") else {
        return false;
    };
    let text = match output {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let text = text.to_lowercase();
    FAILURE_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── C2 合并语义 ──────────────────────────────────────────────────────

    #[test]
    fn terminal_set_matches_contract_and_ignores_case() {
        for status in [
            "completed",
            "succeeded",
            "failed",
            "error",
            "cancelled",
            "interrupted",
            "timed_out",
            "max_tokens",
            "aborted",
        ] {
            assert!(is_terminal_status(status), "`{status}` 应为终态");
        }
        for status in ["running", "pending", "in_progress", "unknown", ""] {
            assert!(!is_terminal_status(status), "`{status}` 不应是终态");
        }
        assert!(is_terminal_status(" Failed "), "终态判定应忽略空白与大小写");
    }

    #[test]
    fn c2_1_terminal_status_overrides_and_keeps_late_error() {
        let existing = json!({
            "turn_id": "t1",
            "status": "running",
            "attempt_index": 1,
            "tool_calls": [{"tool_call_id": "c1", "status": "success", "output": "ok"}]
        });
        let incoming = json!({
            "turn_id": "t1",
            "status": "failed",
            "attempt_index": 2,
            "error": {"type": "TimeoutError", "message": "boom"}
        });
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["status"], "failed");
        assert_eq!(merged["error"]["message"], "boom", "迟到补全的 error 必须保留");
        assert_eq!(merged["attempt_index"], 2);
        assert_eq!(
            merged["tool_calls"][0]["tool_call_id"], "c1",
            "终态覆盖不得吞掉上一轮的 tool_calls"
        );
    }

    #[test]
    fn c2_1_non_terminal_report_never_regresses_terminal_status() {
        let existing = json!({
            "turn_id": "t1",
            "status": "failed",
            "error": {"message": "keep-me"}
        });
        let incoming = json!({"turn_id": "t1", "status": "running", "degraded": false});
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["status"], "failed", "旧终态 + 新非终态禁止回退");
        assert_eq!(merged["error"]["message"], "keep-me", "非终态上报不得清空既有 error");
        assert_eq!(merged["degraded"], false, "非空新字段仍按字段级合并写入");
    }

    #[test]
    fn c2_1_late_terminal_status_advances_between_terminal_states() {
        let existing = json!({"turn_id": "t1", "status": "succeeded"});
        let incoming = json!({"turn_id": "t1", "status": "error", "error": {"message": "late"}});
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["status"], "error", "新终态必须覆盖旧终态");
        assert_eq!(merged["error"]["message"], "late");
    }

    #[test]
    fn c2_2_field_level_merge_skips_null_and_empty_values() {
        let existing = json!({
            "turn_id": "t1",
            "status": "running",
            "model": "claude",
            "error": {"message": "m", "type": "T"},
            "tags": {"app": "api_v2"},
            "steps": ["a"],
            "attempt_index": 2
        });
        let incoming = json!({
            "turn_id": "t1",
            "status": "running",
            "model": "",
            "error": null,
            "tags": {},
            "steps": [],
            "meta": {"k": 1}
        });
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["model"], "claude", "空字符串不得覆盖旧值");
        assert_eq!(merged["error"]["message"], "m", "null 不得覆盖旧值");
        assert_eq!(merged["tags"]["app"], "api_v2", "空对象不得覆盖旧值");
        assert_eq!(merged["steps"][0], "a", "空数组不得覆盖旧值");
        assert_eq!(merged["attempt_index"], 2);
        assert_eq!(merged["meta"]["k"], 1, "新字段应按字段级合并写入");
    }

    #[test]
    fn c2_2_step_deeper_json_object_is_deep_merged() {
        let existing = json!({"turn_id": "t1", "status": "running", "meta": {"a": 1, "b": 2}});
        let incoming = json!({"turn_id": "t1", "status": "running", "meta": {"b": 3, "c": 4}});
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["meta"], json!({"a": 1, "b": 3, "c": 4}));
    }

    #[test]
    fn c2_3_tool_calls_merge_by_id_and_append_new_ids() {
        let existing = json!({
            "turn_id": "t1",
            "status": "running",
            "tool_calls": [
                {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"},
                {"tool_call_id": "c2", "tool_name": "fetch", "status": "success", "output": "ok"}
            ]
        });
        let incoming = json!({
            "turn_id": "t1",
            "status": "failed",
            "tool_calls": [
                {"tool_call_id": "c1", "tool_name": "parse", "status": "error", "error": {"message": "解析失败"}},
                {"tool_call_id": "c3", "tool_name": "retry", "status": "success", "output": "ok"}
            ]
        });
        let merged = merge_turn(&existing, &incoming);
        let calls = merged["tool_calls"].as_array().expect("tool_calls 数组");
        assert_eq!(calls.len(), 3, "旧条目保留 + 新 id 追加");
        assert_eq!(calls[0]["tool_call_id"], "c1", "同 id 条目原位合并，不打乱旧序");
        assert_eq!(calls[0]["status"], "error", "新状态覆盖旧条目");
        assert_eq!(calls[0]["error"]["message"], "解析失败");
        assert_eq!(calls[1]["tool_call_id"], "c2", "旧 tool_call 不得丢失");
        assert_eq!(calls[1]["status"], "success");
        assert_eq!(calls[2]["tool_call_id"], "c3");
    }

    #[test]
    fn c2_3_empty_or_missing_tool_calls_keep_existing() {
        let existing = json!({
            "turn_id": "t1",
            "status": "running",
            "tool_calls": [{"tool_call_id": "c1", "status": "success", "output": "ok"}]
        });
        let merged = merge_turn(&existing, &json!({"turn_id": "t1", "status": "succeeded"}));
        assert_eq!(merged["tool_calls"][0]["tool_call_id"], "c1");
        let merged = merge_turn(&existing, &json!({"turn_id": "t1", "status": "running", "tool_calls": []}));
        assert_eq!(merged["tool_calls"][0]["tool_call_id"], "c1");
    }

    #[test]
    fn c2_4_diagnostics_union_survives_merge() {
        let existing = json!({
            "turn_id": "t1",
            "status": "failed",
            "diagnostics": {"error_contract": "missing", "status_conflict": false}
        });
        let incoming = json!({
            "turn_id": "t1",
            "status": "failed",
            "diagnostics": {"status_conflict": true}
        });
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["diagnostics"]["error_contract"], "missing", "旧诊断不得丢");
        assert_eq!(merged["diagnostics"]["status_conflict"], true, "布尔标注取或");
    }

    #[test]
    fn c2_5_missing_status_never_erases_known_status() {
        let existing = json!({"turn_id": "t1", "status": "running"});
        let merged = merge_turn(&existing, &json!({"turn_id": "t1", "attempt_index": 1}));
        assert_eq!(merged["status"], "running");
        assert_eq!(turn_status_of(&merged), "running");
        assert_eq!(turn_status_of(&json!({"turn_id": "t1"})), "unknown");
    }

    // ── C4 诊断标注 ──────────────────────────────────────────────────────

    #[test]
    fn c4_error_contract_missing_for_terminal_status_without_error_details() {
        let mut turn = json!({
            "turn_id": "t1",
            "status": "failed",
            "tool_calls": [{"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"}]
        });
        let diagnostics = annotate_turn(&mut turn).expect("failed 且无错误详情必须产出诊断");
        assert_eq!(diagnostics.error_contract.as_deref(), Some("missing"));
        assert!(!diagnostics.status_conflict);
        assert!(!diagnostics.error_missing);
        assert_eq!(turn["diagnostics"]["error_contract"], "missing");
        assert!(turn["diagnostics"].get("status_conflict").is_none());
        assert!(turn["diagnostics"].get("error_missing").is_none());
    }

    #[test]
    fn c4_error_contract_not_raised_when_error_or_tool_error_reported() {
        let mut turn = json!({"turn_id": "t1", "status": "error", "error": {"message": "boom"}});
        assert!(annotate_turn(&mut turn).is_none());
        assert!(turn.get("diagnostics").is_none());

        let mut turn = json!({
            "turn_id": "t1",
            "status": "timed_out",
            "tool_calls": [{"tool_call_id": "c1", "status": "error", "error": {"message": "x"}}]
        });
        assert!(annotate_turn(&mut turn).is_none());

        let mut turn = json!({"turn_id": "t1", "status": "completed"});
        assert!(annotate_turn(&mut turn).is_none(), "非 C4 状态集不标注 error_contract");
    }

    #[test]
    fn c4_status_conflict_when_success_output_hits_failure_marker() {
        let mut turn = json!({
            "turn_id": "t1",
            "status": "completed",
            "tool_calls": [{
                "tool_call_id": "c1",
                "tool_name": "format_jd_to_json",
                "status": "success",
                "output": "职位解析失败（原因: 模型返回空结果）"
            }]
        });
        let diagnostics = annotate_turn(&mut turn).expect("成功状态 + 失败文案必须标注");
        assert!(diagnostics.status_conflict);
        assert_eq!(turn["diagnostics"]["status_conflict"], true);

        let mut turn = json!({
            "turn_id": "t2",
            "status": "completed",
            "tool_calls": [{"tool_call_id": "c1", "status": "success", "output": "UNABLE to parse"}]
        });
        assert!(annotate_turn(&mut turn).is_some_and(|d| d.status_conflict), "特征词匹配须大小写不敏感");

        let mut healthy = json!({
            "turn_id": "t3",
            "status": "completed",
            "tool_calls": [{"tool_call_id": "c1", "status": "success", "output": "ok"}]
        });
        assert!(annotate_turn(&mut healthy).is_none());
    }

    #[test]
    fn c4_error_missing_when_tool_call_error_has_no_details() {
        let mut turn = json!({
            "turn_id": "t1",
            "status": "completed",
            "tool_calls": [{"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "error"}]
        });
        let diagnostics = annotate_turn(&mut turn).expect("error 无详情必须标注");
        assert!(diagnostics.error_missing);
        assert_eq!(turn["diagnostics"]["error_missing"], true);

        let mut with_null = json!({
            "turn_id": "t2",
            "status": "completed",
            "tool_calls": [{"tool_call_id": "c1", "status": "error", "error": null}]
        });
        assert!(annotate_turn(&mut with_null).is_some_and(|d| d.error_missing), "error=null 视同缺失");
    }

    // ── C2 会话级聚合 ────────────────────────────────────────────────────

    #[test]
    fn c2_aggregate_sums_turns_and_ignores_client_totals() {
        let turns = vec![
            json!({"turn_id": "t1", "status": "succeeded", "input_tokens": 10, "output_tokens": 5, "duration_ms": 100}),
            json!({"turn_id": "t2", "status": "failed", "input_tokens": 20, "output_tokens": 7, "duration_ms": 200}),
        ];
        let incoming = json!({
            "session_id": "s1",
            "total_input_tokens": 99999,
            "total_output_tokens": 99999,
            "total_duration_ms": 99999,
            "turns": []
        });
        let payload = aggregate_session(&turns, &incoming);
        assert_eq!(payload["total_input_tokens"], 30);
        assert_eq!(payload["total_output_tokens"], 12);
        assert_eq!(payload["total_duration_ms"], 300);
        assert_eq!(payload["turns"].as_array().map(Vec::len), Some(2));
        assert_eq!(payload["session_id"], "s1", "其余顶层取本次上报值");
    }

    #[test]
    fn c2_aggregate_totals_are_null_without_any_turn_field() {
        let turns = vec![json!({"turn_id": "t1", "status": "succeeded"})];
        let payload = aggregate_session(&turns, &json!({"session_id": "s1", "total_input_tokens": 7}));
        assert!(payload["total_input_tokens"].is_null(), "无 turn 含字段时 total 必须为 null");
        assert!(payload["total_output_tokens"].is_null());
        assert!(payload["total_duration_ms"].is_null());
    }

    #[test]
    fn c2_top_level_extra_and_tags_deep_merge_and_eval_status_removed() {
        let old = json!({
            "session_id": "s1",
            "release": "dev",
            "eval_status": "triage_good",
            "extra": {"project_path": "a", "keep": "x"},
            "tags": {"app": "api_v2"},
            "turns": [{"turn_id": "t1", "status": "succeeded"}]
        });
        let incoming = json!({
            "session_id": "s1",
            "eval_status": "triage_bad",
            "extra": {"project_path": "b"},
            "tags": {"component": "agent"},
            "turns": [{"turn_id": "t2", "status": "failed"}]
        });
        let top = merge_top_level(&old, &incoming);
        let turns = vec![
            json!({"turn_id": "t1", "status": "succeeded"}),
            json!({"turn_id": "t2", "status": "failed"}),
        ];
        let payload = aggregate_session(&turns, &top);
        assert_eq!(payload["extra"]["project_path"], "b", "非空新值覆盖");
        assert_eq!(payload["extra"]["keep"], "x", "二次上报缺省字段不得置 null");
        assert_eq!(payload["tags"]["app"], "api_v2");
        assert_eq!(payload["tags"]["component"], "agent");
        assert!(payload.get("eval_status").is_none(), "payload.eval_status 必须移除");
        assert_eq!(payload["release"], "dev");
    }

    #[test]
    fn c2_totals_use_checked_add_and_flag_stats_incomplete() {
        let turns = vec![
            json!({"turn_id": "t1", "status": "succeeded", "input_tokens": i64::MAX}),
            json!({"turn_id": "t2", "status": "succeeded", "input_tokens": 1, "output_tokens": 5}),
        ];
        let payload = aggregate_session(&turns, &json!({"session_id": "s1"}));
        assert!(
            payload["total_input_tokens"].is_null(),
            "[L3A-F9] i64 溢出必须跳过求和（不得回绕/panic）"
        );
        assert_eq!(payload["total_output_tokens"], 5, "未溢出的字段照常求和");
        assert_eq!(
            payload["stats_incomplete"], true,
            "[L3A-F9] 溢出必须标记 stats_incomplete"
        );

        // 无溢出时不得带 stats_incomplete，且旧标记必须被重算清除（派生字段不继承）
        let payload = aggregate_session(
            &[json!({"turn_id": "t1", "status": "succeeded", "input_tokens": 3})],
            &json!({"session_id": "s1", "stats_incomplete": true}),
        );
        assert_eq!(payload["total_input_tokens"], 3);
        assert!(
            payload.get("stats_incomplete").is_none(),
            "[L3A-F9] 旧 stats_incomplete 标记必须按本次聚合重算清除"
        );
    }

    #[test]
    fn c2_aggregate_sorts_turns_by_seq_and_passes_session_id_source() {
        let turns = vec![
            json!({"turn_id": "t2", "seq": 1, "status": "failed"}),
            json!({"turn_id": "t1", "seq": 0, "status": "succeeded"}),
        ];
        let incoming = json!({
            "session_id": "thread-1",
            "session_id_source": "thread_id",
            "eval_status": "triage_good",
            "turns": []
        });
        let payload = aggregate_session(&turns, &incoming);
        let ordered = payload["turns"].as_array().expect("turns 数组");
        assert_eq!(ordered[0]["turn_id"], "t1");
        assert_eq!(ordered[1]["turn_id"], "t2");
        assert_eq!(payload["session_id_source"], "thread_id", "session_id_source 必须透传");
        assert!(payload.get("eval_status").is_none(), "直接调用聚合也必须移除 eval_status");
    }

    #[test]
    fn merge_top_level_keeps_old_only_scalars_and_strips_eval_status() {
        let merged = merge_top_level(
            &json!({"run_id": "r1", "eval_status": "triage_good"}),
            &json!({"release": "dev"}),
        );
        assert_eq!(merged["run_id"], "r1", "旧值缺失新值时保留");
        assert_eq!(merged["release"], "dev");
        assert!(merged.get("eval_status").is_none());
    }

    // ── C1.1 轮次键 / C5 主键来源 ────────────────────────────────────────

    #[test]
    fn turn_key_prefers_explicit_id_and_hashes_missing_id_deterministically() {
        assert_eq!(turn_key_of(&json!({"turn_id": "t1"})), "t1");
        assert_eq!(turn_key_of(&json!({"turn_id": "  t2  "})), "t2");

        let turn = json!({"status": "failed", "tool_calls": [{"tool_call_id": "c1"}]});
        let first = turn_key_of(&turn);
        assert_eq!(first, turn_key_of(&turn), "同一对象必须得到同一自动 turn_id（幂等）");
        assert!(
            first.starts_with("turn_srv_") && first.len() == "turn_srv_".len() + 16,
            "自动 turn_id 形态必须是 turn_srv_<16 hex>（amend v1.4-2）：{first}"
        );
        assert!(
            first["turn_srv_".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "自动 turn_id 必须是十六进制：{first}"
        );
        assert_ne!(
            first,
            turn_key_of(&json!({"status": "succeeded"})),
            "不同对象必须得到不同自动 turn_id"
        );
    }

    #[test]
    fn normalize_incoming_turn_keeps_non_object_payloads() {
        assert_eq!(
            normalize_incoming_turn(&json!({"turn_id": "t1"})),
            json!({"turn_id": "t1"}),
            "对象原样返回"
        );
        assert_eq!(
            normalize_incoming_turn(&json!("not-an-object")),
            json!({"value": "not-an-object"}),
            "非对象元素不得丢数据"
        );
    }

    #[test]
    fn turn_keys_for_keeps_explicit_ids_and_dedupes_content_collisions() {
        // 缺 turn_id 的两条不同 turn → 内容 sha256 不同
        let distinct = vec![json!({"status": "succeeded"}), json!({"status": "failed"})];
        let keys = turn_keys_for(&distinct);
        assert_eq!(keys.len(), 2);
        assert!(keys[0].starts_with("turn_srv_") && keys[1].starts_with("turn_srv_"));
        assert_ne!(keys[0], keys[1], "不同内容必须得到不同 turn_id");

        // 内容完全相同的两条 legacy turn：仍不得塌成一行（追加序号后缀，且确定）
        let identical = vec![json!({"status": "succeeded"}), json!({"status": "succeeded"})];
        let keys = turn_keys_for(&identical);
        assert_ne!(keys[0], keys[1], "[L3A-F1] 内容碰撞也必须两行不同键");
        assert_eq!(keys[1], format!("{}_2", keys[0]), "碰撞后缀必须确定");
        assert_eq!(turn_keys_for(&identical), keys, "重复执行键必须一致（幂等）");

        // 显式 turn_id 原样保留（不追加后缀，即便重复——同 id 即同一 turn）
        let explicit = vec![json!({"turn_id": "t1"}), json!({"turn_id": "t1"})];
        assert_eq!(turn_keys_for(&explicit), vec!["t1", "t1"]);
    }

    #[test]
    fn c2_1_terminal_to_success_clears_legacy_error_residue() {
        let existing = json!({
            "turn_id": "t1",
            "status": "failed",
            "error": {"type": "TimeoutError", "message": "boom"},
            "error_type": "TimeoutError",
            "diagnostics": {"error_contract": "missing", "error_missing": true},
            "tool_calls": [{"tool_call_id": "c1", "status": "error", "error": {"message": "x"}}]
        });
        let incoming = json!({"turn_id": "t1", "status": "succeeded"});
        let merged = merge_turn(&existing, &incoming);
        assert_eq!(merged["status"], "succeeded", "success 类终态覆盖旧终态");
        assert!(merged.get("error").is_none(), "[L3A-F10] 必须清空旧 error");
        assert!(merged.get("error_type").is_none(), "[L3A-F10] 必须清空旧 error_type");
        assert!(
            merged.get("diagnostics").is_none(),
            "[L3A-F10] 必须清空旧 diagnostics"
        );
        assert_eq!(
            merged["tool_calls"][0]["status"], "error",
            "tool_calls 事实字段不得被清空（只清 turn 级错误残留）"
        );

        // 非终态 → 终态（failed）时仍保留 error（终态补全语义不变）
        let merged = merge_turn(
            &json!({"turn_id": "t1", "status": "running"}),
            &json!({"turn_id": "t1", "status": "failed", "error": {"message": "late"}}),
        );
        assert_eq!(merged["error"]["message"], "late");

        // 终态 failed → 终态 failed（非 success 类）：保留 error
        let merged = merge_turn(
            &json!({"turn_id": "t1", "status": "failed", "error": {"message": "a"}}),
            &json!({"turn_id": "t1", "status": "failed"}),
        );
        assert_eq!(merged["error"]["message"], "a", "同终态不得清空既有 error");
    }

    #[test]
    fn c2_3_tool_calls_without_id_merge_by_name_and_arguments() {
        let existing = json!({
            "turn_id": "t1",
            "status": "running",
            "tool_calls": [
                {"tool_name": "parse", "status": "success", "arguments": {"q": 1}, "output": "ok"},
                {"tool_name": "fetch", "status": "success", "arguments": {"u": "x"}, "output": "ok"}
            ]
        });
        let incoming = json!({"turn_id": "t1", "status": "failed", "tool_calls": [
            {"tool_name": "parse", "status": "error", "arguments": {"q": 1}, "error": {"message": "解析失败"}}
        ]});
        let merged = merge_turn(&existing, &incoming);
        let calls = merged["tool_calls"].as_array().expect("tool_calls");
        assert_eq!(calls.len(), 2, "[L3A-F5] 无 tool_call_id 的调用必须按签名合并不追加");
        assert_eq!(calls[0]["status"], "error");
        assert_eq!(calls[0]["error"]["message"], "解析失败");
        assert_eq!(calls[0]["output"], "ok", "字段级合并保留旧 output");

        // 重复重报同一无 id 调用：多轮后条数不增长
        let mut running = merged.clone();
        for round in 0..5 {
            running = merge_turn(&running, &incoming);
            assert_eq!(
                running["tool_calls"].as_array().map(Vec::len),
                Some(2),
                "[L3A-F5] 第 {round} 轮重复上报不得无界追加"
            );
        }

        // 不同 arguments ⇒ 视为不同调用，追加
        let other = json!({"turn_id": "t1", "status": "failed", "tool_calls": [
            {"tool_name": "parse", "status": "error", "arguments": {"q": 2}}
        ]});
        let merged = merge_turn(&existing, &other);
        assert_eq!(
            merged["tool_calls"].as_array().map(Vec::len),
            Some(3),
            "签名不同（arguments 不同）必须追加"
        );
    }

    #[test]
    fn session_id_source_follows_payload_or_falls_back_to_thread_id() {
        assert_eq!(
            session_id_source_of(&json!({"session_id": "s1"})),
            "session_id"
        );
        assert_eq!(
            session_id_source_of(&json!({"session_id": null, "thread_id": "t1"})),
            "thread_id",
            "session_id 为 null 时必须回退 thread_id"
        );
        assert_eq!(
            session_id_source_of(&json!({"session_id": "", "thread_id": "t1"})),
            "thread_id",
            "空串视同缺失"
        );
        assert_eq!(
            session_id_source_of(&json!({"session_id_source": "thread_id", "session_id": "s1"})),
            "thread_id",
            "上报方显式标注优先"
        );
    }
}
