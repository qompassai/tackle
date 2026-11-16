//! The cross-MCP trigger layer's local half (DESIGN §D4): rules
//! are data, listed in the Fleet panel, and every evaluation is
//! audited. Rung 1 ships **no outbound rules**: `triggers.toml`
//! carries zero `[[rule]]` entries, so nothing can fire tonight.
//! What exists and is tested is the machinery the Fleet panel
//! shows — rule loading, the bounded audit log, and the
//! idempotency key set that makes duplicate events drop.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use serde_json::{Value, json};

/// Most audit entries retained in memory (the panel tail).
pub const AUDIT_LINES_MAX: usize = 1_000;

/// Hard ceiling on trigger chain depth (DESIGN §D4).
pub const DEPTH_CEILING: u32 = 2;

/// One declared trigger rule (data, not code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRule {
    /// Stable rule id, e.g. `theodolite-diag-to-bloch-lesson`.
    pub id: String,
    /// BLOCH lane stamp, from birth (D-1030).
    pub origin_lane: String,
    /// BLOCH content label: `sfw` or `nsfw`.
    pub content_label: String,
    /// Event name that can fire the rule.
    pub on_event: String,
    /// Target MCP member name.
    pub target_mcp: String,
    /// Target tool on that member.
    pub target_tool: String,
    /// `auto` or `plan` (plan = a human approves in the TUI).
    pub mode: String,
    /// Chain depth this rule tolerates (clamped to the ceiling).
    pub max_depth: u32,
    /// Panel-controlled enable state; disabling needs no restart.
    pub enabled: bool,
}

/// Load rules from a `triggers.toml` file. A missing file is zero
/// rules (the rung-1 default), never an error; a malformed entry is
/// skipped, never half-parsed into a live rule. The parser accepts
/// the documented `[[rule]]` shape with `key = "value"` lines and
/// the `on = { event = "..." }` / `then = { ... }` inline tables.
pub fn load_rules(path: &Path) -> Vec<TriggerRule> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_rules(&text)
}

/// Parse the `triggers.toml` text shape. Pure; unit-tested.
pub fn parse_rules(text: &str) -> Vec<TriggerRule> {
    let mut rules = Vec::new();
    for block in text.split("[[rule]]").skip(1) {
        if let Some(rule) = parse_rule_block(block) {
            rules.push(rule);
        }
    }
    rules
}

fn quoted(block: &str, key: &str) -> Option<String> {
    for line in block.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key)
            && let Some(rest) = rest.trim_start().strip_prefix('=')
            && let Some(inner) = rest.trim().strip_prefix('"')
            && let Some(end) = inner.find('"')
        {
            return Some(inner[..end].to_string());
        }
    }
    None
}

fn inline_quoted(block: &str, table: &str, key: &str) -> Option<String> {
    // Find `table = { ... key = "value" ... }` on one line.
    for line in block.lines() {
        let line = line.trim();
        if line.starts_with(table)
            && line.contains('{')
            && let Some(pos) = line.find(key)
            && let Some(start) = line[pos + key.len()..].find('"')
        {
            let inner = &line[pos + key.len()..][start + 1..];
            if let Some(end) = inner.find('"') {
                return Some(inner[..end].to_string());
            }
        }
    }
    None
}

fn parse_rule_block(block: &str) -> Option<TriggerRule> {
    let id = quoted(block, "id")?;
    let target_mcp = inline_quoted(block, "then", "mcp")?;
    let target_tool = inline_quoted(block, "then", "tool")?;
    let on_event = inline_quoted(block, "on", "event")?;
    let max_depth = block
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("max_depth")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .and_then(|rest| rest.trim().parse::<u32>().ok())
        })
        .unwrap_or(1)
        .min(DEPTH_CEILING);
    Some(TriggerRule {
        id,
        origin_lane: quoted(block, "origin_lane").unwrap_or_else(|| "qompass".into()),
        content_label: quoted(block, "content_label").unwrap_or_else(|| "sfw".into()),
        on_event,
        target_mcp,
        target_tool,
        mode: quoted(block, "mode").unwrap_or_else(|| "plan".into()),
        max_depth,
        enabled: true,
    })
}

/// The bounded trigger audit: one JSON value per evaluation,
/// firing, refusal, or drop, each carrying the rule's birth
/// labels. Sequence numbers, not wall-clock, keep it deterministic.
#[derive(Debug, Default)]
pub struct AuditLog {
    entries: VecDeque<Value>,
    fired_keys: HashSet<String>,
    seq: u64,
}

impl AuditLog {
    /// An empty audit log.
    pub fn new() -> AuditLog {
        AuditLog::default()
    }

    /// Append one entry; the oldest is dropped past the cap.
    pub fn record(&mut self, kind: &str, rule_id: &str, detail: &str, rule: Option<&TriggerRule>) {
        self.seq = self.seq.saturating_add(1);
        let entry = json!({
            "seq": self.seq,
            "kind": kind,
            "rule": rule_id,
            "detail": detail,
            "origin_lane": rule.map_or("qompass", |r| r.origin_lane.as_str()),
            "content_label": rule.map_or("sfw", |r| r.content_label.as_str()),
        });
        self.entries.push_back(entry);
        while self.entries.len() > AUDIT_LINES_MAX {
            self.entries.pop_front();
        }
    }

    /// Mark a firing key as spent. Returns `false` when the key
    /// already fired — the duplicate is dropped and audited by the
    /// caller (DESIGN §D4 idempotency).
    pub fn try_mark_fired(&mut self, key: &str) -> bool {
        self.fired_keys.insert(key.to_string())
    }

    /// The newest `count` entries, oldest first.
    pub fn tail(&self, count: usize) -> Vec<Value> {
        self.entries
            .iter()
            .rev()
            .take(count)
            .rev()
            .cloned()
            .collect()
    }

    /// Total entries currently retained.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log retains nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[rule]]
id = "theodolite-diag-to-bloch-lesson"
origin_lane = "qompass"
content_label = "sfw"
on = { event = "diagnostics.completed", where = "error_count > 0" }
then = { mcp = "bloch", tool = "lesson_candidate", args_from = "event" }
mode = "auto"
max_depth = 1
"#;

    #[test]
    fn ok_sample_rule_parses_with_birth_labels() {
        let rules = parse_rules(SAMPLE);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].target_mcp, "bloch");
        assert_eq!(rules[0].target_tool, "lesson_candidate");
        assert_eq!(rules[0].on_event, "diagnostics.completed");
        assert_eq!(rules[0].origin_lane, "qompass");
        assert_eq!(rules[0].content_label, "sfw");
    }

    #[test]
    fn ok_comment_only_file_is_zero_rules() {
        assert!(parse_rules("# no outbound rules in rung 1\n").is_empty());
        assert!(load_rules(Path::new("/nonexistent/triggers.toml")).is_empty());
    }

    #[test]
    fn adv_malformed_rule_is_skipped_not_half_parsed() {
        assert!(parse_rules("[[rule]]\nid = \"x\"\n").is_empty());
    }

    #[test]
    fn adv_depth_is_clamped_to_ceiling() {
        let text = SAMPLE.replace("max_depth = 1", "max_depth = 99");
        assert_eq!(parse_rules(&text)[0].max_depth, DEPTH_CEILING);
    }

    #[test]
    fn ok_idempotency_key_fires_once() {
        let mut audit = AuditLog::new();
        assert!(audit.try_mark_fired("rule:event-hash"));
        assert!(!audit.try_mark_fired("rule:event-hash"));
    }

    #[test]
    fn adv_audit_is_bounded() {
        let mut audit = AuditLog::new();
        for index in 0..(AUDIT_LINES_MAX + 25) {
            audit.record("evaluated", "r", &format!("{index}"), None);
        }
        assert_eq!(audit.len(), AUDIT_LINES_MAX);
        assert_eq!(audit.tail(1)[0]["seq"], (AUDIT_LINES_MAX + 25) as u64);
    }
}
