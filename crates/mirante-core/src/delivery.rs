//! Steer vs queue, from the Claude Code transcript (ported from `spike_kit::delivery::
//! classify_transcript`, the version validated in Week 1).
//!
//! Rule, from real 2.1.285 transcripts, by FILE ORDER (timestamps are not monotonic):
//! - A message typed while a turn runs is logged as `queue-operation` `enqueue` with its
//!   `content`.
//! - Steered: a `remove` with `reason: "absorbed_mid_turn"` before that turn ends.
//! - Queued: a `dequeue` (or a non-cancel `remove`), then a `user` line with
//!   `promptSource: "queued"` before the next in-turn enqueue or the next non-queued prompt.
//! - Enqueues pair with remove/dequeue operations first-in, first-out, except that a
//!   `remove` naming its `content` pairs with the oldest untaken enqueue of that text (the
//!   user may delete a message that is not at the head).
//! - A turn starts at a prompt line as the status engine sees it ([`crate::status::is_prompt`]:
//!   typed `origin.kind == "human"`, headless `promptSource == "sdk"`, or a delivered
//!   `promptSource == "queued"`), and ends at the final assistant line
//!   (`stop_reason == "end_turn"`) or a `system` `stop_hook_summary` / `turn_duration` line.
//! - Sidechain lines (`isSidechain`, `agentId`, `parentToolUseID`) are ignored.
//!
//! Change from the spike: when the outcome is not decided yet (no remove/dequeue yet, or a
//! dequeue whose queued prompt line has not been written yet) the result is `Pending`, not
//! `Never`. The UI calls this on a live, growing transcript.
//!
//! Known limitation, global FIFO orphans: the queue is modelled as one FIFO over the whole
//! transcript. A queue operation this module does not know (anything but `enqueue`,
//! `remove`, `dequeue`) takes nothing, so if Claude Code ever adds one that consumes a
//! message, that enqueue stays at the head as an orphan and every later content-less
//! `dequeue` pairs with the wrong message (shifted by one). Pinned by
//! `unknown_queue_op_leaves_an_orphan_that_shifts_fifo_pairing`.
use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use serde_json::Value;

use crate::status::{is_prompt, is_sidechain};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Absorbed by the running turn at a tool boundary (true mid-run steering).
    Steered,
    /// Delivered as a new prompt after the running turn ended.
    Queued,
    /// Enqueued, outcome not written to the transcript yet.
    Pending,
    /// Cancelled, or overtaken by another prompt: never delivered.
    Never,
    /// No matching mid-turn message in the transcript.
    Inconclusive,
}

/// What Mirante puts in front of a person's mid-run message (spec §5, untrusted text): the
/// agent reads it as coming from the human operator, not from the repo or another agent.
/// One line, followed by one space and the message, so a one-line message stays one line
/// (a paste the agent may collapse or a typist that counts lines is unaffected).
pub const OPERATOR_LABEL: &str = "[Mirante · message from the human operator]";

/// `text` as handed to the agent when a person sent it: [`OPERATOR_LABEL`], a space, the
/// text. A slash command (`/…` once leading whitespace is skipped) is never labelled: the
/// command must be the first thing the agent reads, or it is not run.
pub fn label_operator(text: &str) -> String {
    if text.trim_start().starts_with('/') {
        return text.to_string();
    }
    format!("{OPERATOR_LABEL} {text}")
}

/// `text` without a leading [`OPERATOR_LABEL`] (and its space), else `text` unchanged.
pub fn strip_operator_label(text: &str) -> &str {
    match text.strip_prefix(OPERATOR_LABEL) {
        Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
        None => text,
    }
}

/// Whether `text` carries [`OPERATOR_LABEL`] anywhere (an agent's message that does would
/// pass itself off as the person's).
pub fn has_operator_label(text: &str) -> bool {
    text.contains(OPERATOR_LABEL)
}

fn is_turn_end(v: &Value) -> bool {
    (v["type"] == "assistant" && v["message"]["stop_reason"] == "end_turn")
        || (v["type"] == "system" && (v["subtype"] == "stop_hook_summary" || v["subtype"] == "turn_duration"))
}

fn is_op(v: &Value, ops: &[&str]) -> bool {
    v["type"] == "queue-operation" && v["operation"].as_str().is_some_and(|o| ops.contains(&o))
}

/// Main-chain lines plus, for each enqueue, whether it happened inside a running turn and
/// which remove/dequeue took it (FIFO).
struct Analysis<'a> {
    lines: Vec<&'a Value>,
    enqueues: Vec<(usize, bool)>,
    taken_by: HashMap<usize, usize>,
}

fn analyze(all: &[Value]) -> Analysis<'_> {
    let lines: Vec<&Value> = all.iter().filter(|v| !is_sidechain(v)).collect();
    let mut enqueues = Vec::new();
    let mut taken_by = HashMap::new();
    let mut fifo = VecDeque::new();
    let mut in_turn = false;
    for (i, v) in lines.iter().enumerate() {
        if is_prompt(v) {
            in_turn = true;
        } else if is_turn_end(v) {
            in_turn = false;
        } else if is_op(v, &["enqueue"]) {
            enqueues.push((i, in_turn));
            fifo.push_back(i);
        } else if is_op(v, &["remove", "dequeue"]) {
            let by_content = v["content"].as_str().map(str::trim).and_then(|c| {
                fifo.iter().position(|&e: &usize| lines[e]["content"].as_str().is_some_and(|ec| ec.trim() == c))
            });
            if let Some(e) = fifo.remove(by_content.unwrap_or(0)) {
                taken_by.insert(e, i);
            }
        }
    }
    Analysis { lines, enqueues, taken_by }
}

fn classify_enqueue(a: &Analysis<'_>, enqueue: usize) -> Delivery {
    let lines = &a.lines;
    let Some(&taken) = a.taken_by.get(&enqueue) else {
        return Delivery::Pending;
    };
    let end = lines[enqueue + 1..].iter().position(|v| is_turn_end(v) || is_prompt(v)).map(|p| p + enqueue + 1);
    let before_end = end.is_none_or(|e| taken < e);
    let op = lines[taken];
    if before_end && op["reason"] == "absorbed_mid_turn" {
        return Delivery::Steered;
    }
    if op["operation"] == "remove" && op["reason"].as_str().is_some_and(|r| r.contains("cancel")) {
        return Delivery::Never;
    }
    // A queued user line must follow before the next in-turn enqueue or the next non-queued prompt.
    let mut in_turn = before_end;
    for v in &lines[taken + 1..] {
        if v["type"] == "user" && v["promptSource"] == "queued" {
            return Delivery::Queued;
        }
        if is_prompt(v) {
            return Delivery::Never;
        }
        if is_turn_end(v) {
            in_turn = false;
        } else if in_turn && is_op(v, &["enqueue"]) {
            return Delivery::Never;
        }
    }
    Delivery::Pending
}

/// Outcome of the first message enqueued while a turn was running.
pub fn classify_transcript(lines: &[Value]) -> Delivery {
    let a = analyze(lines);
    match a.enqueues.iter().find(|(_, in_turn)| *in_turn) {
        Some(&(i, _)) => classify_enqueue(&a, i),
        None => Delivery::Inconclusive,
    }
}

/// "Was my message steered or queued?" — outcome of the most recent message whose enqueued
/// text equals `text` (trimmed, [`OPERATOR_LABEL`] ignored). `Inconclusive` when no such message was enqueued mid-turn.
pub fn classify_message(lines: &[Value], text: &str) -> Delivery {
    let a = analyze(lines);
    // Matched without the operator label on either side: the transcript holds what was
    // typed (labelled when a person sent it), the caller may hold either.
    let want = strip_operator_label(text.trim()).trim();
    let found =
        a.enqueues.iter().rev().find(|(i, _)| {
            a.lines[*i]["content"].as_str().is_some_and(|c| strip_operator_label(c.trim()).trim() == want)
        });
    match found {
        Some(&(i, true)) => classify_enqueue(&a, i),
        _ => Delivery::Inconclusive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Minimal, anonymized shapes of real Claude Code 2.1.285 transcript lines (Week 1, Task 8).
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
    fn enqueue(text: &str) -> Value {
        json!({"type": "queue-operation", "operation": "enqueue", "content": text})
    }
    fn dequeue() -> Value {
        json!({"type": "queue-operation", "operation": "dequeue"})
    }
    fn removed(reason: &str) -> Value {
        json!({"type": "queue-operation", "operation": "remove", "content": "BANANA please",
               "reason": reason, "commandUuid": "c1"})
    }
    fn answer(text: &str) -> Value {
        json!({"type": "assistant", "message": {"stop_reason": "end_turn",
               "content": [{"type": "text", "text": text}]}})
    }
    fn queued_user() -> Value {
        json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {}})
    }
    fn turn_end() -> Vec<Value> {
        vec![
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration", "durationMs": 10763}),
        ]
    }

    #[test]
    fn remove_before_turn_end_is_steered() {
        let mut t = vec![prompt("run five steps"), tool_use(), tool_result(), queue("enqueue"), tool_use()];
        t.extend([removed("absorbed_mid_turn"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.push(answer("BANANA"));
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn headless_sdk_prompt_starts_a_turn() {
        let mut t = vec![queue("enqueue"), dequeue()];
        t.push(json!({"type": "user", "promptSource": "sdk",
                      "message": {"role": "user", "content": [{"type": "text", "text": "run five steps"}]}}));
        t.extend([tool_use(), queue("enqueue"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.extend([removed("absorbed_mid_turn"), answer("BANANA")]);
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn dequeue_after_end_turn_is_queued() {
        let t = vec![
            prompt("list 60 fruits"),
            queue("enqueue"),
            answer("1. apple ..."),
            dequeue(),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued",
                   "message": {"role": "user", "content": "BANANA please"}}),
            answer("BANANA"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn enqueue_while_idle_is_not_mid_run() {
        let t = vec![
            queue("enqueue"),
            dequeue(),
            prompt("say hi"),
            answer("hi"),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn remove_after_next_human_prompt_is_queued() {
        let t =
            vec![prompt("one"), queue("enqueue"), answer("done"), prompt("two"), removed("dequeued"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn tool_result_user_line_does_not_end_the_turn() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), tool_result(), removed("absorbed_mid_turn")];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn enqueued_but_not_yet_taken_is_pending() {
        let mut t = vec![prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn dequeued_without_queued_line_yet_is_pending() {
        let t = vec![prompt("one"), queue("enqueue"), answer("done"), dequeue()];
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn without_enqueue_is_inconclusive() {
        let mut t = vec![prompt("one"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn remove_before_enqueue_is_ignored() {
        let mut t = vec![removed("dequeued"), prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn sidechain_end_turn_does_not_end_parent_turn() {
        let t = vec![
            prompt("one"),
            tool_use(),
            queue("enqueue"),
            json!({"type": "assistant", "isSidechain": true, "agentId": "a1",
                   "message": {"stop_reason": "end_turn", "content": []}}),
            json!({"type": "system", "subtype": "turn_duration", "parentToolUseID": "toolu_1"}),
            removed("absorbed_mid_turn"),
            answer("BANANA"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn sidechain_prompt_and_enqueue_are_ignored() {
        let t = vec![
            json!({"type": "user", "isSidechain": true, "origin": {"kind": "human"}, "message": {}}),
            json!({"type": "queue-operation", "operation": "enqueue", "agentId": "a1"}),
            answer("done"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn cancelled_remove_mid_turn_is_never() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), removed("cancelled"), answer("done")];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn non_absorbed_remove_then_queued_user_is_queued() {
        let t = vec![prompt("one"), queue("enqueue"), dequeue(), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn cancelled_enqueue_does_not_borrow_a_later_queued_line() {
        let mut t = vec![prompt("one"), tool_use(), queue("enqueue"), removed("cancelled"), answer("done")];
        t.extend(turn_end());
        t.extend([prompt("two"), queue("enqueue"), dequeue(), queued_user()]);
        assert_eq!(classify_transcript(&t), Delivery::Never);
        let t = vec![prompt("one"), queue("enqueue"), removed("user_cancelled"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn queued_line_after_next_in_turn_enqueue_or_prompt_is_never() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), dequeue(), queue("enqueue"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
        let t = vec![prompt("one"), queue("enqueue"), dequeue(), prompt("two"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn multiple_enqueues_pair_fifo() {
        let t = vec![
            prompt("one"),
            tool_use(),
            queue("enqueue"),
            queue("enqueue"),
            removed("absorbed_mid_turn"),
            answer("x"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
        let t = vec![prompt("one"), queue("enqueue"), queue("enqueue"), answer("x"), dequeue(), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn the_operator_label_prefixes_a_message_but_never_a_slash_command() {
        let l = label_operator("use tabs");
        assert_eq!(l, format!("{OPERATOR_LABEL} use tabs"));
        assert_eq!(strip_operator_label(&l), "use tabs");
        assert_eq!(strip_operator_label("use tabs"), "use tabs");
        // A slash command must stay the first thing typed, or the agent never runs it.
        assert_eq!(label_operator("  /compact keep it"), "  /compact keep it");
        assert!(label_operator("fix a/b").starts_with(OPERATOR_LABEL));
        assert!(has_operator_label(&format!("x\n{OPERATOR_LABEL} y")));
        assert!(!has_operator_label("Mirante says hi"));
    }

    #[test]
    fn classify_message_matches_a_labelled_message_by_its_text() {
        let t = vec![
            prompt("run five steps"),
            tool_use(),
            enqueue(&label_operator("A")),
            json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn"}),
            tool_result(),
            answer("done"),
        ];
        assert_eq!(classify_message(&t, "A"), Delivery::Steered);
        assert_eq!(classify_message(&t, &label_operator("A")), Delivery::Steered);
    }

    #[test]
    fn classify_message_finds_each_message_by_text() {
        // "A" is absorbed mid-turn; "B" arrives too late for a tool boundary and is queued.
        let t = vec![
            prompt("run five steps"),
            tool_use(),
            enqueue("A"),
            json!({"type": "queue-operation", "operation": "remove", "content": "A", "reason": "absorbed_mid_turn"}),
            tool_result(),
            enqueue("B"),
            answer("done"),
            dequeue(),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {"content": "B"}}),
        ];
        assert_eq!(classify_message(&t, "A"), Delivery::Steered);
        assert_eq!(classify_message(&t, " B "), Delivery::Queued);
        assert_eq!(classify_message(&t, "C"), Delivery::Inconclusive);
    }

    #[test]
    fn classify_message_is_pending_while_waiting_and_inconclusive_when_idle() {
        let live = vec![prompt("one"), tool_use(), enqueue("A")];
        assert_eq!(classify_message(&live, "A"), Delivery::Pending);
        let idle = vec![enqueue("A"), dequeue(), prompt("A")];
        assert_eq!(classify_message(&idle, "A"), Delivery::Inconclusive);
    }

    /// A queued prompt opens a running turn (shared `status::is_prompt`): a message typed
    /// during that turn is mid-run and can be steered. 2.1.285 queued lines also carry
    /// `origin.kind: "human"`; this one does not, so the test pins the `promptSource` rule.
    #[test]
    fn enqueue_during_a_turn_opened_by_a_queued_prompt_is_mid_run() {
        let t = vec![
            prompt("one"),
            enqueue("A"),
            answer("done"),
            dequeue(),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "promptSource": "queued", "message": {"content": "A"}}),
            tool_use(),
            enqueue("B"),
            json!({"type": "queue-operation", "operation": "remove", "content": "B", "reason": "absorbed_mid_turn"}),
            answer("B done"),
        ];
        assert_eq!(classify_message(&t, "A"), Delivery::Queued);
        assert_eq!(classify_message(&t, "B"), Delivery::Steered);
    }

    /// A `remove` that names its content pairs with that enqueue even when it is not the
    /// FIFO head (the user deleted the second of two queued messages).
    #[test]
    fn non_head_remove_pairs_by_content() {
        let t = vec![
            prompt("one"),
            tool_use(),
            enqueue("A"),
            enqueue("B"),
            json!({"type": "queue-operation", "operation": "remove", "content": "B", "reason": "cancelled"}),
            json!({"type": "queue-operation", "operation": "remove", "content": "A", "reason": "absorbed_mid_turn"}),
            answer("A done"),
        ];
        assert_eq!(classify_message(&t, "A"), Delivery::Steered);
        assert_eq!(classify_message(&t, "B"), Delivery::Never);
    }

    /// Known limitation (global FIFO): an operation this module does not know (here a made-up
    /// `clear`) takes nothing, so its enqueue stays at the head as an orphan and the next
    /// content-less `dequeue` pairs with it instead of with the message it really took.
    /// Pinned so a new Claude Code queue operation shows up as a test to revisit.
    #[test]
    fn unknown_queue_op_leaves_an_orphan_that_shifts_fifo_pairing() {
        let t = vec![
            prompt("one"),
            tool_use(),
            enqueue("A"),
            json!({"type": "queue-operation", "operation": "clear"}),
            enqueue("B"),
            answer("done"),
            dequeue(),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {"content": "B"}}),
        ];
        // Reality: A was cleared (Never), B was queued. FIFO credits the dequeue to A.
        assert_eq!(classify_message(&t, "A"), Delivery::Queued);
        assert_eq!(classify_message(&t, "B"), Delivery::Pending);
    }
}
