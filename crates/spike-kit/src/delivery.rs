use serde_json::Value;

use crate::hook::LoggedEvent;

/// How a second message, sent while the first turn was still running, was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Acted on before the first turn ended (true mid-run steering).
    Steered,
    /// Acted on only after the first turn ended.
    Queued,
    /// Never acted on.
    Never,
}

fn decide(marker_at: Option<u64>, first_end: u64) -> Delivery {
    match marker_at {
        None => Delivery::Never,
        Some(t) if t < first_end => Delivery::Steered,
        Some(_) => Delivery::Queued,
    }
}

/// Interactive PTY session: was the 2nd UserPromptSubmit before the 1st Stop?
pub fn classify_hooks(events: &[LoggedEvent]) -> Delivery {
    let prompts: Vec<u64> = events
        .iter()
        .filter(|e| e.event.hook_event_name == "UserPromptSubmit")
        .map(|e| e.ts_ms)
        .collect();
    if prompts.len() < 2 {
        return Delivery::Never;
    }
    let first_stop = events
        .iter()
        .filter(|e| e.event.hook_event_name == "Stop" && e.ts_ms > prompts[0])
        .map(|e| e.ts_ms)
        .min()
        .unwrap_or(u64::MAX);
    if prompts[1] < first_stop {
        Delivery::Steered
    } else {
        Delivery::Queued
    }
}

/// Headless stream-json log ({ts, dir, line}): did an assistant message containing
/// `marker` appear before the first `result` event?
pub fn classify_stream(log: &[Value], marker: &str) -> Delivery {
    let out = |v: &&Value| v["dir"] == "out";
    let first_result = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["type"] == "result")
        .filter_map(|v| v["ts"].as_u64())
        .min()
        .unwrap_or(u64::MAX);
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["type"] == "assistant" && v["line"]["message"]["content"].to_string().contains(marker))
        .filter_map(|v| v["ts"].as_u64())
        .min();
    decide(marker_at, first_result)
}

/// Codex app-server log ({ts, dir, line}): did an agentMessage containing `marker`
/// appear before the first `turn/completed` notification?
pub fn classify_codex(log: &[Value], marker: &str) -> Delivery {
    let out = |v: &&Value| v["dir"] == "out";
    let first_done = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["method"] == "turn/completed")
        .filter_map(|v| v["ts"].as_u64())
        .min()
        .unwrap_or(u64::MAX);
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| {
            let s = v["line"].to_string();
            s.contains(marker) && s.contains("agentMessage")
        })
        .filter_map(|v| v["ts"].as_u64())
        .min();
    decide(marker_at, first_done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::HookEvent;
    use serde_json::json;

    fn logged(ts: u64, name: &str) -> LoggedEvent {
        LoggedEvent {
            ts_ms: ts,
            task_id: None,
            event: HookEvent {
                hook_event_name: name.into(),
                session_id: None,
                transcript_path: None,
                source: None,
                message: None,
                tool_name: None,
            },
        }
    }

    #[test]
    fn hooks_second_prompt_before_first_stop_is_steered() {
        let e = vec![logged(1, "UserPromptSubmit"), logged(5, "UserPromptSubmit"), logged(9, "Stop")];
        assert_eq!(classify_hooks(&e), Delivery::Steered);
    }

    #[test]
    fn hooks_second_prompt_after_first_stop_is_queued() {
        let e = vec![logged(1, "UserPromptSubmit"), logged(5, "Stop"), logged(6, "UserPromptSubmit"), logged(9, "Stop")];
        assert_eq!(classify_hooks(&e), Delivery::Queued);
    }

    #[test]
    fn hooks_single_prompt_is_never() {
        assert_eq!(classify_hooks(&[logged(1, "UserPromptSubmit")]), Delivery::Never);
    }

    fn out(ts: u64, line: serde_json::Value) -> serde_json::Value {
        json!({"ts": ts, "dir": "out", "line": line})
    }

    #[test]
    fn stream_marker_before_first_result_is_steered() {
        let log = vec![
            json!({"ts": 1, "dir": "in", "line": {"type": "user", "message": {"content": "BANANA"}}}),
            out(5, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BANANA"}]}})),
            out(9, json!({"type": "result"})),
        ];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Steered);
    }

    #[test]
    fn stream_marker_after_first_result_is_queued() {
        let log = vec![
            out(5, json!({"type": "result"})),
            out(7, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BANANA"}]}})),
        ];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Queued);
    }

    #[test]
    fn stream_without_marker_is_never() {
        assert_eq!(classify_stream(&[out(5, json!({"type": "result"}))], "BANANA"), Delivery::Never);
    }

    #[test]
    fn codex_agent_message_before_turn_completed_is_steered() {
        let log = vec![
            out(3, json!({"method": "item/completed", "params": {"item": {"type": "userMessage", "text": "BANANA"}}})),
            out(4, json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "BANANA"}}})),
            out(8, json!({"method": "turn/completed"})),
        ];
        assert_eq!(classify_codex(&log, "BANANA"), Delivery::Steered);
    }
}
