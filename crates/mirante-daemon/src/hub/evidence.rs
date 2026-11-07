//! Evidence for typing a Claude permission answer. Screen text can never prove which request
//! a dialog belongs to (a two-line command looks exactly like a command plus its
//! description), so the hub types an answer only when the session's transcript shows that
//! the card's tool call is the one waiting: [`ToolLedger`] follows the `tool_use` /
//! `tool_result` blocks of the transcript the driver tails, and [`ToolLedger::check`] says
//! whether a card's `tool_use_id` is its agent's newest tool call with no result yet, with
//! no newer tool call of any agent still open.
use std::collections::VecDeque;

use mirante_core::status::is_sidechain;
use serde_json::Value;

/// Tool calls remembered per session (the newest ones; the check only looks at the end).
const LEDGER: usize = 512;

/// Whose conversation a transcript line belongs to: the main agent, or a side agent
/// (sidechain line, by its `agentId` when it has one).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Chain {
    Main,
    Side(Option<String>),
}

#[derive(Debug, Clone)]
struct ToolUse {
    chain: Chain,
    id: String,
    /// A `tool_result` for it was seen: the tool ran or was refused; its dialog is gone.
    done: bool,
}

/// The session's recent tool calls in transcript order.
#[derive(Debug, Default)]
pub(crate) struct ToolLedger {
    uses: VecDeque<ToolUse>,
}

fn chain_of(line: &Value) -> Chain {
    if is_sidechain(line) {
        Chain::Side(line["agentId"].as_str().map(str::to_string))
    } else {
        Chain::Main
    }
}

fn blocks<'a>(line: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    line["message"]["content"].as_array().into_iter().flatten().filter(move |b| b["type"] == kind)
}

impl ToolLedger {
    /// Records the `tool_use` blocks of an assistant line and the `tool_result` blocks of a
    /// user line. True when the line had any (the hub then bumps the card generation).
    pub(crate) fn observe(&mut self, line: &Value) -> bool {
        let mut any = false;
        match line["type"].as_str() {
            Some("assistant") => {
                let chain = chain_of(line);
                for b in blocks(line, "tool_use") {
                    if let Some(id) = b["id"].as_str() {
                        self.uses.push_back(ToolUse { chain: chain.clone(), id: id.to_string(), done: false });
                        any = true;
                    }
                }
                let excess = self.uses.len().saturating_sub(LEDGER);
                self.uses.drain(..excess);
            }
            Some("user") => {
                for b in blocks(line, "tool_result") {
                    if let Some(id) = b["tool_use_id"].as_str() {
                        if let Some(u) = self.uses.iter_mut().rev().find(|u| u.id == id) {
                            u.done = true;
                        }
                        any = true;
                    }
                }
            }
            _ => {}
        }
        any
    }

    /// `Ok` when the transcript shows tool call `tool_use_id` as the newest one of its agent
    /// (`agent`: `None` = the main agent; a side agent's card needs that agent's sidechain
    /// lines in this transcript) with no result yet, and every newer tool call of any other
    /// agent already has its result. Else why not.
    pub(crate) fn check(&self, tool_use_id: Option<&str>, agent: Option<&str>) -> Result<(), String> {
        let id = tool_use_id.ok_or("the card has no tool_use_id (neither the hook nor its PreToolUse had one)")?;
        let chain = match agent {
            None => Chain::Main,
            Some(a) => Chain::Side(Some(a.to_string())),
        };
        let pos = self
            .uses
            .iter()
            .rposition(|u| u.id == id)
            .ok_or_else(|| format!("the transcript does not show tool call {id} (yet)"))?;
        let this = &self.uses[pos];
        if this.chain != chain {
            return Err(format!("tool call {id} is not this agent's in the transcript"));
        }
        if this.done {
            return Err(format!("tool call {id} already has a result"));
        }
        for later in self.uses.iter().skip(pos + 1) {
            if later.chain == chain {
                return Err(format!("the agent made a newer tool call ({}) after this card's", later.id));
            }
            if !later.done {
                return Err(format!("another agent's newer tool call ({}) is still open", later.id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_use(id: &str) -> Value {
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash"}]}})
    }

    fn side_use(id: &str, agent: &str) -> Value {
        let mut v = tool_use(id);
        v["isSidechain"] = json!(true);
        v["agentId"] = json!(agent);
        v
    }

    fn result(id: &str) -> Value {
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id}]}})
    }

    #[test]
    fn the_newest_open_tool_call_of_the_agent_is_the_evidence() {
        let mut l = ToolLedger::default();
        assert!(l.check(Some("t1"), None).unwrap_err().contains("does not show"));
        assert!(l.observe(&tool_use("t1")));
        assert_eq!(l.check(Some("t1"), None), Ok(()));
        assert!(l.check(None, None).unwrap_err().contains("no tool_use_id"));
        // A newer tool call of the same agent: the dialog on screen may be that one's.
        l.observe(&tool_use("t2"));
        assert!(l.check(Some("t1"), None).unwrap_err().contains("newer tool call (t2)"));
        assert_eq!(l.check(Some("t2"), None), Ok(()));
        // Its result arrived: its dialog is gone.
        assert!(l.observe(&result("t2")));
        assert!(l.check(Some("t2"), None).unwrap_err().contains("already has a result"));
        assert!(!l.observe(&json!({"type": "system", "subtype": "turn_duration"})));
    }

    #[test]
    fn several_tool_calls_in_one_assistant_line_are_in_order() {
        let mut l = ToolLedger::default();
        l.observe(&json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "a"}, {"type": "text", "text": "x"}, {"type": "tool_use", "id": "b"}]}}));
        assert!(l.check(Some("a"), None).is_err());
        assert_eq!(l.check(Some("b"), None), Ok(()));
    }

    #[test]
    fn side_agents_are_kept_apart_from_the_main_agent() {
        let mut l = ToolLedger::default();
        l.observe(&tool_use("m1"));
        l.observe(&side_use("s1", "a-1"));
        // The main card is not the newest open call any more: a side agent's is.
        assert!(l.check(Some("m1"), None).unwrap_err().contains("another agent"));
        assert_eq!(l.check(Some("s1"), Some("a-1")), Ok(()));
        assert!(l.check(Some("s1"), None).unwrap_err().contains("not this agent's"));
        assert!(l.check(Some("s1"), Some("a-2")).is_err());
        // Once the side call has its result, the main one is the open one again.
        l.observe(&result("s1"));
        assert_eq!(l.check(Some("m1"), None), Ok(()));
    }

    #[test]
    fn the_ledger_is_bounded() {
        let mut l = ToolLedger::default();
        for i in 0..LEDGER + 10 {
            l.observe(&tool_use(&format!("t{i}")));
        }
        assert_eq!(l.uses.len(), LEDGER);
        assert!(l.check(Some("t0"), None).is_err());
    }
}
