use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// Subset of the JSON Claude Code writes to a hook's stdin. Unknown fields are ignored.
/// `agent_id` / `agent_type` are present when a subagent or side agent emitted the event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
}

impl HookEvent {
    /// Parses one hook payload. Errors are returned, never swallowed into an empty event.
    pub fn parse(raw: &str) -> Result<HookEvent> {
        Ok(serde_json::from_str(raw)?)
    }

    pub fn named(name: &str) -> HookEvent {
        HookEvent { hook_event_name: name.to_string(), ..HookEvent::default() }
    }

    /// True when a subagent / side agent emitted the event (the payload names an agent id).
    pub fn is_side_agent(&self) -> bool {
        self.agent_id.is_some()
    }

    /// Human-readable detail for a permission card: the Bash command, the file path,
    /// or the compact JSON of the tool input.
    pub fn permission_detail(&self) -> String {
        let Some(input) = &self.tool_input else {
            return String::new();
        };
        for key in ["command", "file_path", "path", "url"] {
            if let Some(s) = input[key].as_str() {
                return s.to_string();
            }
        }
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payload_with_agent_identity_and_ignores_unknown_fields() {
        let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash",
            "tool_input":{"command":"ls"},"agent_id":"a-7","agent_type":"general-purpose","extra":1}"#;
        let e = HookEvent::parse(raw).unwrap();
        assert_eq!(e.hook_event_name, "PreToolUse");
        assert_eq!(e.agent_id.as_deref(), Some("a-7"));
        assert_eq!(e.agent_type.as_deref(), Some("general-purpose"));
        assert!(e.is_side_agent());
        assert_eq!(e.permission_detail(), "ls");
    }

    #[test]
    fn main_agent_payload_has_no_agent_id() {
        let e = HookEvent::parse(r#"{"hook_event_name":"Stop","session_id":"s1"}"#).unwrap();
        assert!(!e.is_side_agent());
        assert_eq!(e, HookEvent { session_id: Some("s1".into()), ..HookEvent::named("Stop") });
    }

    #[test]
    fn invalid_payload_is_an_error_not_an_empty_event() {
        assert!(HookEvent::parse("not json").is_err());
        assert!(HookEvent::parse(r#"{"session_id":"s1"}"#).is_err());
    }

    #[test]
    fn permission_detail_falls_back_to_json() {
        let e = HookEvent {
            tool_input: Some(serde_json::json!({"file_path": "src/a.rs"})),
            ..HookEvent::named("PermissionRequest")
        };
        assert_eq!(e.permission_detail(), "src/a.rs");
        let e = HookEvent { tool_input: Some(serde_json::json!({"q": 1})), ..HookEvent::named("PermissionRequest") };
        assert_eq!(e.permission_detail(), r#"{"q":1}"#);
        assert_eq!(HookEvent::named("PermissionRequest").permission_detail(), "");
    }
}
