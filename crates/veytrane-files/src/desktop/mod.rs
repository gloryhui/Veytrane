//! Pure file-task projections shared by the GPUI desktop views.

use serde_json::Value;

/// Task page size used by the desktop list projection.
pub const TASK_PAGE_SIZE: usize = 128;

pub fn task_state(task: &Value) -> &'static str {
    match task["state"].as_str().unwrap_or("") {
        "scanning" => "扫描中",
        "queued" => "排队中",
        "connecting" => "连接中",
        "negotiating" => "协商中",
        "transferring" => "传输中",
        "pausing" => "正在暂停",
        "paused" => "已暂停",
        "finalizing" => "正在发布确认",
        "completed" => "已完成",
        "interrupted" => "中断待继续",
        "failed" => "失败",
        _ => "未知",
    }
}

pub fn matches_task(
    task: &Value,
    query: &str,
    direction: u8,
    state: u8,
    peer: Option<&str>,
    devices: &Value,
) -> bool {
    let task_peer = task["peer"].as_str().unwrap_or("");
    if peer.is_some_and(|peer| peer != task_peer) {
        return false;
    }
    if direction != 0
        && task["direction"].as_str() != Some(if direction == 1 { "send" } else { "receive" })
    {
        return false;
    }
    let phase = task["state"].as_str().unwrap_or("");
    if match state {
        1 => !matches!(
            phase,
            "scanning"
                | "queued"
                | "connecting"
                | "negotiating"
                | "transferring"
                | "pausing"
                | "finalizing"
        ),
        2 => !matches!(phase, "paused" | "interrupted"),
        3 => phase != "completed",
        4 => phase != "failed",
        _ => false,
    } {
        return false;
    }
    let query = query.trim().to_lowercase();
    query.is_empty()
        || task["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains(&query)
        || task_peer.contains(&query)
        || devices.as_array().into_iter().flatten().any(|d| {
            d["node_id"].as_str() == Some(task_peer)
                && d["display_name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskPagination {
    pub total: u64,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

impl TaskPagination {
    pub fn from_value(task_page: &Value) -> Self {
        Self {
            total: task_page["total"].as_u64().unwrap_or(0),
            offset: task_page["offset"].as_u64().unwrap_or(0) as usize,
            next_offset: task_page["next_offset"].as_u64().map(|n| n as usize),
        }
    }

    pub fn previous_offset(&self) -> usize {
        self.offset.saturating_sub(TASK_PAGE_SIZE)
    }
}

pub fn preview_summary(preview: &Value, format_bytes: impl FnOnce(f64) -> String) -> String {
    format!(
        "{} · 新增 {} · 变化 {} · 不变 {} · 待传 {}",
        preview["name"].as_str().unwrap_or("目录"),
        preview["added"],
        preview["changed"],
        preview["unchanged"],
        format_bytes(preview["transfer_bytes"].as_u64().unwrap_or(0) as f64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_keep_direction_state_and_device_identity_distinct() {
        let task =
            json!({"name":"report.txt","peer":"abc","direction":"receive","state":"completed"});
        let devices = json!([{"node_id":"abc","display_name":"Home PC"}]);
        assert!(matches_task(&task, "home", 2, 3, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 1, 3, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 2, 1, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 2, 3, Some("other"), &devices));
    }

    #[test]
    fn task_state_covers_wire_states_and_falls_back_to_unknown() {
        let cases = [
            ("scanning", "扫描中"),
            ("queued", "排队中"),
            ("connecting", "连接中"),
            ("negotiating", "协商中"),
            ("transferring", "传输中"),
            ("pausing", "正在暂停"),
            ("paused", "已暂停"),
            ("finalizing", "正在发布确认"),
            ("completed", "已完成"),
            ("interrupted", "中断待继续"),
            ("failed", "失败"),
            ("new-state", "未知"),
        ];
        for (state, expected) in cases {
            assert_eq!(task_state(&json!({ "state": state })), expected);
        }
        assert_eq!(task_state(&json!({})), "未知");
    }

    #[test]
    fn task_pagination_projects_offsets_and_previous_page_floor() {
        assert_eq!(
            TaskPagination::from_value(&Value::Null),
            TaskPagination::default()
        );
        let page = TaskPagination::from_value(&json!({
            "total": 300,
            "offset": 256,
            "next_offset": 384
        }));
        assert_eq!(
            page,
            TaskPagination {
                total: 300,
                offset: 256,
                next_offset: Some(384)
            }
        );
        assert_eq!(page.previous_offset(), 128);
        assert_eq!(
            TaskPagination {
                total: 0,
                offset: 64,
                next_offset: None
            }
            .previous_offset(),
            0
        );
    }

    #[test]
    fn preview_summary_keeps_field_order_and_name_fallback() {
        assert_eq!(
            preview_summary(
                &json!({
                    "name": "photos",
                    "added": 2,
                    "changed": 3,
                    "unchanged": 4,
                    "transfer_bytes": 1536
                }),
                |value| format!("{value:.0} B")
            ),
            "photos · 新增 2 · 变化 3 · 不变 4 · 待传 1536 B"
        );
        assert_eq!(
            preview_summary(&json!({}), |value| format!("{value:.0} B")),
            "目录 · 新增 null · 变化 null · 不变 null · 待传 0 B"
        );
    }
}
