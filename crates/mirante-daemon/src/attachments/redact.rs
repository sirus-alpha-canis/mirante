//! Redaction of what a person attaches, before it is stored.
//!
//! A HAR (`.har`, or any JSON with `log.entries`) carries cookies and auth headers in the
//! clear: every request and response header whose name says credential ([`sensitive`]) has
//! its value replaced with [`MARK`], so does every cookie value, every query-string or form
//! parameter whose name says credential, and the same parameters inside each request URL,
//! `redirectURL` and `Location` / `Content-Location` / `Referer` header (query and
//! fragment). Request and response bodies (`postData.text`, `content.text`; a base64 body
//! decoded first, a binary one replaced with [`BINARY`]) lose the values of credential
//! fields when form-encoded or JSON. Then every remaining string goes through
//! [`mirante_core::redact::redact_counted`] (tokens in bodies and URLs). A `.har` that is
//! not valid JSON is refused rather than stored half-masked.
//!
//! Other text: JSON-looking text (or a HAR that does not parse) gets the values of
//! credential keys, and of a `"value"` next to a credential `"name"`, masked by a lenient
//! scan ([`scan_mask`]) that keeps its layout; then everything goes through
//! `redact_counted`. Best effort: it lowers what leaks, it is no guarantee.
use mirante_core::redact::redact_counted;
use serde_json::Value;

/// What a removed value reads as.
pub const MARK: &str = "[redacted by Mirante]";

/// What a HAR body that is not text reads as once removed.
pub const BINARY: &str = "[binary body removed by Mirante]";

/// Header names always treated as credentials (compared without case).
const NAMES: [&str; 9] = [
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
    "x-csrf-token",
    "x-goog-api-key",
    "ocp-apim-subscription-key",
];

/// Query and form parameter names that are credentials there only (a JSON `"code"` is
/// most often an error code): OAuth's authorization code and its PKCE verifier.
const PARAMS: [&str; 2] = ["code", "code_verifier"];

/// Any name containing one of these (without case) is treated as a credential.
const PARTS: [&str; 8] = ["token", "secret", "session", "auth", "passw", "credential", "apikey", "signature"];

/// Any name with one of these as a whole word (split on `-`, `_`, `.`, spaces and
/// camelCase) is treated as a credential: `api_key` and `apiKey` are, `keyboard` is not.
const WORDS: [&str; 7] = ["key", "pass", "pwd", "sig", "jwt", "otp", "pin"];

/// Headers whose value is a URL (its credential parameters are masked).
const URL_HEADERS: [&str; 3] = ["location", "content-location", "referer"];

/// `name`'s words, lower case: split on anything not alphanumeric and before an upper-case
/// letter that follows a lower-case one.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// True when a header, cookie, parameter or JSON key named `name` carries a credential.
pub fn sensitive(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    NAMES.contains(&n.as_str())
        || PARTS.iter().any(|p| n.contains(p))
        || words(name).iter().any(|w| WORDS.contains(&w.as_str()))
}

/// [`sensitive`], or a query / form parameter that is a credential there ([`PARAMS`]).
pub fn sensitive_param(name: &str) -> bool {
    sensitive(name) || PARAMS.contains(&name.trim().to_ascii_lowercase().as_str())
}

/// True when `v` is a HAR log (it has `log.entries`).
pub fn is_har(v: &Value) -> bool {
    v.get("log").and_then(|l| l.get("entries")).is_some_and(Value::is_array)
}

/// Text as stored and how many secrets were removed. `har_name`: the file's name says HAR
/// (`.har`); a JSON text that parses as a HAR is one whatever its name. `Err`: a `.har`
/// that is not valid JSON (refused: it could not be masked field by field).
pub fn redact_text(text: &str, har_name: bool) -> Result<(String, u32), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let head = text.trim_start();
    let looks_json = head.starts_with('{') || head.starts_with('[');
    let mut n = 0u32;
    if har_name || head.starts_with('{') {
        match serde_json::from_str::<Value>(text) {
            Ok(mut v) if is_har(&v) => {
                let n = redact_har(&mut v);
                // Compact: a pretty copy would add a third of the size again.
                return Ok((v.to_string(), n));
            }
            Ok(_) => {}
            Err(e) if har_name => {
                return Err(format!(
                    "it is not valid JSON ({e}), so its cookies and auth headers could not be removed: \
                     export the HAR again"
                ));
            }
            Err(_) => {}
        }
    }
    let masked = if looks_json { scan_mask(text, &mut n) } else { text.to_string() };
    let (out, c) = redact_around_marks(&masked);
    Ok((out, n.saturating_add(count(c))))
}

/// [`redact_counted`] on the text between the [`MARK`]s already in it (with the quotes
/// around one), so a mark is never taken for a value: `sig=[redacted by Mirante]` would
/// become `sig=«redacted» by Mirante]`, and `"key": "[redacted by Mirante]"` lose its quotes.
fn redact_around_marks(text: &str) -> (String, usize) {
    if !text.contains(MARK) {
        return redact_counted(text);
    }
    let b = text.as_bytes();
    let (mut out, mut total, mut at) = (String::with_capacity(text.len()), 0, 0);
    let mut piece = |s: &str, out: &mut String| {
        let (r, c) = redact_counted(s);
        total += c;
        out.push_str(&r);
    };
    let mut from = 0;
    while let Some(i) = text[from..].find(MARK).map(|i| from + i) {
        let (mut s, mut e) = (i, i + MARK.len());
        if s > 0 && matches!(b[s - 1], b'"' | b'\'') && b.get(e) == Some(&b[s - 1]) {
            s -= 1;
            e += 1;
        }
        piece(&text[at..s], &mut out);
        out.push_str(&text[s..e]);
        at = e;
        from = e;
    }
    piece(&text[at..], &mut out);
    (out, total)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn bump(n: &mut u32) {
    *n = n.saturating_add(1);
}

/// Replaces `slot` (a string) with [`MARK`] unless empty or already masked.
fn mask(slot: &mut Value, n: &mut u32) {
    if slot.as_str().is_some_and(|s| !s.is_empty() && s != MARK) {
        *slot = Value::String(MARK.to_string());
        bump(n);
    }
}

/// `[{name, value}]` lists: values of credential names masked (`all`: every value;
/// `params`: query or form parameters, [`sensitive_param`]); the value of a header that
/// holds a URL gets its credential parameters masked.
fn mask_pairs(list: Option<&mut Value>, all: bool, params: bool, n: &mut u32) {
    let Some(items) = list.and_then(Value::as_array_mut) else { return };
    for item in items {
        let Some(obj) = item.as_object_mut() else { continue };
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(v) = obj.get_mut("value") else { continue };
        if all || if params { sensitive_param(&name) } else { sensitive(&name) } {
            mask(v, n);
        } else if URL_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()) {
            mask_url_slot(v, n);
        }
    }
}

/// The URL in `slot` with its credential parameters masked.
fn mask_url_slot(slot: &mut Value, n: &mut u32) {
    if let Some(masked) = slot.as_str().and_then(|u| mask_url(u, n)) {
        *slot = Value::String(masked);
    }
}

/// `k=v&k=v` with the values of credential names masked; `None` when nothing changed.
fn mask_query(query: &str, n: &mut u32) -> Option<String> {
    let mut changed = false;
    let parts: Vec<String> = query
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !v.is_empty() && v != MARK && sensitive_param(&decode_name(k)) => {
                changed = true;
                bump(n);
                format!("{k}={MARK}")
            }
            _ => p.to_string(),
        })
        .collect();
    changed.then(|| parts.join("&"))
}

/// `url` with the values of credential query (and fragment) parameters masked.
fn mask_url(url: &str, n: &mut u32) -> Option<String> {
    let (rest, frag) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (base, query) = match rest.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (rest, None),
    };
    let q = query.map(|q| mask_query(q, n));
    // An OAuth implicit flow puts `access_token=` in the fragment.
    let f = frag.filter(|f| f.contains('=')).map(|f| mask_query(f, n));
    if !matches!(q, Some(Some(_))) && !matches!(f, Some(Some(_))) {
        return None;
    }
    let mut out = base.to_string();
    if let Some(query) = query {
        out.push('?');
        out.push_str(q.flatten().as_deref().unwrap_or(query));
    }
    if let Some(frag) = frag {
        out.push('#');
        out.push_str(f.flatten().as_deref().unwrap_or(frag));
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
            let (out, c) = redact_around_marks(s);
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

/// JSON values of credential keys masked, and the `value` of an object whose `name` is a
/// credential, at any depth.
fn mask_json_keys(v: &mut Value, n: &mut u32) {
    match v {
        Value::Object(map) => {
            let named = map.get("name").and_then(Value::as_str).is_some_and(sensitive);
            for (k, val) in map.iter_mut() {
                if (sensitive(k) || (named && k == "value")) && val.is_string() {
                    mask(val, n);
                    continue;
                }
                mask_json_keys(val, n);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| mask_json_keys(i, n)),
        _ => {}
    }
}

/// A body (`postData.text`, `content.text`) with the values of credential fields masked:
/// form-encoded (by its MIME type) or JSON (by its MIME type or its first character; one
/// that does not parse goes through [`scan_mask`]). `None`: nothing changed.
fn mask_body(text: &str, mime: &str, n: &mut u32) -> Option<String> {
    let mime = mime.to_ascii_lowercase();
    let head = text.trim_start();
    if mime.contains("json") || head.starts_with('{') || head.starts_with('[') {
        let before = *n;
        let out = match serde_json::from_str::<Value>(text) {
            Ok(mut v) => {
                mask_json_keys(&mut v, n);
                v.to_string()
            }
            Err(_) => scan_mask(text, n),
        };
        return (*n != before).then_some(out);
    }
    if mime.contains("x-www-form-urlencoded") {
        return mask_query(text.trim_end(), n);
    }
    None
}

/// `content` (a response's or a `postData`): a base64 `text` decoded (text) or replaced with
/// [`BINARY`] (anything else), then its credential fields masked ([`mask_body`]).
fn redact_body(content: &mut serde_json::Map<String, Value>, n: &mut u32) {
    let base64 = content.get("encoding").and_then(Value::as_str).is_some_and(|e| e.eq_ignore_ascii_case("base64"));
    if base64 {
        let Some(b64) = content.get("text").and_then(Value::as_str) else { return };
        let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let text = super::decode_b64(&clean, super::MAX_FILE).ok().and_then(|b| {
            if b.contains(&0) {
                None
            } else {
                String::from_utf8(b).ok()
            }
        });
        content.remove("encoding");
        let body = text.unwrap_or_else(|| BINARY.to_string());
        content.insert("text".into(), Value::String(body));
    }
    let mime = content.get("mimeType").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(text) = content.get_mut("text") else { return };
    if let Some(masked) = text.as_str().and_then(|t| mask_body(t, &mime, n)) {
        *text = Value::String(masked);
    }
}

/// Masks a HAR log in place; returns how many values were removed.
pub fn redact_har(v: &mut Value) -> u32 {
    let mut n = 0u32;
    if let Some(entries) = v.get_mut("log").and_then(|l| l.get_mut("entries")).and_then(Value::as_array_mut) {
        for e in entries {
            for side in ["request", "response"] {
                let Some(part) = e.get_mut(side).and_then(Value::as_object_mut) else { continue };
                mask_pairs(part.get_mut("headers"), false, false, &mut n);
                mask_pairs(part.get_mut("cookies"), true, false, &mut n);
                mask_pairs(part.get_mut("queryString"), false, true, &mut n);
                if let Some(post) = part.get_mut("postData").and_then(Value::as_object_mut) {
                    mask_pairs(post.get_mut("params"), false, true, &mut n);
                    redact_body(post, &mut n);
                }
                if let Some(content) = part.get_mut("content").and_then(Value::as_object_mut) {
                    redact_body(content, &mut n);
                }
                for key in ["url", "redirectURL"] {
                    if let Some(url) = part.get_mut(key) {
                        mask_url_slot(url, &mut n);
                    }
                }
            }
        }
    }
    // The fields first, then the tokens left in every string (around the marks).
    redact_strings(v, &mut n);
    n
}

/// One open `{…}` or `[…]` while [`scan_mask`] reads.
#[derive(Default)]
struct Frame {
    object: bool,
    /// The next string is a key.
    expect_key: bool,
    key: Option<String>,
    /// Its `"name"` is a credential.
    named: bool,
    /// Where its `"value"` string's contents are.
    value: Option<(usize, usize)>,
    /// Inside a `"cookies"` list: every value is a credential.
    cookies: bool,
}

/// A JSON string's contents decoded (escapes), or as they are when they do not decode.
fn json_str(raw: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string())
}

/// JSON-ish `text` (it may be cut off or invalid) with the string values of credential keys
/// masked, and the string `"value"` of an object whose `"name"` is a credential (or that
/// sits in a `"cookies"` list), without
/// parsing it: a lenient scan of strings, braces, colons and commas. The layout is kept.
pub fn scan_mask(text: &str, n: &mut u32) -> String {
    let b = text.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let close = |f: Frame, spans: &mut Vec<(usize, usize)>| {
        if let (true, Some(v)) = (f.named, f.value) {
            spans.push(v);
        }
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = j.min(b.len());
                if let Some(f) = stack.last_mut().filter(|f| f.object) {
                    let raw = &text[start..end];
                    if f.expect_key {
                        f.key = Some(json_str(raw));
                    } else {
                        match f.key.take().as_deref() {
                            Some("name") => f.named |= sensitive(&json_str(raw)),
                            Some("value") => f.value = Some((start, end)),
                            Some(k) if sensitive(k) => spans.push((start, end)),
                            _ => {}
                        }
                    }
                }
                i = end + 1;
                continue;
            }
            c @ (b'{' | b'[') => {
                let cookies = stack.last_mut().is_some_and(|f| {
                    let inherit = f.key.take().as_deref() == Some("cookies") || (!f.object && f.cookies);
                    inherit
                });
                let object = c == b'{';
                stack.push(Frame { object, expect_key: object, named: object && cookies, cookies, ..Frame::default() });
            }
            b'}' | b']' => {
                if let Some(f) = stack.pop() {
                    close(f, &mut spans);
                }
            }
            b':' => {
                if let Some(f) = stack.last_mut() {
                    f.expect_key = false;
                }
            }
            b',' => {
                if let Some(f) = stack.last_mut().filter(|f| f.object) {
                    f.expect_key = true;
                    f.key = None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    while let Some(f) = stack.pop() {
        close(f, &mut spans);
    }
    spans.retain(|&(s, e)| s < e && &text[s..e] != MARK);
    spans.sort_unstable();
    spans.dedup();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in spans {
        if s < at {
            continue;
        }
        out.push_str(&text[at..s]);
        out.push_str(MARK);
        bump(n);
        at = e;
    }
    out.push_str(&text[at..]);
    out
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
        let (out, n) = redact_text(&text, true).unwrap();
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
        let (out, n) = redact_text(&har(&token).to_string(), false).unwrap();
        assert!(!out.contains(&token) && n > 0);
        let log = format!("GET /x?token={token}\nok\n");
        let (out, n) = redact_text(&log, false).unwrap();
        assert!(!out.contains(&token), "{out}");
        assert!(n >= 1);
        let plain = "{\"a\": 1}";
        assert_eq!(redact_text(plain, false).unwrap(), (plain.to_string(), 0));
    }

    #[test]
    fn a_har_that_is_not_json_is_refused_and_one_with_a_bom_is_read() {
        let token = gh_token();
        let e = redact_text(&format!("broken {token}"), true).unwrap_err();
        assert!(e.contains("not valid JSON"), "{e}");
        // Cut off: refused too, never stored half-masked.
        let text = har(&token).to_string();
        assert!(redact_text(&text[..text.len() / 2], true).is_err());
        let (out, n) = redact_text(&format!("\u{feff}{text}"), true).unwrap();
        assert!(!out.contains(&token) && !out.contains("abc123") && n >= 12, "{out}");
        // Stored compact (a pretty copy would add a third again).
        assert!(!out.contains('\n'), "{out}");
    }

    #[test]
    fn json_that_does_not_parse_still_loses_credential_values_by_a_scan() {
        let token = gh_token();
        // A HAR cut off, under another name: no parse, so the scan masks it.
        let text = har(&token).to_string();
        let cut = &text[..text.find("\"response\"").unwrap()];
        let (out, n) = redact_text(cut, false).unwrap();
        assert!(!out.contains(&token) && !out.contains("abc123") && !out.contains("csrf-value"), "{out}");
        assert!(out.contains("application/json") && out.contains("\"ana\""), "{out}");
        assert!(n >= 5, "{n}");
        // A value before its name, nested objects, layout kept.
        let mut n = 0;
        let src = "[\n  {\"value\": \"v1\", \"name\": \"Cookie\"},\n  {\"name\": \"Accept\", \"value\": \"*/*\"},\n  \
                   {\"password\": \"hunter2\", \"inner\": {\"api_key\": \"k\\\"q\", \"ok\": \"yes\"}}\n";
        let out = scan_mask(src, &mut n);
        assert_eq!(
            out,
            format!(
                "[\n  {{\"value\": \"{MARK}\", \"name\": \"Cookie\"}},\n  {{\"name\": \"Accept\", \"value\": \"*/*\"}},\n  \
                 {{\"password\": \"{MARK}\", \"inner\": {{\"api_key\": \"{MARK}\", \"ok\": \"yes\"}}}}\n"
            )
        );
        assert_eq!(n, 3);
        // Valid JSON that is no HAR keeps its layout and loses credential values.
        let cfg = "{\n  \"user\": \"ana\",\n  \"client_secret\": \"s3\"\n}\n";
        let (out, n) = redact_text(cfg, false).unwrap();
        assert_eq!(out, format!("{{\n  \"user\": \"ana\",\n  \"client_secret\": \"{MARK}\"\n}}\n"));
        assert_eq!(n, 1);
    }

    #[test]
    fn more_credential_names_match_by_word_and_obvious_non_secrets_do_not() {
        for n in [
            "pass",
            "passwd",
            "pwd",
            "Password",
            "user_password",
            "key",
            "api_key",
            "apiKey",
            "api-key",
            "x-goog-api-key",
            "Ocp-Apim-Subscription-Key",
            "sig",
            "X-Amz-Signature",
            "credential",
            "aws_credentials",
            "id_token",
            "jwt",
        ] {
            assert!(sensitive(n), "{n}");
        }
        for n in ["keyboard", "monkey", "passage_id", "signal", "design", "Keep-Alive", "code", "status_code"] {
            assert!(!sensitive(n), "{n}");
        }
        // OAuth's code and verifier: parameters only.
        assert!(sensitive_param("code") && sensitive_param("code_verifier") && !sensitive_param("status_code"));
        assert!(!sensitive_param("page"));
    }

    #[test]
    fn har_bodies_redirects_and_location_headers_lose_their_credentials() {
        let token = gh_token();
        let b64 = |s: &str| {
            const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for c in s.as_bytes().chunks(3) {
                let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
                for i in 0..4 {
                    out.push(if i <= c.len() { A[(v >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
                }
            }
            out
        };
        let v = json!({ "log": { "entries": [
            { "request": { "url": "https://h.test/login",
                "headers": [{ "name": "x-goog-api-key", "value": "g1" }, { "name": "Referer", "value": "https://h.test/?sig=s1&a=1" }],
                "queryString": [{ "name": "code", "value": "c1" }, { "name": "keyboard", "value": "qwerty" }],
                "postData": { "mimeType": "application/x-www-form-urlencoded", "text": "user=ana&password=p1&code=c2" } },
              "response": { "status": 302, "redirectURL": "https://h.test/cb#access_token=t1&state=x",
                "headers": [{ "name": "Location", "value": "https://h.test/cb?code=c3&state=x" }],
                "content": { "mimeType": "application/json", "text": "{\"access_token\":\"t2\",\"code\":404,\"user\":\"ana\"}" } } },
            { "request": { "url": "https://h.test/api",
                "postData": { "mimeType": "application/json", "text": "{\"pwd\":\"p2\",\"q\":\"ok\"}" } },
              "response": { "content": { "mimeType": "application/json", "encoding": "base64",
                "text": b64(&format!("{{\"refresh_token\":\"t3\",\"note\":\"{token}\"}}")) } } },
            { "request": { "url": "https://h.test/img.png" },
              "response": { "content": { "mimeType": "image/png", "encoding": "base64", "text": b64("\u{0}\u{1}PNG") } } },
        ]}});
        let (out, _) = redact_text(&v.to_string(), true).unwrap();
        for secret in ["g1", "s1", "c1", "p1", "c2", "t1", "c3", "t2", "p2", "t3", &token] {
            assert!(!out.contains(&format!("{secret}\"")) && !out.contains(&format!("={secret}")), "{secret}: {out}");
        }
        let o: Value = serde_json::from_str(&out).unwrap();
        let e = &o["log"]["entries"];
        assert_eq!(e[0]["request"]["queryString"][1]["value"], "qwerty");
        assert_eq!(e[0]["request"]["headers"][1]["value"], format!("https://h.test/?sig={MARK}&a=1"));
        assert_eq!(e[0]["request"]["postData"]["text"], format!("user=ana&password={MARK}&code={MARK}"));
        assert_eq!(e[0]["response"]["redirectURL"], format!("https://h.test/cb#access_token={MARK}&state=x"));
        assert_eq!(e[0]["response"]["headers"][0]["value"], format!("https://h.test/cb?code={MARK}&state=x"));
        // A JSON "code" (an error code) is no credential.
        let body: Value = serde_json::from_str(e[0]["response"]["content"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body, json!({ "access_token": MARK, "code": 404, "user": "ana" }));
        let body: Value = serde_json::from_str(e[1]["request"]["postData"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body, json!({ "pwd": MARK, "q": "ok" }));
        // A base64 text body is decoded, then masked; a binary one is removed.
        assert!(e[1]["response"]["content"].get("encoding").is_none());
        let body = e[1]["response"]["content"]["text"].as_str().unwrap();
        assert!(body.contains(&format!("\"refresh_token\":\"{MARK}\"")), "{body}");
        assert_eq!(e[2]["response"]["content"]["text"], BINARY);
        assert!(e[2]["response"]["content"].get("encoding").is_none());
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
