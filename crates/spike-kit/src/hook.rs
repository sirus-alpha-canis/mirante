use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Subset of the JSON Claude Code writes to a hook's stdin. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub ts_ms: u64,
    #[serde(default)]
    pub task_id: Option<String>,
    pub event: HookEvent,
}

pub const PLAIN_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
    "PreCompact",
];
pub const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PermissionRequest"];

/// Settings file passed with `claude --settings <file>`: registers `hook_cmd` for every
/// event and allows only the listed tool patterns. Global ~/.claude config is untouched.
pub fn settings_json(hook_cmd: &str, allow: &[&str]) -> Value {
    let handler = json!([{ "type": "command", "command": hook_cmd }]);
    let mut hooks = serde_json::Map::new();
    for name in PLAIN_EVENTS {
        hooks.insert(name.to_string(), json!([{ "hooks": handler.clone() }]));
    }
    for name in TOOL_EVENTS {
        hooks.insert(name.to_string(), json!([{ "matcher": "*", "hooks": handler.clone() }]));
    }
    json!({ "hooks": Value::Object(hooks), "permissions": { "allow": allow } })
}

/// What `mirante-hook` appends to the log: a LoggedEvent, or the raw text if unparseable.
pub fn logged_from_stdin(raw: &str, task_id: Option<String>, ts_ms: u64) -> Value {
    match serde_json::from_str::<HookEvent>(raw) {
        Ok(event) => serde_json::to_value(LoggedEvent { ts_ms, task_id, event }).unwrap_or(Value::Null),
        Err(_) => json!({ "ts_ms": ts_ms, "task_id": task_id, "raw": raw }),
    }
}

pub fn load_events(lines: &[Value], task_id: Option<&str>) -> Vec<LoggedEvent> {
    let mut events: Vec<LoggedEvent> = lines
        .iter()
        .filter_map(|v| serde_json::from_value::<LoggedEvent>(v.clone()).ok())
        .filter(|e| task_id.is_none_or(|t| e.task_id.as_deref() == Some(t)))
        .collect();
    // Concurrent hook processes can append out of order; stable sort keeps ties in file order.
    events.sort_by_key(|e| e.ts_ms);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_payload_and_ignores_unknown_fields() {
        let raw = r#"{"hook_event_name":"Stop","session_id":"s1","transcript_path":"/t.jsonl","stop_hook_active":false}"#;
        let e: LoggedEvent = serde_json::from_value(logged_from_stdin(raw, Some("t1".into()), 42)).unwrap();
        assert_eq!(e.event.hook_event_name, "Stop");
        assert_eq!(e.event.session_id.as_deref(), Some("s1"));
        assert_eq!(e.event.transcript_path.as_deref(), Some("/t.jsonl"));
        assert_eq!(e.task_id.as_deref(), Some("t1"));
        assert_eq!(e.ts_ms, 42);
    }

    #[test]
    fn keeps_raw_text_when_payload_is_not_a_hook_event() {
        let v = logged_from_stdin("not json", None, 7);
        assert_eq!(v["raw"], "not json");
        assert_eq!(v["ts_ms"], 7);
    }

    #[test]
    fn settings_register_every_event_with_the_hook_command() {
        let s = settings_json("/bin/mirante-hook", &["Bash(sleep:*)"]);
        for name in PLAIN_EVENTS.iter().chain(TOOL_EVENTS.iter()) {
            assert_eq!(s["hooks"][*name][0]["hooks"][0]["command"], "/bin/mirante-hook", "{name}");
        }
        assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(s["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(s["permissions"]["allow"][0], "Bash(sleep:*)");
    }

    #[test]
    fn load_events_filters_by_task_and_skips_raw_lines() {
        let lines = vec![
            logged_from_stdin(r#"{"hook_event_name":"Stop"}"#, Some("a".into()), 1),
            logged_from_stdin(r#"{"hook_event_name":"Stop"}"#, Some("b".into()), 2),
            logged_from_stdin("garbage", Some("a".into()), 3),
        ];
        assert_eq!(load_events(&lines, Some("a")).len(), 1);
        assert_eq!(load_events(&lines, None).len(), 2);
    }

    #[test]
    fn load_events_sorts_by_timestamp_stably() {
        let lines = vec![
            logged_from_stdin(r#"{"hook_event_name":"Stop"}"#, None, 9),
            logged_from_stdin(r#"{"hook_event_name":"SessionStart"}"#, None, 1),
            logged_from_stdin(r#"{"hook_event_name":"PreToolUse"}"#, None, 5),
            logged_from_stdin(r#"{"hook_event_name":"PostToolUse"}"#, None, 5),
        ];
        let names: Vec<(u64, String)> =
            load_events(&lines, None).into_iter().map(|e| (e.ts_ms, e.event.hook_event_name)).collect();
        assert_eq!(
            names,
            vec![
                (1, "SessionStart".to_string()),
                (5, "PreToolUse".to_string()),
                (5, "PostToolUse".to_string()),
                (9, "Stop".to_string())
            ]
        );
    }
}
