//! Best-effort masking of credentials in text that Mirante shows or stores: feed step
//! summaries, and memory proposals written by agents. It lowers what leaks, it is no
//! guarantee.
/// What a masked credential reads as.
const REDACTED: &str = "«redacted»";

/// `text` with the credentials a command, URL or note commonly carries masked: an
/// `Authorization:` header value, a `Bearer` token, the `user:password@` of a URL,
/// assignments whose name says secret (`*_KEY=`, `*TOKEN*=`, `*SECRET*=`, `*PASSWORD*=`,
/// any case), query parameters such as `?token=` / `&api_key=` / `&sig=`, PEM private key
/// blocks, and provider tokens (`AKIA…`, `ghp_…`, `github_pat_…`, `sk-…`, `xox?-…`, JWTs).
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
        if !label.ends_with("PRIVATE KEY") || !b[label_end..].starts_with(b"-----") {
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

const PREFIXED: &[Prefixed] = &[
    (b"AKIA", 20, |c| c.is_ascii_uppercase() || c.is_ascii_digit()),
    (b"ghp_", 20, |c| c.is_ascii_alphanumeric() || c == b'_'),
    (b"gho_", 20, |c| c.is_ascii_alphanumeric() || c == b'_'),
    (b"ghs_", 20, |c| c.is_ascii_alphanumeric() || c == b'_'),
    (b"github_pat_", 20, |c| c.is_ascii_alphanumeric() || c == b'_'),
    (b"sk-", 20, |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
    (b"xoxa-", 10, |c| c.is_ascii_alphanumeric() || c == b'-'),
    (b"xoxb-", 10, |c| c.is_ascii_alphanumeric() || c == b'-'),
    (b"xoxp-", 10, |c| c.is_ascii_alphanumeric() || c == b'-'),
    (b"xoxs-", 10, |c| c.is_ascii_alphanumeric() || c == b'-'),
];

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
        let hit = hit.or_else(|| {
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
    let up = name.to_ascii_uppercase();
    let env = up.ends_with("_KEY") || ["TOKEN", "SECRET", "PASSWORD", "PASSWD"].iter().any(|w| up.contains(w));
    env || (in_query && ["KEY", "PASS", "SIG", "AUTH", "CREDENTIAL"].iter().any(|w| up.contains(w)))
}

fn redact_assignments(text: &str) -> String {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'=' {
            i += 1;
            continue;
        }
        let name_start = b[..i].iter().rposition(|c| !(c.is_ascii_alphanumeric() || *c == b'_')).map_or(0, |p| p + 1);
        let in_query = name_start > 0 && matches!(b[name_start - 1], b'?' | b'&');
        let start = i + 1;
        if name_start == i || !secret_name(&text[name_start..i], in_query) {
            i = start;
            continue;
        }
        let end = match b.get(start) {
            Some(q @ (b'"' | b'\'')) => b[start + 1..].iter().position(|c| c == q).map_or(b.len(), |p| start + p + 2),
            _ => value_end(b, start, b"\"'&;#)`"),
        };
        if end > start {
            ranges.push((start, end));
        }
        i = end.max(start);
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
