//! Screen text builders: pure functions from state snapshots to
//! display lines. The full-screen UI wraps these lines in panels;
//! line mode prints them verbatim, so both modes show the same
//! facts (the phlow precedent: one execute path, two renderers).
//!
//! The Editor screen's honesty contract lives here: [`EditorView`]
//! distinguishes live data from a stale cache at the type level,
//! and the stale rendering says so in its first line.

use serde_json::Value;

/// What the Editor screen may show, by freshness.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorView {
    /// Fetched while the editor was connected; safe to call live.
    Live {
        /// `tackle_editor_state` payload.
        state: Value,
        /// `tackle_scene_hierarchy` payload.
        hierarchy: Value,
    },
    /// A cache from an earlier connection, kept for reference after
    /// a disconnect. Rendered with an explicit stale banner; never
    /// presented as live.
    Stale {
        /// Last hierarchy payload fetched while connected.
        hierarchy: Value,
    },
    /// No editor session and nothing cached.
    Disconnected,
    /// Nothing fetched yet this session.
    Empty,
}

/// Fleet members shown in the Fleet panel. Reachability is honest:
/// rung 1 probes nothing outbound.
pub const FLEET_MEMBERS: &[(&str, &str)] = &[
    ("tackle", "self"),
    ("bloch", "not probed in rung 1"),
    ("theodolite", "not probed in rung 1"),
    ("lumen", "not probed in rung 1"),
    ("phlow", "not probed in rung 1"),
];

/// Overview lines from a facade status value.
pub fn overview_lines(status: &Value, tool_count: usize, rule_count: usize) -> Vec<String> {
    let get = |key: &str| status.get(key).cloned().unwrap_or(Value::Null);
    let text = |value: &Value| match value {
        Value::String(s) => s.clone(),
        Value::Null => "—".to_string(),
        other => other.to_string(),
    };
    vec![
        format!("member: {}", text(&get("member"))),
        format!("version: {}", text(&get("version"))),
        format!("server: {}", text(&get("server"))),
        format!(
            "editor: {}",
            if get("editor_connected").as_bool() == Some(true) {
                "connected"
            } else {
                "NOT connected"
            }
        ),
        format!("tools: {tool_count}"),
        format!("trigger rules: {rule_count} (none fire outbound in rung 1)"),
        "gate: U2 writes are plan-mode only in rung 1".to_string(),
    ]
}

/// Editor lines, honouring the freshness of `view`.
pub fn editor_lines(view: &EditorView) -> Vec<String> {
    match view {
        EditorView::Live { state, hierarchy } => {
            let mut lines = vec![
                "editor: LIVE".to_string(),
                format!(
                    "scene: {}",
                    state
                        .get("activeScene")
                        .and_then(Value::as_str)
                        .unwrap_or("—")
                ),
                format!(
                    "unity: {}",
                    state
                        .get("unityVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("—")
                ),
            ];
            lines.extend(hierarchy_lines(hierarchy));
            lines
        }
        EditorView::Stale { hierarchy } => {
            let mut lines = vec![
                "editor: DISCONNECTED — showing last cached scene (STALE, not live)".to_string(),
            ];
            lines.extend(hierarchy_lines(hierarchy));
            lines
        }
        EditorView::Disconnected => vec![
            "editor: DISCONNECTED".to_string(),
            "no cached scene this session; nothing is presented as live".to_string(),
        ],
        EditorView::Empty => vec![
            "editor: not fetched yet".to_string(),
            "press r (or `refresh` in line mode) to read the U1 read core".to_string(),
        ],
    }
}

fn hierarchy_lines(hierarchy: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "hierarchy: {} node(s)",
        hierarchy
            .get("totalCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    )];
    if let Some(nodes) = hierarchy.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            let depth = node.get("depth").and_then(Value::as_u64).unwrap_or(0);
            let indent = "  ".repeat(usize::try_from(depth).unwrap_or(0).min(16));
            lines.push(format!(
                "{indent}{}",
                node.get("path")
                    .or_else(|| node.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("?")
            ));
        }
    }
    lines
}

/// Workflow list lines: one per definition, `>` marks selection,
/// the class stamp makes read vs write unmissable.
pub fn workflow_lines(
    defs: &[(String, String, tackle::decide::Class)],
    selected: usize,
    mode: &str,
    staged_tool: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![format!("mode: {mode}")];
    for (index, (tool, label, class)) in defs.iter().enumerate() {
        let marker = if index == selected { ">" } else { " " };
        let class_text = match class {
            tackle::decide::Class::Read => "U1 read",
            tackle::decide::Class::Write => "U2 write (plan only)",
            tackle::decide::Class::Destructive => "U2 destructive (plan only)",
        };
        let staged = if staged_tool == Some(tool.as_str()) {
            " [staged]"
        } else {
            ""
        };
        lines.push(format!("{marker} {tool} — {label} [{class_text}]{staged}"));
    }
    lines
}

/// Fleet lines: members, rules with enable state, audit tail.
pub fn fleet_lines(rules: &[crate::triggers::TriggerRule], audit_tail: &[Value]) -> Vec<String> {
    let mut lines = vec!["fleet members:".to_string()];
    for (name, state) in FLEET_MEMBERS {
        lines.push(format!("  {name}: {state}"));
    }
    lines.push(format!("trigger rules: {}", rules.len()));
    for rule in rules {
        lines.push(format!(
            "  {} → {}:{} [{}] enabled={}",
            rule.id, rule.target_mcp, rule.target_tool, rule.mode, rule.enabled
        ));
    }
    if rules.is_empty() {
        lines.push("  (no outbound rules in rung 1 — nothing can fire)".to_string());
    }
    lines.push("trigger audit (tail):".to_string());
    if audit_tail.is_empty() {
        lines.push("  (empty)".to_string());
    }
    for entry in audit_tail {
        lines.push(format!("  {entry}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_overview_reports_disconnected_honestly() {
        let status = json!({"member": "tackle", "editor_connected": false});
        let lines = overview_lines(&status, 18, 0);
        assert!(lines.iter().any(|l| l.contains("NOT connected")));
    }

    #[test]
    fn adv_stale_view_never_claims_live() {
        let view = EditorView::Stale {
            hierarchy: json!({"totalCount": 1, "nodes": []}),
        };
        let lines = editor_lines(&view);
        assert!(lines[0].contains("STALE"));
        assert!(lines[0].contains("DISCONNECTED"));
        assert!(!lines.iter().any(|l| l == "editor: LIVE"));
    }

    #[test]
    fn ok_live_view_labels_live_and_lists_nodes() {
        let view = EditorView::Live {
            state: json!({"activeScene": "Main", "unityVersion": "6000.0.0f1"}),
            hierarchy: json!({"totalCount": 1, "nodes": [
                {"path": "Root", "depth": 0}
            ]}),
        };
        let lines = editor_lines(&view);
        assert_eq!(lines[0], "editor: LIVE");
        assert!(lines.iter().any(|l| l.contains("Root")));
    }

    #[test]
    fn adv_disconnected_view_shows_no_scene() {
        let lines = editor_lines(&EditorView::Disconnected);
        assert!(lines.iter().all(|l| !l.contains("hierarchy:")));
    }

    #[test]
    fn ok_fleet_with_zero_rules_says_nothing_fires() {
        let lines = fleet_lines(&[], &[]);
        assert!(lines.iter().any(|l| l.contains("nothing can fire")));
    }
}
