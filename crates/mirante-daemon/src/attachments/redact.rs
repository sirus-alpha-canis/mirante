//! Redaction of what a person attaches, before it is stored.
//!
//! A HAR (`.har`, or any JSON with `log.entries`) carries cookies and auth headers in the
//! clear: every request and response header whose name says credential has its value
//! replaced with [`MARK`], so does every cookie value, every query-string or form
//! parameter whose name says credential, and the same parameters inside each request URL.
//! Then every remaining string goes through [`mirante_core::redact::redact_counted`]
//! (tokens in bodies and URLs). Other text goes through `redact_counted` as a whole.
//! Best effort: it lowers what leaks, it is no guarantee.
use mirante_core::redact::redact_counted;
use serde_json::Value;

/// What a removed value reads as.
pub const MARK: &str = "[redacted by Mirante]";

/// Header names always treated as credentials (compared without case).
const NAMES: [&str; 7] =
    ["cookie", "set-cookie", "authorization", "proxy-authorization", "x-api-key", "x-auth-token", "x-csrf-token"];

/// Any name containing one of these (without case) is treated as a credential.
const PARTS: [&str; 4] = ["token", "secret", "session", "auth"];

/// True when a header, cookie or parameter named `name` carries a credential.
pub fn sensitive(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    NAMES.contains(&n.as_str()) || PARTS.iter().any(|p| n.contains(p))
}

/// True when `v` is a HAR log (it has `log.entries`).
pub fn is_har(v: &Value) -> bool {
    v.get("log").and_then(|l| l.get("entries")).is_some_and(Value::is_array)
}

/// Text as stored and how many secrets were removed. `har_name`: the file's name says HAR
/// (`.har`); a JSON text that parses as a HAR is one whatever its name.
pub fn redact_text(text: &str, har_name: bool) -> (String, u32) {
    let looks_json = har_name || text.trim_start().starts_with('{');
    if looks_json {
        if let Ok(mut v) = serde_json::from_str::<Value>(text) {
            if is_har(&v) {
                let n = redact_har(&mut v);
                let out = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                return (out, n);
            }
        }
    }
    let (out, n) = redact_counted(text);
    (out, count(n))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Replaces `slot` (a string) with [`MARK`] unless empty or already masked.
fn mask(slot: &mut Value, n: &mut u32) {
    if let Some(s) = slot.as_str() {
        if !s.is_empty() && s != MARK {
            *slot = Value::String(MARK.to_string());
            *n = n.saturating_add(1);
        }
    }
}

/// `[{name, value}]` lists: values of credential names masked (`all`: every value).
fn mask_pairs(list: Option<&mut Value>, all: bool, n: &mut u32) {
    let Some(items) = list.and_then(Value::as_array_mut) else { return };
    for item in items {
        let Some(obj) = item.as_object_mut() else { continue };
        let hit = all || obj.get("name").and_then(Value::as_str).is_some_and(sensitive);
        if hit {
            if let Some(v) = obj.get_mut("value") {
                mask(v, n);
            }
        }
    }
}

/// `url` with the values of credential query parameters masked.
fn mask_url(url: &str, n: &mut u32) -> Option<String> {
    let (base, rest) = url.split_once('?')?;
    let (query, frag) = match rest.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (rest, None),
    };
    let mut changed = false;
    let parts: Vec<String> = query
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !v.is_empty() && v != MARK && sensitive(&decode_name(k)) => {
                changed = true;
                *n = n.saturating_add(1);
                format!("{k}={MARK}")
            }
            _ => p.to_string(),
        })
        .collect();
    if !changed {
        return None;
    }
    let mut out = format!("{base}?{}", parts.join("&"));
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    Some(out)
}

/// A query parameter's name with `%XX` and `+` decoded (enough to match it).
fn decode_name(k: &str) -> String {
    let b = k.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every string in `v` through `redact_counted`.
fn redact_strings(v: &mut Value, n: &mut u32) {
    match v {
        Value::String(s) => {
            let (out, c) = redact_counted(s);
            if c > 0 {
                *s = out;
                *n = n.saturating_add(count(c));
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| redact_strings(i, n)),
        Value::Object(map) => map.values_mut().for_each(|i| redact_strings(i, n)),
        _ => {}
    }
}

/// Masks a HAR log in place; returns how many values were removed.
pub fn redact_har(v: &mut Value) -> u32 {
    let mut n = 0u32;
    if let Some(entries) = v.get_mut("log").and_then(|l| l.get_mut("entries")).and_then(Value::as_array_mut) {
        for e in entries {
            for side in ["request", "response"] {
                let Some(part) = e.get_mut(side).and_then(Value::as_object_mut) else { continue };
                mask_pairs(part.get_mut("headers"), false, &mut n);
                mask_pairs(part.get_mut("cookies"), true, &mut n);
                mask_pairs(part.get_mut("queryString"), false, &mut n);
                if let Some(post) = part.get_mut("postData").and_then(Value::as_object_mut) {
                    mask_pairs(post.get_mut("params"), false, &mut n);
                }
                if let Some(url) = part.get_mut("url") {
                    if let Some(masked) = url.as_str().and_then(|u| mask_url(u, &mut n)) {
                        *url = Value::String(masked);
                    }
                }
            }
        }
    }
    redact_strings(v, &mut n);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A GitHub-shaped token built at run time (no token literal in the source).
    fn gh_token() -> String {
        format!("{}{}_{}", "gh", "p", "Z9y8X7w6V5u4T3s2R1q0P9o8N7m6L5k4J3i2")
    }

    fn har(token: &str) -> Value {
        json!({ "log": { "version": "1.2", "entries": [{
            "request": {
                "method": "GET",
                "url": format!("https://api.example.test/v1/items?page=2&access_token={token}&q=ok"),
                "headers": [
                    { "name": "Authorization", "value": format!("Bearer {token}") },
                    { "name": "cookie", "value": "sid=abc123" },
                    { "name": "X-CSRF-Token", "value": "csrf-value" },
                    { "name": "X-Session-Id", "value": "s-1" },
                    { "name": "Accept", "value": "application/json" },
                ],
                "cookies": [{ "name": "sid", "value": "abc123" }],
                "queryString": [
                    { "name": "page", "value": "2" },
                    { "name": "access_token", "value": token },
                ],
                "postData": { "mimeType": "application/x-www-form-urlencoded",
                    "params": [{ "name": "client_secret", "value": "cs" }, { "name": "user", "value": "ana" }],
                    "text": format!("note={token}") },
            },
            "response": {
                "status": 200,
                "headers": [{ "name": "Set-Cookie", "value": "sid=new; HttpOnly" }, { "name": "Content-Type", "value": "text/plain" }],
                "cookies": [{ "name": "sid", "value": "new" }],
                "content": { "text": format!("{{\"echo\":\"{token}\"}}") },
            },
        }]}})
    }

    #[test]
    fn credential_names_match_without_case() {
        for n in ["Cookie", "SET-COOKIE", "authorization", "Proxy-Authorization", "x-api-key", "X-Auth-Token"] {
            assert!(sensitive(n), "{n}");
        }
        for n in ["X-Session-Id", "refresh_token", "client_secret", "OAuth-State", "csrftoken"] {
            assert!(sensitive(n), "{n}");
        }
        for n in ["Accept", "Content-Type", "page", "user", "q"] {
            assert!(!sensitive(n), "{n}");
        }
    }

    #[test]
    fn a_har_loses_headers_cookies_params_and_tokens_in_bodies() {
        let token = gh_token();
        let text = har(&token).to_string();
        let (out, n) = redact_text(&text, true);
        assert!(!out.contains(&token), "{out}");
        assert!(!out.contains("abc123") && !out.contains("csrf-value") && !out.contains("sid=new"), "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        let req = &v["log"]["entries"][0]["request"];
        assert_eq!(req["headers"][0]["value"], MARK);
        assert_eq!(req["headers"][3]["value"], MARK);
        assert_eq!(req["headers"][4]["value"], "application/json");
        assert_eq!(req["cookies"][0]["value"], MARK);
        assert_eq!(req["queryString"][0]["value"], "2");
        assert_eq!(req["queryString"][1]["value"], MARK);
        assert_eq!(req["postData"]["params"][0]["value"], MARK);
        assert_eq!(req["postData"]["params"][1]["value"], "ana");
        let url = req["url"].as_str().unwrap();
        assert!(url.starts_with("https://api.example.test/v1/items?page=2&access_token="), "{url}");
        assert!(url.ends_with("&q=ok"), "{url}");
        let res = &v["log"]["entries"][0]["response"];
        assert_eq!(res["headers"][0]["value"], MARK);
        assert_eq!(res["headers"][1]["value"], "text/plain");
        assert_eq!(res["cookies"][0]["value"], MARK);
        // 4 request headers + cookie + query param + form param + url param, 1 response
        // header + cookie, then the tokens in the post text and the response body.
        assert!(n >= 12, "{n}");
    }

    #[test]
    fn json_that_is_a_har_is_redacted_whatever_its_name_and_other_text_through_redact() {
        let token = gh_token();
        let (out, n) = redact_text(&har(&token).to_string(), false);
        assert!(!out.contains(&token) && n > 0);
        let log = format!("GET /x?token={token}\nok\n");
        let (out, n) = redact_text(&log, false);
        assert!(!out.contains(&token), "{out}");
        assert!(n >= 1);
        let plain = "{\"a\": 1}";
        assert_eq!(redact_text(plain, false), (plain.to_string(), 0));
        // A .har that is not JSON is still text through redact.
        let (out, _) = redact_text(&format!("broken {token}"), true);
        assert!(!out.contains(&token));
    }

    #[test]
    fn url_names_are_matched_decoded_and_the_fragment_kept() {
        let mut n = 0;
        let out = mask_url("https://h/p?Auth%5Ftoken=v&x=1#frag", &mut n).unwrap();
        assert_eq!(out, format!("https://h/p?Auth%5Ftoken={MARK}&x=1#frag"));
        assert_eq!(n, 1);
        assert!(mask_url("https://h/p?x=1", &mut n).is_none());
        assert!(mask_url("https://h/p", &mut n).is_none());
        assert_eq!(decode_name("a%2"), "a%2");
    }
}
