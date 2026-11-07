use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Sums token usage of assistant messages in a Claude Code transcript (JSONL).
/// One API message can span several lines with the same message.id: counted once.
pub fn sum_transcript(lines: &[Value], session_id: Option<&str>) -> Usage {
    let mut seen = HashSet::new();
    let mut u = Usage::default();
    for v in lines {
        if v["type"] != "assistant" {
            continue;
        }
        if let Some(sid) = session_id {
            if v["sessionId"].as_str() != Some(sid) {
                continue;
            }
        }
        if let Some(id) = v["message"]["id"].as_str() {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        let m = &v["message"]["usage"];
        let f = |k: &str| m[k].as_u64().unwrap_or(0);
        u.input += f("input_tokens");
        u.output += f("output_tokens");
        u.cache_creation += f("cache_creation_input_tokens");
        u.cache_read += f("cache_read_input_tokens");
    }
    u
}

/// Text of the last assistant message that has any text block.
pub fn last_assistant_text(lines: &[Value]) -> Option<String> {
    lines.iter().rev().filter(|v| v["type"] == "assistant").find_map(|v| {
        let text: String = v["message"]["content"]
            .as_array()?
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(sid: &str, id: &str, input: u64, output: u64, text: &str) -> Value {
        json!({
            "type": "assistant",
            "sessionId": sid,
            "message": {
                "id": id,
                "content": [{"type": "text", "text": text}],
                "usage": {"input_tokens": input, "output_tokens": output,
                          "cache_creation_input_tokens": 1, "cache_read_input_tokens": 2}
            }
        })
    }

    #[test]
    fn sums_once_per_message_id_and_filters_session() {
        let lines = vec![
            assistant("s1", "m1", 10, 5, "a"),
            assistant("s1", "m1", 10, 5, "a-dup"),
            assistant("s1", "m2", 20, 7, "b"),
            assistant("s2", "m3", 999, 999, "other"),
            json!({"type": "user", "message": {"content": "hi"}}),
        ];
        let u = sum_transcript(&lines, Some("s1"));
        assert_eq!(u, Usage { input: 30, output: 12, cache_creation: 2, cache_read: 4 });
        assert_eq!(u.total(), 48);
    }

    #[test]
    fn last_assistant_text_skips_tool_only_messages() {
        let lines = vec![
            assistant("s", "m1", 0, 0, "JABUTICABA"),
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}}),
        ];
        assert_eq!(last_assistant_text(&lines).as_deref(), Some("JABUTICABA"));
    }
}
