use crate::models::{Event, Issue, IssueStatus};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// 一次指纹重索引所需的全部事件派生信息。
/// 由上层（ingest crate）从事件 payload 计算后通过闭包注入，
/// 保证重索引与实时上报走同一套指纹逻辑。
#[derive(Debug, Clone)]
pub struct FingerprintOut {
    pub fingerprint: String,
    pub title: String,
    pub culprit: Option<String>,
    pub platform: String,
    pub project: Option<String>,
}

/// 幂等重索引规划：以“新指纹”为唯一分组键，重建 issue 归属。
/// 规则（可预测、可复跑）：
/// 1. 同一新指纹的所有事件归并为一只 issue；
/// 2. 若某只旧 issue 的全部事件都落入同一新组（1:1 映射），复用其 id 并保留人工状态
///    （status / assigned_to），其余行只做计数与时间戳刷新；
/// 3. 新组若由多只旧 issue 合并而来，或由单只旧 issue 拆分产生（旧 issue 事件跨多组），
///    则生成全新 id，status 置 unresolved；
/// 4. 未被复用且不再被任何事件引用的旧 issue 进入删除列表；
/// 5. payload 无法解析指纹的事件（闭包返回 None）保持原位，其 issue 不会被删除。
#[derive(Debug, Default)]
pub struct ReindexPlan {
    /// 需要写入（INSERT 或 UPDATE）的 issue 全量行；id 已定好（复用或新生成）。
    pub issue_rows: Vec<Issue>,
    /// 复用既有 issue 的 id 集合（用于 UPDATE 而非 INSERT）。
    pub reused_ids: HashSet<String>,
    /// 事件重挂载映射 (event_id -> issue_id)。
    pub assignments: Vec<(String, String)>,
    /// 需删除的旧 issue id。
    pub delete_ids: Vec<String>,
    /// 指纹解析失败、原地保留的事件数。
    pub skipped_events: usize,
}

pub fn plan_reindex(
    events: &[Event],
    issues: &[Issue],
    fingerprint_of: &(dyn for<'a> Fn(&'a serde_json::Value) -> Option<FingerprintOut> + Sync),
) -> ReindexPlan {
    // 1. 计算每个事件的新指纹，跳过无法解析的
    let mut by_fp: BTreeMap<String, Vec<usize>> = BTreeMap::new(); // fp -> event indices
    let mut infos: HashMap<usize, FingerprintOut> = HashMap::new();
    let mut skipped = 0usize;
    for (i, ev) in events.iter().enumerate() {
        match fingerprint_of(&ev.payload) {
            Some(info) => {
                by_fp.entry(info.fingerprint.clone()).or_default().push(i);
                infos.insert(i, info);
            }
            None => skipped += 1,
        }
    }

    // 2. 统计每只旧 issue 参与了多少个新组（用于判断 1:1 复用）
    let mut old_issue_groups: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (fp, indices) in &by_fp {
        for &i in indices {
            let issue_id = events[i].issue_id.as_str();
            old_issue_groups
                .entry(issue_id)
                .or_default()
                .insert(fp.as_str());
        }
    }

    let old_issue_by_id: HashMap<&str, &Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut plan = ReindexPlan {
        skipped_events: skipped,
        ..Default::default()
    };

    // 3. 逐组生成 issue 行
    for (fp, indices) in &by_fp {
        let group_events: Vec<&Event> = indices.iter().map(|&i| &events[i]).collect();

        // 组内事件原本所属的旧 issue 集合
        let mut contributing: Vec<&str> = Vec::new();
        {
            let mut seen = HashSet::new();
            for ev in &group_events {
                if seen.insert(ev.issue_id.as_str()) {
                    contributing.push(ev.issue_id.as_str());
                }
            }
        }

        // 判定 1:1 复用：组内事件全部来自同一条旧 issue，且该旧 issue 的事件也全部在本组
        let reusable_old_id = if contributing.len() == 1 {
            let cid = contributing[0];
            if old_issue_groups.get(cid).map(|s| s.len()) == Some(1) {
                Some(cid)
            } else {
                None
            }
        } else {
            None
        };

        let (issue_id, status, assigned_to) = match reusable_old_id {
            Some(cid) => {
                let old = old_issue_by_id[cid];
                (cid.to_string(), old.status.clone(), old.assigned_to.clone())
            }
            None => (Uuid::new_v4().to_string(), IssueStatus::Unresolved, None),
        };

        // 组内最早事件（按 created_at）作为 title/culprit/platform/project 基准
        let first_idx = indices
            .iter()
            .min_by(|&a, &b| events[*a].created_at.cmp(&events[*b].created_at))
            .copied()
            .unwrap_or(indices[0]);
        let info = &infos[&first_idx];

        let first_seen = group_events
            .iter()
            .map(|e| e.created_at)
            .min()
            .unwrap_or(events[first_idx].created_at);
        let last_seen = group_events
            .iter()
            .map(|e| e.created_at)
            .max()
            .unwrap_or(events[first_idx].created_at);
        let last_release = group_events.iter().filter_map(|e| e.release.clone()).max();

        plan.issue_rows.push(Issue {
            id: issue_id.clone(),
            fingerprint: fp.clone(),
            title: info.title.clone(),
            culprit: info.culprit.clone(),
            platform: info.platform.clone(),
            status,
            assigned_to,
            count: group_events.len() as i64,
            last_release,
            project: info.project.clone(),
            first_seen_at: first_seen,
            last_seen_at: last_seen,
        });

        if reusable_old_id.is_some() {
            plan.reused_ids.insert(issue_id.clone());
        }

        for &i in indices {
            plan.assignments
                .push((events[i].id.clone(), issue_id.clone()));
        }
    }

    // 4. 删除列表：未被复用、且不承载任何（可解析或不可解析）事件的旧 issue
    let reused: HashSet<&str> = plan.reused_ids.iter().map(|s| s.as_str()).collect();
    let assigned_issue_ids: HashSet<&str> = plan
        .assignments
        .iter()
        .map(|(_, iid)| iid.as_str())
        .collect();
    for issue in issues {
        let still_hosts_skipped = events
            .iter()
            .any(|e| e.issue_id == issue.id && fingerprint_of(&e.payload).is_none());
        if !reused.contains(issue.id.as_str())
            && !assigned_issue_ids.contains(issue.id.as_str())
            && !still_hosts_skipped
        {
            plan.delete_ids.push(issue.id.clone());
        }
    }

    plan
}

/// 重索引执行摘要（供运维与日志输出）。
#[derive(Debug, Default, Clone)]
pub struct ReindexSummary {
    pub issues_created: usize,
    pub issues_reused: usize,
    pub issues_deleted: usize,
    pub events_moved: usize,
    pub events_skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn event(id: &str, issue_id: &str, fp: &str, minute: u32) -> Event {
        Event {
            id: id.to_string(),
            issue_id: issue_id.to_string(),
            payload: json!({"fp_hint": fp}),
            release: Some("0.2.49".to_string()),
            environment: None,
            created_at: Utc.with_ymd_and_hms(2026, 10, 8, 12, minute, 0).unwrap(),
        }
    }

    fn issue(id: &str, fp: &str, status: IssueStatus) -> Issue {
        let now = Utc::now();
        Issue {
            id: id.to_string(),
            fingerprint: fp.to_string(),
            title: "t".to_string(),
            culprit: None,
            platform: "rust".to_string(),
            status,
            assigned_to: None,
            count: 1,
            last_release: None,
            project: None,
            first_seen_at: now,
            last_seen_at: now,
        }
    }

    fn fp_hint_of(payload: &serde_json::Value) -> Option<FingerprintOut> {
        payload
            .get("fp_hint")
            .and_then(|v| v.as_str())
            .map(|fp| FingerprintOut {
                fingerprint: fp.to_string(),
                title: format!("err:{}", fp),
                culprit: None,
                platform: "rust".to_string(),
                project: None,
            })
    }

    #[test]
    fn merges_issues_with_same_new_fingerprint_and_deletes_originals() {
        let events = vec![
            event("e1", "a", "same-fp", 1),
            event("e2", "b", "same-fp", 2),
        ];
        let issues = vec![
            issue("a", "old-a", IssueStatus::Unresolved),
            issue("b", "old-b", IssueStatus::Unresolved),
        ];
        let plan = plan_reindex(&events, &issues, &fp_hint_of);

        assert_eq!(plan.issue_rows.len(), 1);
        assert_eq!(plan.assignments.len(), 2);
        assert!(plan.reused_ids.is_empty(), "merge 场景不得复用任何旧 id");
        let new_id = &plan.issue_rows[0].id;
        assert!(plan.assignments.iter().all(|(_, iid)| iid == new_id));
        assert!(plan.delete_ids.contains(&"a".to_string()));
        assert!(plan.delete_ids.contains(&"b".to_string()));
        assert_eq!(plan.issue_rows[0].count, 2);
    }

    #[test]
    fn reuses_issue_when_one_to_one_mapping() {
        let events = vec![event("e1", "x", "fp-x", 1), event("e2", "x", "fp-x", 2)];
        let issues = vec![issue("x", "fp-x", IssueStatus::Resolved)];
        let plan = plan_reindex(&events, &issues, &fp_hint_of);

        assert_eq!(plan.issue_rows.len(), 1);
        assert_eq!(plan.issue_rows[0].id, "x");
        assert!(plan.reused_ids.contains("x"));
        // 人工状态保留
        assert_eq!(plan.issue_rows[0].status, IssueStatus::Resolved);
        assert!(plan.delete_ids.is_empty());
        assert_eq!(plan.issue_rows[0].count, 2);
    }

    #[test]
    fn splits_issue_when_events_map_to_different_groups() {
        let events = vec![event("e1", "x", "fp-1", 1), event("e2", "x", "fp-2", 2)];
        let issues = vec![issue("x", "old", IssueStatus::Unresolved)];
        let plan = plan_reindex(&events, &issues, &fp_hint_of);

        assert_eq!(plan.issue_rows.len(), 2);
        assert!(plan.reused_ids.is_empty());
        assert!(plan.delete_ids.contains(&"x".to_string()));
    }

    #[test]
    fn keeps_unparseable_events_and_their_issue() {
        let events = vec![
            event("e1", "x", "fp-1", 1),
            Event {
                id: "e2".to_string(),
                issue_id: "y".to_string(),
                payload: json!({}),
                release: None,
                environment: None,
                created_at: Utc.with_ymd_and_hms(2026, 10, 8, 12, 2, 0).unwrap(),
            },
        ];
        let issues = vec![
            issue("x", "old-x", IssueStatus::Unresolved),
            issue("y", "old-y", IssueStatus::Unresolved),
        ];
        let plan = plan_reindex(&events, &issues, &fp_hint_of);

        assert_eq!(plan.skipped_events, 1);
        // y 因承载不可解析事件而保留
        assert!(!plan.delete_ids.contains(&"y".to_string()));
        // x 的唯一可解析事件 1:1 归组，仍被复用（不被删除）
        assert!(plan.reused_ids.contains("x"));
        assert!(!plan.delete_ids.contains(&"x".to_string()));
    }

    #[test]
    fn preserves_first_seen_from_earliest_event() {
        let events = vec![event("e1", "x", "fp", 1), event("e2", "x", "fp", 5)];
        let issues = vec![issue("x", "fp", IssueStatus::Unresolved)];
        let plan = plan_reindex(&events, &issues, &fp_hint_of);
        let row = &plan.issue_rows[0];
        assert_eq!(row.first_seen_at, events[0].created_at);
        assert_eq!(row.last_seen_at, events[1].created_at);
    }
}
