use serde_json::Value;

use crate::hook::LoggedEvent;

/// How a second message, sent while the first turn was still running, was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Acted on before the first turn ended (true mid-run steering).
    Steered,
    /// Acted on only after the first turn ended.
    Queued,
    /// Never acted on (the first turn did end).
    Never,
    /// The first turn never ended in the log, so steered vs queued cannot be told apart.
    Inconclusive,
}

fn decide(marker_at: Option<u64>, first_end: Option<u64>) -> Delivery {
    let Some(end) = first_end else {
        return Delivery::Inconclusive;
    };
    match marker_at {
        None => Delivery::Never,
        Some(t) if t < end => Delivery::Steered,
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
    // Fewer than 2 prompts: injection without a UserPromptSubmit is possible, so not provably Never.
    if prompts.len() < 2 {
        return Delivery::Inconclusive;
    }
    // Events are sorted by ts (hook::load_events), so strict `>` is fine here.
    let Some(first_stop) = events
        .iter()
        .filter(|e| e.event.hook_event_name == "Stop" && e.ts_ms > prompts[0])
        .map(|e| e.ts_ms)
        .min()
    else {
        return Delivery::Inconclusive;
    };
    if prompts[1] < first_stop {
        Delivery::Steered
    } else {
        Delivery::Queued
    }
}

/// Interactive Claude Code transcript (`~/.claude/projects/<cwd>/<session>.jsonl`).
///
/// Rule, derived from real 2.1.285 transcripts (Task 8 / 8b):
/// - A message typed while a turn runs is logged as
///   `{"type":"queue-operation","operation":"enqueue","content":<text>}`.
/// - Taken mid-turn (steered): `{"type":"queue-operation","operation":"remove",
///   "reason":"absorbed_mid_turn","commandUuid":...}`, then an attachment
///   `{"type":"queued_command","prompt":<text>}`, then the assistant reacts in the same turn.
/// - Taken after the turn (queued): `{"type":"queue-operation","operation":"dequeue"}` (no
///   content), then a new `{"type":"user","origin":{"kind":"human"},"promptSource":"queued"}`.
/// - A turn is over at its final assistant line (`message.stop_reason == "end_turn"`), then
///   `{"type":"system","subtype":"stop_hook_summary"}` / `"turn_duration"`. A typed prompt
///   (`type:"user"` with `origin.kind:"human"`; tool results have no `origin`) starts a turn.
/// - Headless `-p --input-format stream-json` (Task 9): the prompt line has no `origin`, only
///   `promptSource:"sdk"`; it also starts a turn. A mid-turn stream-json message is logged
///   like the interactive case (enqueue, queued_command, remove absorbed_mid_turn).
///
/// Positions are FILE ORDER, not `timestamp`: lines are appended as things happen, but
/// timestamps are not monotonic (an assistant line had a timestamp 4s older than the
/// enqueue before it; a `dequeue` carried a later timestamp than the system lines after it).
///
/// Classification: take the first `enqueue` that happens inside a running turn (after a
/// human prompt and before that turn's end; headless `-p` also logs enqueue/dequeue for its
/// initial prompt while idle). None → `Inconclusive`. Then the first `remove`/`dequeue`
/// after it: none → `Never`; before the running turn's end (or no end at all) → `Steered`;
/// otherwise → `Queued`.
pub fn classify_transcript(lines: &[Value]) -> Delivery {
    let is_prompt = |v: &Value| v["type"] == "user" && (v["origin"]["kind"] == "human" || v["promptSource"] == "sdk");
    let is_turn_end = |v: &Value| {
        (v["type"] == "assistant" && v["message"]["stop_reason"] == "end_turn")
            || (v["type"] == "system" && (v["subtype"] == "stop_hook_summary" || v["subtype"] == "turn_duration"))
    };
    let is_op = |v: &Value, ops: &[&str]| {
        v["type"] == "queue-operation" && v["operation"].as_str().is_some_and(|o| ops.contains(&o))
    };

    let mut in_turn = false;
    let mut enqueue = None;
    for (i, v) in lines.iter().enumerate() {
        if is_prompt(v) {
            in_turn = true;
        } else if is_turn_end(v) {
            in_turn = false;
        } else if in_turn && is_op(v, &["enqueue"]) {
            enqueue = Some(i);
            break;
        }
    }
    let Some(enqueue) = enqueue else {
        return Delivery::Inconclusive;
    };
    let after = &lines[enqueue + 1..];
    let Some(taken) = after.iter().position(|v| is_op(v, &["remove", "dequeue"])) else {
        return Delivery::Never;
    };
    match after.iter().position(|v| is_turn_end(v) || is_prompt(v)) {
        Some(end) if end < taken => Delivery::Queued,
        _ => Delivery::Steered,
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
        .min();
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| {
            v["line"]["type"] == "assistant"
                && v["line"]["message"]["content"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|b| {
                        b["type"] == "text" && b["text"].as_str().is_some_and(|t| t.contains(marker))
                    })
                })
        })
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
        .min();
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| {
            let item = &v["line"]["params"]["item"];
            item["type"] == "agentMessage" && item["text"].as_str().is_some_and(|t| t.contains(marker))
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

    // Minimal, anonymized shapes of real Claude Code 2.1.285 transcript lines (Task 8).
    fn prompt(text: &str) -> Value {
        json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed",
               "message": {"role": "user", "content": text}})
    }
    fn tool_use() -> Value {
        json!({"type": "assistant", "message": {"stop_reason": "tool_use",
               "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash"}]}})
    }
    fn tool_result() -> Value {
        json!({"type": "user", "message": {"role": "user",
               "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "passo-1"}]}})
    }
    fn queue(op: &str) -> Value {
        json!({"type": "queue-operation", "operation": op, "content": "BANANA please"})
    }
    fn removed(reason: &str) -> Value {
        json!({"type": "queue-operation", "operation": "remove", "content": "BANANA please",
               "reason": reason, "commandUuid": "c1"})
    }
    fn answer(text: &str) -> Value {
        json!({"type": "assistant", "message": {"stop_reason": "end_turn",
               "content": [{"type": "text", "text": text}]}})
    }
    fn turn_end() -> Vec<Value> {
        vec![
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration", "durationMs": 10763}),
        ]
    }

    #[test]
    fn transcript_remove_before_turn_end_is_steered() {
        // Real order from Task 8: enqueue, tool_use, remove, tool_result, queued_command, answer, turn end.
        let mut t = vec![prompt("run five steps"), tool_use(), tool_result(), queue("enqueue"), tool_use()];
        t.extend([removed("absorbed_mid_turn"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.push(answer("BANANA"));
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn transcript_headless_sdk_prompt_starts_a_turn() {
        // Real order from Task 9 (claude -p, stream-json in): enqueue+dequeue of the initial
        // prompt while idle, the sdk prompt, then the 2nd message absorbed mid-turn.
        let mut t = vec![queue("enqueue"), json!({"type": "queue-operation", "operation": "dequeue"})];
        t.push(json!({"type": "user", "promptSource": "sdk",
                      "message": {"role": "user", "content": [{"type": "text", "text": "run five steps"}]}}));
        t.extend([tool_use(), queue("enqueue"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.extend([removed("absorbed_mid_turn"), answer("BANANA")]);
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn transcript_dequeue_after_end_turn_is_queued() {
        // Real order from a Task 8b probe (tool-less turn): the `dequeue` line is written
        // BEFORE the system turn-end lines, but after the end_turn assistant line.
        let t = vec![
            prompt("list 60 fruits"),
            queue("enqueue"),
            answer("1. apple ..."),
            json!({"type": "queue-operation", "operation": "dequeue"}),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued",
                   "message": {"role": "user", "content": "BANANA please"}}),
            answer("BANANA"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn transcript_enqueue_while_idle_is_not_mid_run() {
        // Headless `claude -p` logs enqueue+dequeue for the initial prompt before any turn.
        let t = vec![
            queue("enqueue"),
            json!({"type": "queue-operation", "operation": "dequeue"}),
            prompt("say hi"),
            answer("hi"),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn transcript_remove_after_next_human_prompt_is_queued() {
        // No system line, but a new typed prompt also ends the turn that was running.
        let t = vec![prompt("one"), queue("enqueue"), answer("done"), prompt("two"), removed("dequeued")];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn transcript_tool_result_user_line_does_not_end_the_turn() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), tool_result(), removed("absorbed_mid_turn")];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn transcript_enqueued_but_never_removed_is_never() {
        let mut t = vec![prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn transcript_without_enqueue_is_inconclusive() {
        let mut t = vec![prompt("one"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn transcript_remove_before_enqueue_is_ignored() {
        let mut t = vec![removed("dequeued"), prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Never);
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
    fn hooks_single_prompt_is_inconclusive() {
        assert_eq!(classify_hooks(&[logged(1, "UserPromptSubmit")]), Delivery::Inconclusive);
    }

    #[test]
    fn hooks_no_stop_after_first_prompt_is_inconclusive() {
        let e = vec![logged(1, "UserPromptSubmit"), logged(5, "UserPromptSubmit")];
        assert_eq!(classify_hooks(&e), Delivery::Inconclusive);
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
    fn stream_without_result_is_inconclusive() {
        let log = vec![out(5, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BANANA"}]}}))];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Inconclusive);
    }

    #[test]
    fn stream_marker_only_in_tool_use_input_is_not_counted() {
        let log = vec![
            out(5, json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": "echo BANANA"}}]}})),
            out(9, json!({"type": "result"})),
        ];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Never);
    }

    #[test]
    fn codex_without_turn_completed_is_inconclusive() {
        let log = vec![out(4, json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "BANANA"}}}))];
        assert_eq!(classify_codex(&log, "BANANA"), Delivery::Inconclusive);
    }

    #[test]
    fn codex_marker_only_in_user_message_inside_turn_completed_is_never() {
        let log = vec![out(
            8,
            json!({"method": "turn/completed", "params": {"turn": {"items": [
                {"type": "userMessage", "text": "BANANA"},
                {"type": "agentMessage", "text": "ok"}]}}}),
        )];
        assert_eq!(classify_codex(&log, "BANANA"), Delivery::Never);
    }

    #[test]
    fn codex_agent_message_after_turn_completed_is_queued() {
        let log = vec![
            out(8, json!({"method": "turn/completed"})),
            out(12, json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "got BANANA"}}})),
        ];
        assert_eq!(classify_codex(&log, "BANANA"), Delivery::Queued);
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
