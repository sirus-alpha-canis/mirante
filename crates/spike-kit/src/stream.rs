use serde_json::json;

/// One line of `claude -p --input-format stream-json` input.
pub fn user_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [ { "type": "text", "text": text } ] }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_line_is_single_line_json_with_text() {
        let l = user_line("oi\nmundo");
        assert!(!l.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "oi\nmundo");
    }
}
