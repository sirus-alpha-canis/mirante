use serde_json::Value;

/// Where a thread id / turn id may appear in app-server traffic ({ts, dir, line} log).
/// Verified against the generated schema in Task 10; extend if the schema differs.
pub const THREAD_POINTERS: [&str; 3] = ["/line/result/thread/id", "/line/result/threadId", "/line/params/thread/id"];
pub const TURN_POINTERS: [&str; 3] = ["/line/result/turn/id", "/line/params/turn/id", "/line/result/turnId"];

pub fn capture(log: &[Value], pointers: &[&str]) -> Option<String> {
    log.iter()
        .filter(|v| v["dir"] == "out")
        .find_map(|v| pointers.iter().find_map(|p| v.pointer(p).and_then(Value::as_str).map(str::to_string)))
}

/// Fills $CWD, $THREAD and $TURN placeholders in a script message.
pub fn fill(template: &str, cwd: &str, thread: Option<&str>, turn: Option<&str>) -> String {
    template
        .replace("$CWD", cwd)
        .replace("$THREAD", thread.unwrap_or(""))
        .replace("$TURN", turn.unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn captures_first_matching_pointer_from_output_only() {
        let log = vec![
            json!({"dir": "in", "line": {"result": {"thread": {"id": "wrong"}}}}),
            json!({"dir": "out", "line": {"id": 2, "result": {"thread": {"id": "th_1"}}}}),
            json!({"dir": "out", "line": {"method": "turn/started", "params": {"turn": {"id": "tu_9"}}}}),
        ];
        assert_eq!(capture(&log, &THREAD_POINTERS).as_deref(), Some("th_1"));
        assert_eq!(capture(&log, &TURN_POINTERS).as_deref(), Some("tu_9"));
    }

    #[test]
    fn fill_replaces_placeholders() {
        let t = r#"{"threadId":"$THREAD","expectedTurnId":"$TURN","cwd":"$CWD"}"#;
        assert_eq!(
            fill(t, "/tmp/x", Some("th_1"), Some("tu_9")),
            r#"{"threadId":"th_1","expectedTurnId":"tu_9","cwd":"/tmp/x"}"#
        );
    }
}
