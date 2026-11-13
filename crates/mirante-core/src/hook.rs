use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// Subset of the JSON Claude Code writes to a hook's stdin. Unknown fields are ignored.
///
/// Field names follow the hooks reference (<https://code.claude.com/docs/en/hooks>):
/// - common to every event: `session_id`, `transcript_path`, `cwd`, `permission_mode`,
///   `hook_event_name` (plus `prompt_id`, `effort`, ... that Mirante does not read);
/// - `UserPromptSubmit`: `prompt`; `Notification`: `message`; `SessionStart`: `source`
///   (`startup` | `resume` | `clear` | `compact` | `fork`);
/// - tool events (`PreToolUse`, `PostToolUse`, `PermissionRequest`, ...): `tool_name`,
///   `tool_input`, `tool_use_id` (the docs list `tool_use_id` for Pre/PostToolUse; it is
///   optional everywhere here);
/// - agent identity: `agent_id` (the subagent / in-process teammate the hook fired in) and
///   `agent_type` (agent name). `agent_type` is ALSO present on the main thread when the
///   session runs with `--agent`, so only `agent_id` marks a side agent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    /// `SubagentStop`: the side agent's own transcript (`<dir>/<session>/subagents/
    /// agent-<id>.jsonl`). Checked against the expected path before any read.
    #[serde(default)]
    pub agent_transcript_path: Option<String>,
    /// `Stop` / `SubagentStop`: the agent's final text.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

impl HookEvent {
    /// Parses one hook payload. Errors are returned, never swallowed into an empty event.
    pub fn parse(raw: &str) -> Result<HookEvent> {
        Ok(serde_json::from_str(raw)?)
    }

    pub fn named(name: &str) -> HookEvent {
        HookEvent { hook_event_name: name.to_string(), ..HookEvent::default() }
    }

    /// True when a subagent / side agent emitted the event: the payload names an `agent_id`,
    /// or the event is `SubagentStart` / `SubagentStop` (a side agent's lifecycle, never the
    /// main turn's). `agent_type` alone is the main thread of an `--agent` session.
    pub fn is_side_agent(&self) -> bool {
        self.agent_id.is_some() || matches!(self.hook_event_name.as_str(), "SubagentStart" | "SubagentStop")
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

    /// Field names from https://code.claude.com/docs/en/hooks (common fields `cwd`,
    /// `permission_mode`; `prompt` on UserPromptSubmit; `tool_use_id` on tool events).
    #[test]
    fn parses_documented_common_and_per_event_fields() {
        let raw = r#"{"hook_event_name":"UserPromptSubmit","session_id":"s1","transcript_path":"/t.jsonl",
            "cwd":"/repo","permission_mode":"default","prompt":"/review","prompt_id":"p-1"}"#;
        let e = HookEvent::parse(raw).unwrap();
        assert_eq!(e.cwd.as_deref(), Some("/repo"));
        assert_eq!(e.permission_mode.as_deref(), Some("default"));
        assert_eq!(e.prompt.as_deref(), Some("/review"));
        let raw = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{},"tool_use_id":"toolu_01"}"#;
        assert_eq!(HookEvent::parse(raw).unwrap().tool_use_id.as_deref(), Some("toolu_01"));
    }

    /// With `--agent`, the main thread carries `agent_type` but no `agent_id`: not a side agent.
    #[test]
    fn agent_type_alone_is_the_main_thread() {
        let e = HookEvent::parse(r#"{"hook_event_name":"PreToolUse","agent_type":"reviewer"}"#).unwrap();
        assert!(!e.is_side_agent());
    }

    #[test]
    fn subagent_lifecycle_events_are_side_agent_events() {
        assert!(HookEvent::named("SubagentStop").is_side_agent());
        assert!(HookEvent::named("SubagentStart").is_side_agent());
        assert!(!HookEvent::named("Stop").is_side_agent());
    }

    #[test]
    fn invalid_payload_is_an_error_not_an_empty_event() {
        assert!(HookEvent::parse("not json").is_err());
        assert!(HookEvent::parse(r#"{"session_id":"s1"}"#).is_err());
    }

    /// S1 (Claude 2.1.285): `SubagentStop` names the side file and carries the final text.
    #[test]
    fn subagent_stop_carries_its_side_file_and_final_text() {
        let raw = r#"{"hook_event_name":"SubagentStop","agent_id":"a7386d060a40b63d6","agent_type":"general-purpose",
            "agent_transcript_path":"/p/s/subagents/agent-a7386d060a40b63d6.jsonl","last_assistant_message":"done",
            "background_tasks":[{"id":"x","status":"running"}]}"#;
        let e = HookEvent::parse(raw).unwrap();
        assert!(e.is_side_agent());
        assert_eq!(e.agent_transcript_path.as_deref(), Some("/p/s/subagents/agent-a7386d060a40b63d6.jsonl"));
        assert_eq!(e.last_assistant_message.as_deref(), Some("done"));
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
