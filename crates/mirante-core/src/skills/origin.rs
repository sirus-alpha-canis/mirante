//! Origins: which client a repository belongs to.
//!
//! A task's skills follow its repository's `origin` remote, normalized into a key
//! `host/seg/…` (lowercase, no credentials, no `.git`), whatever form the URL takes: scp-like
//! `user@host:path`, `ssh://`, `https://user:token@host/…`, `git://`, an `~/.ssh/config`
//! alias, a `<forge>-<suffix>` host alias, or one of Azure DevOps's four spellings. The
//! tenant (the boundary the miner never crosses) is the host plus the first segment.
//!
//! The URL is read raw (`git config --no-includes --get remote.origin.url` through the
//! hardened git), never through `git remote get-url`, which applies `insteadOf` rewrites
//! an agent could plant in the repo config. `~/.ssh/config` is parsed for literal
//! `Host`/`HostName` pairs only: no wildcards, no `Match`, no `Include`, never `ssh -G`.
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::SkillError;
use crate::dna::sha256_hex;
use crate::git::Git;

/// Largest `~/.ssh/config` read.
const MAX_SSH_CONFIG: u64 = 256 * 1024;

/// Hosts whose `<forge>-<suffix>` aliases (`github.com-work`) mean the forge itself.
const FORGES: [&str; 3] = ["github.com", "gitlab.com", "bitbucket.org"];

/// The URL may carry a token: an error says what is wrong with it, never quotes it.
fn err(why: &str) -> SkillError {
    SkillError::Origin(why.to_string())
}

/// A normalized origin: lowercase host, lowercase path segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub host: String,
    pub segments: Vec<String>,
}

impl Origin {
    /// `host/seg/…`.
    pub fn key(&self) -> String {
        std::iter::once(self.host.as_str())
            .chain(self.segments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Literal `Host` aliases of `~/.ssh/config`, lowercased, mapped to their `HostName`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Aliases(HashMap<String, String>);

impl Aliases {
    pub fn get(&self, host: &str) -> Option<&str> {
        self.0.get(host).map(String::as_str)
    }
}

/// The literal `Host alias` → `HostName` pairs of an ssh config text. Keywords are
/// case-insensitive and take `key value` or `key=value`. A `Host` line's patterns with
/// `*`, `?` or `!` are skipped (a line with a negation is skipped whole); `Match` starts a
/// block that maps nothing; `Include` is ignored. The first `HostName` of an alias wins,
/// as in ssh; a `HostName` with `%` tokens is skipped.
pub fn aliases(text: &str) -> Aliases {
    let mut map = HashMap::new();
    let mut current: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = match line.find(|c: char| c.is_whitespace() || c == '=') {
            Some(i) => (&line[..i], line[i..].trim_start_matches(|c: char| c.is_whitespace() || c == '=')),
            None => (line, ""),
        };
        let rest = rest.trim();
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                let patterns: Vec<&str> = rest.split_whitespace().collect();
                current = if patterns.iter().any(|p| p.starts_with('!')) {
                    Vec::new()
                } else {
                    patterns
                        .into_iter()
                        .filter(|p| !p.contains(['*', '?']))
                        .map(|p| p.trim_matches('"').to_ascii_lowercase())
                        .filter(|p| !p.is_empty())
                        .collect()
                };
            }
            "match" => current = Vec::new(),
            "hostname" => {
                let value = rest.trim_matches('"').to_ascii_lowercase();
                if value.is_empty() || value.contains('%') || value.contains(char::is_whitespace) {
                    continue;
                }
                for alias in &current {
                    map.entry(alias.clone()).or_insert_with(|| value.clone());
                }
            }
            _ => {}
        }
    }
    Aliases(map)
}

/// [`aliases`] of `<home>/.ssh/config`, read without following a symlink and at most
/// 256 KiB; any error gives no aliases.
pub fn read_aliases(home: &Path) -> Aliases {
    let read = || -> std::io::Result<String> {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(home.join(".ssh/config"))?;
        if !f.metadata()?.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let mut text = String::new();
        f.take(MAX_SSH_CONFIG).read_to_string(&mut text)?;
        Ok(text)
    };
    read().map(|t| aliases(&t)).unwrap_or_default()
}

/// `%XX` escapes decoded; an error for a bad escape or a result that is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !h.starts_with(['.', '-'])
}

/// The path's segments: split on `/`, each percent-decoded, lowercased and checked (no
/// empty segment, no `.` or `..`, no `/`, `\` or control character after decoding).
fn segments(path: &str) -> Result<Vec<String>, SkillError> {
    let mut path = path.trim_start_matches('/').trim_end_matches('/');
    if let Some(p) = path.strip_suffix(".git") {
        path = p.trim_end_matches('/');
    }
    if path.is_empty() {
        return Err(err("no repository path"));
    }
    let mut out = Vec::new();
    for raw in path.split('/') {
        let seg = percent_decode(raw).ok_or_else(|| err("bad percent escape"))?.to_lowercase();
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(err("empty, . or .. path segment"));
        }
        if seg.contains(['/', '\\']) || seg.chars().any(char::is_control) || seg.chars().count() > 200 {
            return Err(err("path segment with a separator or control character"));
        }
        out.push(seg);
    }
    Ok(out)
}

/// The host as written, mapped: an exact ssh alias (ssh forms only) to its `HostName`,
/// else a `<forge>-<suffix>` alias to the forge; lowercased.
fn map_host(host: &str, ssh: bool, a: &Aliases) -> String {
    let host = host.to_ascii_lowercase();
    if ssh {
        if let Some(real) = a.get(&host) {
            return real.to_string();
        }
    }
    for forge in FORGES {
        if host.strip_prefix(forge).is_some_and(|rest| rest.starts_with('-') && rest.len() > 1) {
            return forge.to_string();
        }
    }
    host
}

/// Azure DevOps's spellings as `dev.azure.com/<org>/<project>/<repo>`.
fn azure(host: &str, segs: Vec<String>) -> (String, Vec<String>) {
    let s: Vec<&str> = segs.iter().map(String::as_str).collect();
    let azure = |v: &[&str]| ("dev.azure.com".to_string(), v.iter().map(|x| x.to_string()).collect());
    match (host, &s[..]) {
        ("ssh.dev.azure.com" | "vs-ssh.visualstudio.com", ["v3", o, p, r]) => azure(&[o, p, r]),
        ("dev.azure.com", [o, p, "_git", r]) => azure(&[o, p, r]),
        (h, rest) if h.ends_with(".visualstudio.com") && h != "vs-ssh.visualstudio.com" => {
            let org = h.trim_end_matches(".visualstudio.com");
            let rest = match rest {
                ["defaultcollection", tail @ ..] => tail,
                all => all,
            };
            match rest {
                [p, "_git", r] => azure(&[org, p, r]),
                _ => (host.to_string(), segs),
            }
        }
        _ => (host.to_string(), segs),
    }
}

/// The normalized origin of a remote URL. Errors: empty, `file://` or a local path (use
/// [`local_key`]), an unknown scheme, a bad host, no path, or a bad segment. The userinfo
/// (a user, a token) is dropped and never appears in an error.
pub fn normalize(url: &str, a: &Aliases) -> Result<Origin, SkillError> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_control) {
        return Err(err("empty, or a control character"));
    }
    let (ssh, authority, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let ssh = match scheme.to_ascii_lowercase().as_str() {
            "ssh" | "git+ssh" | "ssh+git" => true,
            "https" | "http" | "git" => false,
            "file" => return Err(err("a local repository")),
            _ => return Err(err("unknown scheme")),
        };
        let (authority, path) = rest.split_once('/').ok_or_else(|| err("no repository path"))?;
        // `user:token@host:port`: the userinfo ends at the last `@`; then the port.
        let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match hostport.rsplit_once(':') {
            Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
            Some(_) => return Err(err("bad port")),
            None => hostport,
        };
        (ssh, host, path)
    } else {
        // scp-like `[user@]host:path`: a `:` before any `/`, and not a local path.
        let colon = url.find(':').ok_or_else(|| err("a local repository"))?;
        if url[..colon].contains('/') || url.starts_with(['.', '/']) {
            return Err(err("a local repository"));
        }
        let host = url[..colon].rsplit_once('@').map_or(&url[..colon], |(_, h)| h);
        (true, host, &url[colon + 1..])
    };
    if !valid_host(authority) {
        return Err(err("bad host"));
    }
    let host = map_host(authority, ssh, a);
    if !valid_host(&host) {
        return Err(err("bad host"));
    }
    let (host, segments) = azure(&host, segments(path)?);
    Ok(Origin { host, segments })
}

/// The key of a repository without a usable origin: `local/<first 16 hex of
/// sha256(canonical path)>`. Only `repo` and `personal` scopes match it.
pub fn local_key(repo: &Path) -> String {
    let path = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    format!("local/{}", &sha256_hex(path.to_string_lossy().as_bytes())[..16])
}

/// The origin key of `repo`: its raw `remote.origin.url` (hardened git, no includes, no
/// `insteadOf`), normalized; [`local_key`] when there is none or it does not normalize.
pub fn origin_of(repo: &Path, a: &Aliases) -> String {
    let out = Git::checkout(repo).args(["config", "--no-includes", "--get", "remote.origin.url"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let url = String::from_utf8_lossy(&o.stdout).trim_end_matches(['\n', '\r']).to_string();
            normalize(&url, a).map(|o| o.key()).unwrap_or_else(|_| local_key(repo))
        }
        _ => local_key(repo),
    }
}

/// The tenant of an origin key: its host and first segment (`dev.azure.com/<org>`,
/// `github.com/<owner>`, `local/<hash>`).
pub fn tenant(key: &str) -> String {
    key.split('/').take(2).collect::<Vec<_>>().join("/")
}

/// Which repositories a skill applies to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Every repository, every client.
    Personal,
    /// Repositories whose key has these segments as a prefix (at least host + 1).
    Org(String),
    /// Exactly this repository key.
    Repo(String),
}

/// A key's segments, each non-empty, at least `min` of them.
fn key_segments(v: &str, min: usize) -> Option<Vec<&str>> {
    let segs: Vec<&str> = v.split('/').collect();
    let ok = segs.len() >= min
        && v.len() <= 1024
        && segs.iter().all(|s| !s.is_empty() && *s != "." && *s != ".." && !s.chars().any(char::is_control));
    ok.then_some(segs)
}

impl Scope {
    pub fn org(p: &str) -> Scope {
        Scope::Org(p.to_string())
    }

    pub fn repo(k: &str) -> Scope {
        Scope::Repo(k.to_string())
    }

    /// `personal`, `org:<host>/<seg>[/…]` or `repo:<key>`. A bare host is too wide.
    pub fn parse(s: &str) -> Result<Scope, SkillError> {
        let bad = |why: &str| SkillError::Scope(format!("{s:?}: {why}"));
        if s == "personal" {
            return Ok(Scope::Personal);
        }
        let (kind, value) = s.split_once(':').ok_or_else(|| bad("expected personal, org:<prefix> or repo:<key>"))?;
        Scope::from_parts(kind, value).map_err(|e| match e {
            SkillError::Scope(why) => bad(&why),
            other => other,
        })
    }

    /// The scope of a `(kind, value)` pair (the manifest's and the index's form).
    pub fn from_parts(kind: &str, value: &str) -> Result<Scope, SkillError> {
        let bad = |why: &str| SkillError::Scope(why.to_string());
        match kind {
            "personal" if value.is_empty() => Ok(Scope::Personal),
            "personal" => Err(bad("personal takes no value")),
            "org" | "repo" => {
                key_segments(value, 2).ok_or_else(|| bad("needs a host and at least one path segment"))?;
                if value != value.to_lowercase() {
                    return Err(bad("keys are lowercase"));
                }
                Ok(if kind == "org" { Scope::Org(value.into()) } else { Scope::Repo(value.into()) })
            }
            _ => Err(bad("unknown kind")),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Scope::Personal => "personal",
            Scope::Org(_) => "org",
            Scope::Repo(_) => "repo",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Scope::Personal => "",
            Scope::Org(v) | Scope::Repo(v) => v,
        }
    }

    pub fn render(&self) -> String {
        match self {
            Scope::Personal => "personal".into(),
            other => format!("{}:{}", other.kind(), other.value()),
        }
    }

    /// Whether a task whose origin key is `key` gets this scope's skills.
    pub fn matches(&self, key: &str) -> bool {
        match self {
            Scope::Personal => true,
            Scope::Repo(k) => k == key,
            Scope::Org(p) => {
                let (p, k): (Vec<&str>, Vec<&str>) = (p.split('/').collect(), key.split('/').collect());
                p.len() >= 2 && p.len() <= k.len() && p.iter().zip(&k).all(|(a, b)| a == b)
            }
        }
    }

    /// Narrower first: repo, then org (longer prefix first), then personal.
    pub fn rank(&self) -> usize {
        match self {
            Scope::Repo(_) => 0,
            Scope::Org(p) => 1000usize.saturating_sub(p.split('/').count()),
            Scope::Personal => 2000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn normalizes_ssh_https_tokens_and_forge_aliases() {
        let a = aliases("Host azure-work\n  HostName ssh.dev.azure.com\nHost gh-x\n  HostName github.com\n");
        for (url, want) in [
            ("git@github.com-personal:Sirus-Alpha-Canis/Mirante.git", "github.com/sirus-alpha-canis/mirante"),
            ("https://x-access-token:ghp_abc@github.com/o/r.git/", "github.com/o/r"),
            ("ssh://git@github.com:22/o/r", "github.com/o/r"),
            ("git@gh-x:o/r.git", "github.com/o/r"),
            ("contoso@azure-work:v3/contoso/Proj Name/Web-App", "dev.azure.com/contoso/g2s site b2c/site-b2c"),
            ("https://contoso@dev.azure.com/contoso/Proj/_git/Repo", "dev.azure.com/contoso/proj/repo"),
            ("https://contoso.visualstudio.com/Proj/_git/Repo", "dev.azure.com/contoso/proj/repo"),
            ("contoso@vs-ssh.visualstudio.com:v3/contoso/Proj/Repo", "dev.azure.com/contoso/proj/repo"),
            ("https://contoso.visualstudio.com/DefaultCollection/Proj/_git/Repo", "dev.azure.com/contoso/proj/repo"),
            ("git://gitlab.com/g/sub/r.git", "gitlab.com/g/sub/r"),
            ("git@bitbucket.org-work:team/r.git", "bitbucket.org/team/r"),
        ] {
            assert_eq!(normalize(url, &a).unwrap().key(), want, "{url}");
        }
    }

    #[test]
    fn a_token_never_reaches_the_key_or_an_error() {
        let a = Aliases::default();
        let o = normalize("https://x:ghp_secret123@github.com/o/r", &a).unwrap();
        assert!(!o.key().contains("ghp_"));
        let e = normalize("https://x:ghp_secret123@bad host/o/r", &a).unwrap_err();
        assert!(!e.to_string().contains("ghp_"), "{e}");
        // An alias is ssh-only: an https host is never remapped by ~/.ssh/config.
        let a = aliases("Host github.com\n HostName evil.example\n");
        assert_eq!(normalize("https://github.com/o/r", &a).unwrap().key(), "github.com/o/r");
    }

    #[test]
    fn refuses_traversal_controls_and_empty() {
        for u in [
            "git@h:../x",
            "https://h/a/%2e%2e/b",
            "git@h:a\u{1b}b",
            "",
            "https://h/",
            "https://h/a/%2Fb",
            "https://h/a/%zz",
            "file:///srv/r.git",
            "/srv/r.git",
            "./r",
            "ftp://h/o/r",
            "https://h:x/o/r",
        ] {
            assert!(normalize(u, &Aliases::default()).is_err(), "{u}");
        }
    }

    #[test]
    fn tenant_is_host_plus_first_segment() {
        assert_eq!(tenant("dev.azure.com/contoso/p/r"), "dev.azure.com/contoso");
        assert_eq!(tenant("github.com/acme/api"), "github.com/acme");
        assert_eq!(tenant("local/0123456789abcdef"), "local/0123456789abcdef");
    }

    #[test]
    fn scope_matching_is_segment_prefix() {
        let k = "github.com/acme/api";
        assert!(Scope::org("github.com/acme").matches(k) && !Scope::org("github.com/ac").matches(k));
        assert!(
            Scope::repo(k).matches(k) && !Scope::repo("github.com/acme/api2").matches(k) && Scope::Personal.matches(k)
        );
        assert!(Scope::parse("org:github.com").is_err()); // host alone is too wide
        assert!(!Scope::org("github.com/acme/api/x").matches(k));
        for s in ["personal", "org:github.com/acme", "repo:github.com/acme/api"] {
            assert_eq!(Scope::parse(s).unwrap().render(), s);
        }
        for bad in ["org:", "repo:github.com", "org:github.com//x", "team:x/y", "org:GitHub.com/acme", "personal:x"] {
            assert!(Scope::parse(bad).is_err(), "{bad}");
        }
        assert!(Scope::repo(k).rank() < Scope::org("github.com/acme").rank());
        assert!(Scope::org("github.com/acme/api").rank() < Scope::org("github.com/acme").rank());
        assert!(Scope::org("github.com/acme").rank() < Scope::Personal.rank());
    }

    #[test]
    fn ssh_config_parser_takes_literal_hosts_only() {
        let a = aliases(
            "# comment\n\
             Host *.x wild?\n  HostName nope.example\n\
             Host Work other\n  hostname=GitHub.com\n  HostName second.example\n\
             Match exec \"true\"\n  HostName matched.example\n\
             Include ~/.ssh/more\n\
             Host !neg yes\n  HostName negated.example\n\
             HOST tok\n  HostName %h.example\n\
             Host spaced\n  HostName   gitlab.com  \n",
        );
        assert_eq!(a.get("work"), Some("github.com"));
        assert_eq!(a.get("other"), Some("github.com"), "first HostName wins");
        assert_eq!(a.get("*.x"), None);
        assert_eq!(a.get("wild?"), None);
        assert_eq!(a.get("yes"), None, "a line with a negation is skipped");
        assert_eq!(a.get("tok"), None, "%-tokens are not resolved");
        assert_eq!(a.get("spaced"), Some("gitlab.com"));
        assert_eq!(a.0.len(), 3, "{a:?}");
    }

    #[test]
    fn read_aliases_ignores_a_symlinked_config() {
        let home = TempDir::new("ssh-home");
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let elsewhere = home.path().join("elsewhere");
        std::fs::write(&elsewhere, "Host a\n HostName github.com\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.path().join(".ssh/config")).unwrap();
        assert_eq!(read_aliases(home.path()), Aliases::default());
        std::fs::remove_file(home.path().join(".ssh/config")).unwrap();
        std::fs::copy(&elsewhere, home.path().join(".ssh/config")).unwrap();
        assert_eq!(read_aliases(home.path()).get("a"), Some("github.com"));
        assert_eq!(read_aliases(&home.path().join("missing")), Aliases::default());
    }

    #[test]
    fn local_repos_get_a_path_key() {
        assert!(local_key(Path::new("/r")).starts_with("local/"));
        assert_eq!(local_key(Path::new("/r")).len(), "local/".len() + 16);
        assert_ne!(local_key(Path::new("/r")), local_key(Path::new("/s")));
    }

    #[test]
    fn origin_of_reads_the_raw_url_without_insteadof() {
        let r = TempDir::new("origin");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").arg("-C").arg(r.path()).args(args).status().unwrap().success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        assert_eq!(origin_of(r.path(), &Aliases::default()), local_key(r.path()));
        git(&["remote", "add", "origin", "https://tok@github.com/Acme/API.git"]);
        // An insteadOf planted in the repo config does not move the task to another tenant.
        git(&["config", "url.https://github.com/evil/.insteadOf", "https://tok@github.com/Acme/"]);
        assert_eq!(origin_of(r.path(), &Aliases::default()), "github.com/acme/api");
        git(&["remote", "set-url", "origin", "/srv/local.git"]);
        assert_eq!(origin_of(r.path(), &Aliases::default()), local_key(r.path()));
    }
}
