//! Best-effort masking of credentials in text that Mirante shows or stores: feed step
//! summaries, and memory proposals written by agents. It lowers what leaks, it is no
//! guarantee.
/// What a masked credential reads as.
const REDACTED: &str = "«redacted»";

/// How many masked values `text` shows (text that went through [`redact`] earlier, e.g. a
/// mined proposal built from redacted sentences).
pub fn masked_count(text: &str) -> usize {
    text.matches(REDACTED).count()
}

/// `text` with the credentials a command, URL or note commonly carries masked: an
/// `Authorization:` header value, a `Bearer` token, the `user:password@` of a URL,
/// assignments whose name says secret (`*_KEY=`, `*TOKEN*=`, `*SECRET*=`, `*PASSWORD*=`,
/// any case, also spaced `key = v`, YAML `password: v` and JSON `"api_key": "v"`), query
/// parameters such as `?token=` / `&api_key=` / `&sig=`, PEM and PGP private key blocks,
/// and provider tokens (AWS `AKIA…`/`ASIA…`, GitHub `ghp_`/`gho_`/`ghs_`/`ghu_`/`ghr_`/
/// `github_pat_`, OpenAI/Anthropic `sk-…`, Slack `xox?-…`/`xapp-…`, Google `AIza…`,
/// Stripe `sk_live_`/`rk_live_`, GitLab `glpat-`, npm `npm_`, Hugging Face `hf_`, JWTs).
/// Best effort: it lowers what leaks, it is no guarantee.
pub fn redact(text: &str) -> String {
    redact_counted(text).0
}

/// [`redact`], plus how many values it masked (`«redacted»` markers added beyond those
/// already in the input).
pub fn redact_counted(text: &str) -> (String, usize) {
    let before = text.matches(REDACTED).count();
    let out = redact_pem(text);
    let out = redact_header(&out);
    let out = redact_bearer(&out);
    let out = redact_userinfo(&out);
    let out = redact_assignments(&out);
    let out = redact_prefixed(&out);
    let n = out.matches(REDACTED).count().saturating_sub(before);
    (out, n)
}

/// Masks `-----BEGIN … PRIVATE KEY-----` through the matching `-----END …-----` (or the end
/// of the text when the block is cut off).
fn redact_pem(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(i) = find_ci(b, at, b"-----BEGIN ") {
        let label_start = i + "-----BEGIN ".len();
        let label_end = b[label_start..]
            .iter()
            .position(|c| !(c.is_ascii_uppercase() || *c == b' '))
            .map_or(b.len(), |p| label_start + p);
        let label = &text[label_start..label_end];
        let private = label.ends_with("PRIVATE KEY") || label.ends_with("PRIVATE KEY BLOCK");
        if !private || !b[label_end..].starts_with(b"-----") {
            at = label_start;
            continue;
        }
        let footer = format!("-----END {label}-----");
        let end = find_ci(b, label_end, footer.as_bytes()).map_or(b.len(), |j| j + footer.len());
        ranges.push((i, end));
        at = end;
    }
    mask(text, &ranges)
}

/// A token prefix, the minimum length of the whole token, and its other bytes.
type Prefixed = (&'static [u8], usize, fn(u8) -> bool);

fn upper_digit(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

fn alnum_us(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn alnum_dash(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

fn alnum_us_dash(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

const PREFIXED: &[Prefixed] = &[
    (b"AKIA", 20, upper_digit),
    (b"ASIA", 20, upper_digit),
    (b"ghp_", 20, alnum_us),
    (b"gho_", 20, alnum_us),
    (b"ghs_", 20, alnum_us),
    (b"ghu_", 20, alnum_us),
    (b"ghr_", 20, alnum_us),
    (b"github_pat_", 20, alnum_us),
    (b"sk-", 20, alnum_us_dash),
    (b"xapp-", 15, alnum_dash),
    (b"AIza", 30, alnum_us_dash),
    (b"sk_live_", 20, alnum),
    (b"rk_live_", 20, alnum),
    (b"sk_test_", 20, alnum),
    (b"rk_test_", 20, alnum),
    (b"glpat-", 20, alnum_us_dash),
    (b"npm_", 30, alnum),
    (b"hf_", 30, alnum),
];

/// A Slack token: `xox` + a lowercase letter + `-` or `.` (`xoxb-…`, `xoxe.xoxp-…`).
fn slack_token(b: &[u8], i: usize) -> Option<usize> {
    let t = &b[i..];
    let is_slack = t.len() > 5 && t.starts_with(b"xox") && t[3].is_ascii_lowercase() && matches!(t[4], b'-' | b'.');
    if !is_slack {
        return None;
    }
    let end = i + t.iter().position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.'))).unwrap_or(t.len());
    (end - i >= 10).then_some(end)
}

fn jwt_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
}

/// Masks provider tokens by prefix, and JWTs (`eyJ….eyJ…`). A token starts at a word
/// boundary.
fn redact_prefixed(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || matches!(b[i - 1], b'_' | b'-'));
        if !boundary {
            i += 1;
            continue;
        }
        let hit = PREFIXED.iter().find_map(|(prefix, min, ok)| {
            let rest = b[i..].strip_prefix(*prefix)?;
            let end = i + prefix.len() + rest.iter().position(|c| !ok(*c)).unwrap_or(rest.len());
            (end - i >= *min).then_some(end)
        });
        let hit = hit.or_else(|| slack_token(b, i)).or_else(|| {
            if !b[i..].starts_with(b"eyJ") {
                return None;
            }
            let end = b[i..].iter().position(|c| !jwt_byte(*c)).map_or(b.len(), |p| i + p);
            find_ci(&b[..end], i + 3, b".eyJ").map(|_| end)
        });
        match hit {
            Some(end) => {
                ranges.push((i, end));
                i = end;
            }
            None => i += 1,
        }
    }
    mask(text, &ranges)
}

/// Index of the first case-insensitive `needle` in `hay` at or after `from`.
fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let last = hay.len().checked_sub(needle.len())?;
    (from..=last).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// End of a value starting at `from`: the first byte in `stops`, or the end.
fn value_end(b: &[u8], from: usize, stops: &[u8]) -> usize {
    b[from..].iter().position(|c| c.is_ascii_whitespace() || stops.contains(c)).map_or(b.len(), |p| from + p)
}

/// Replaces each `(start, end)` byte range of `text` (ascending, on ASCII boundaries).
fn mask(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for &(start, end) in ranges {
        out.push_str(&text[last..start]);
        out.push_str(REDACTED);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

fn redact_header(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(i) = find_ci(b, at, b"authorization:") {
        let mut start = i + "authorization:".len();
        while b.get(start).is_some_and(|c| *c == b' ' || *c == b'\t') {
            start += 1;
        }
        let end = b[start..].iter().position(|c| b"\r\n\"'".contains(c)).map_or(b.len(), |p| start + p);
        if end > start {
            ranges.push((start, end));
        }
        at = end.max(i + 1);
    }
    mask(text, &ranges)
}

fn redact_bearer(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(i) = find_ci(b, at, b"bearer ") {
        let mut start = i + "bearer ".len();
        at = start;
        if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
            continue;
        }
        while b.get(start) == Some(&b' ') {
            start += 1;
        }
        let end = value_end(b, start, b"\"'");
        if end > start {
            ranges.push((start, end));
            at = end;
        }
    }
    mask(text, &ranges)
}

fn redact_userinfo(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(i) = find_ci(b, at, b"://") {
        let start = i + 3;
        let end = value_end(b, start, b"/?#\"'");
        if let Some(userinfo) = b[start..end].iter().rposition(|c| *c == b'@') {
            if b[start..start + userinfo].contains(&b':') {
                ranges.push((start, start + userinfo));
            }
        }
        at = end.max(start);
    }
    mask(text, &ranges)
}

/// A `name=` whose value is masked. `in_query`: the name follows `?` or `&`.
fn secret_name(name: &str, in_query: bool) -> bool {
    let up = name.to_ascii_uppercase().replace('-', "_");
    let env = up.ends_with("_KEY") || ["TOKEN", "SECRET", "PASSWORD", "PASSWD"].iter().any(|w| up.contains(w));
    env || (in_query && ["KEY", "PASS", "SIG", "AUTH", "CREDENTIAL"].iter().any(|w| up.contains(w)))
}

/// A `name:` whose value is masked: stricter than `=` (code is full of `token_len: usize`),
/// the name must end with the secret word.
fn secret_key(name: &str) -> bool {
    let up = name.to_ascii_uppercase().replace('-', "_");
    ["_KEY", "APIKEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD"].iter().any(|w| up.ends_with(w))
}

/// Type names that follow `name:` in code (`password: String`, `token: Option<&str>`,
/// `secret: string`): not values.
fn is_type_name(v: &str) -> bool {
    const TYPES: &[&str] = &[
        "String",
        "Option",
        "Vec",
        "Box",
        "Arc",
        "Rc",
        "Cow",
        "Secret",
        "SecretString",
        "SecretBox",
        "PathBuf",
        "Path",
        "Bytes",
        "Uuid",
        "str",
        "string",
        "number",
        "boolean",
        "bool",
        "any",
        "unknown",
        "usize",
        "u8",
        "u16",
        "u32",
        "u64",
        "i32",
        "i64",
    ];
    let v = v.trim_start_matches('&');
    let v = v.strip_prefix("'static ").unwrap_or(v);
    let ident = v.split(['<', '[', '?']).next().unwrap_or_default();
    TYPES.contains(&ident)
}

fn redact_assignments(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let sep = b[i];
        if sep != b'=' && sep != b':' {
            i += 1;
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { 0 };
        let next = b.get(i + 1).copied().unwrap_or(0);
        let comparison = sep == b'=' && (matches!(next, b'=' | b'>') || matches!(prev, b'=' | b'!' | b'<' | b'>'));
        let path_or_url = sep == b':' && (matches!(next, b':' | b'/' | b'=') || prev == b':');
        if comparison || path_or_url {
            i += 1;
            continue;
        }
        // The name: back over `:` of `:=`, spaces, and a closing quote (`"api_key": …`).
        let mut j = if sep == b'=' && prev == b':' { i - 1 } else { i };
        while j > 0 && matches!(b[j - 1], b' ' | b'\t') {
            j -= 1;
        }
        let spaced = j < i;
        if j > 0 && matches!(b[j - 1], b'"' | b'\'') {
            j -= 1;
        }
        let name_end = j;
        let name_start = b[..name_end]
            .iter()
            .rposition(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')))
            .map_or(0, |p| p + 1);
        if name_start == name_end {
            i += 1;
            continue;
        }
        let name = &text[name_start..name_end];
        let in_query = !spaced && name_start > 0 && matches!(b[name_start - 1], b'?' | b'&');
        let secret = if sep == b'=' { secret_name(name, in_query) } else { secret_key(name) };
        if !secret {
            i += 1;
            continue;
        }
        let mut start = i + 1;
        while matches!(b.get(start), Some(b' ' | b'\t')) {
            start += 1;
        }
        let quoted = matches!(b.get(start), Some(b'"' | b'\''));
        let end = match b.get(start) {
            Some(q @ (b'"' | b'\'')) => b[start + 1..].iter().position(|c| c == q).map_or(b.len(), |p| start + p + 2),
            _ if sep == b':' => value_end(b, start, b"\"'&;#)`,}"),
            _ => value_end(b, start, b"\"'&;#)`"),
        };
        if end > start && !(sep == b':' && !quoted && is_type_name(&text[start..end])) {
            ranges.push((start, end));
        }
        i = end.max(i + 1);
    }
    mask(text, &ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_summaries_redact_credentials() {
        let cases = [
            ("curl -H 'Authorization: Bearer abc.def' x", "curl -H 'Authorization: «redacted»' x"),
            ("curl -H \"authorization: token ghp_1\" -v", "curl -H \"authorization: «redacted»\" -v"),
            ("echo Bearer s3cr3t-tok done", "echo Bearer «redacted» done"),
            ("OPENAI_API_KEY=sk-123 cargo run", "OPENAI_API_KEY=«redacted» cargo run"),
            ("export GH_TOKEN=\"ghp abc\" && x", "export GH_TOKEN=«redacted» && x"),
            ("MY_SECRET_VALUE=a1;ls", "MY_SECRET_VALUE=«redacted»;ls"),
            ("PASSWORD=hunter2 psql", "PASSWORD=«redacted» psql"),
            ("db_password=hunter2 psql", "db_password=«redacted» psql"),
            ("git clone https://user:pa55@github.com/a/b", "git clone https://«redacted»@github.com/a/b"),
            (
                "curl 'https://api.x/v1?token=abc&q=1&api_key=k#f'",
                "curl 'https://api.x/v1?token=«redacted»&q=1&api_key=«redacted»#f'",
            ),
            ("curl https://x/y?sig=zz", "curl https://x/y?sig=«redacted»"),
            ("cargo test --workspace", "cargo test --workspace"),
            ("KEYBOARD=us MONKEY=1 a=b", "KEYBOARD=us MONKEY=1 a=b"),
            ("ssh://git@github.com/a/b", "ssh://git@github.com/a/b"),
            ("café Bearer ü", "café Bearer «redacted»"),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn masks_provider_tokens_and_private_keys() {
        let s = "k=AKIAABCDEFGHIJKLMNOP gh ghp_0123456789abcdefghijABCDEFGHIJ012345 \
                 sk-ant-api03-abcDEF_123 xoxb-1-2-abc eyJhbGciOi.eyJzdWIi.sig";
        let r = redact(s);
        for leak in ["AKIAABCDEFGHIJKLMNOP", "ghp_0123", "sk-ant-api03", "xoxb-1", "eyJhbGciOi.eyJzdWIi"] {
            assert!(!r.contains(leak), "{leak} leaked: {r}");
        }
        let pem = "a\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nb";
        assert_eq!(redact(pem), "a\n«redacted»\nb");
    }

    #[test]
    fn masks_more_providers_and_pgp_keys() {
        let tokens = [
            "xoxr-1234567890-abcdef",
            "xoxe.xoxp-1-abcdefghijkl",
            "xoxc-1234567890-abcdef",
            "xoxd-1234567890abcdef",
            "xapp-1-A0123456789-0123456789-abcdef",
            "ghu_0123456789abcdefghijABCDEFGHIJ0123",
            "ghr_0123456789abcdefghijABCDEFGHIJ0123",
            "ASIAABCDEFGHIJKLMNOP",
            "AIzaSyA0123456789abcdefghijklmnopqrstu",
            concat!("sk_", "live_0123456789abcdefghijklmn"),
            concat!("rk_", "live_0123456789abcdefghijklmn"),
            "glpat-0123456789abcdefghij",
            "npm_0123456789abcdefghijklmnopqrstuvwxyz",
            "hf_0123456789abcdefghijklmnopqrstuvwxyzAB",
        ];
        for t in tokens {
            let r = redact(&format!("use {t} now"));
            assert_eq!(r, "use «redacted» now", "{t}");
        }
        let pgp = "x\n-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOY\n-----END PGP PRIVATE KEY BLOCK-----\ny";
        assert_eq!(redact(pgp), "x\n«redacted»\ny");
    }

    #[test]
    fn masks_spaced_colon_and_quoted_assignments() {
        let cases = [
            ("api_key = abc123", "api_key = «redacted»"),
            ("password: hunter2", "password: «redacted»"),
            ("  db_password:  'hunter2'", "  db_password:  «redacted»"),
            ("{\"api_key\": \"v4lue\", \"x\": 1}", "{\"api_key\": «redacted», \"x\": 1}"),
            ("SECRET = \"a b\"", "SECRET = «redacted»"),
            ("--password=hunter2", "--password=«redacted»"),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
        for code in [
            "if token == expected { }",
            "struct C { password: String, token: Option<String> }",
            "let n = token::parse(x);",
            "fn f(password: &str) {}",
            "interface A { secret: string }",
        ] {
            assert_eq!(redact(code), code);
        }
    }

    #[test]
    fn counts_what_it_masked() {
        let (text, n) = redact_counted("TOKEN=abc and ghp_0123456789abcdefghijABCDEFGHIJ012345");
        assert_eq!(n, 2);
        assert!(!text.contains("abc and") || text.contains("«redacted» and"));
    }

    #[test]
    fn leaves_ordinary_code_alone() {
        let code = "fn key_for(token_len: usize) -> usize { token_len * 2 }";
        assert_eq!(redact(code), code);
    }
}
