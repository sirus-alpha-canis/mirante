//! Reading one file of an agent's worktree for the person (the cockpit's side pane).
//!
//! Paths come from a client, so nothing here trusts them: a path is relative, made of plain
//! names (no `.`, `..`, empty level, backslash or NUL), never names `.git` at any depth, and
//! is opened one level at a time through a [`SafeDir`] (`O_NOFOLLOW` at every level), so a
//! symlink anywhere on the way, or as the file itself, is refused rather than followed. Only
//! a regular file with a single link is read, at most [`Caps::read_max`] bytes.
//!
//! What was read is sniffed: an image (PNG, JPEG, GIF or WebP by its magic bytes, never
//! SVG), text (valid UTF-8 with no NUL in its first 8 KiB, shown up to [`Caps::text_max`]),
//! or binary (anything else).
use std::path::Path;

use crate::safedir::{SafeDir, SafeError};

/// How much is read and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// A larger file is not read at all ([`Kind::TooLarge`]).
    pub read_max: usize,
    /// Text past this is cut (on a character boundary) and marked truncated.
    pub text_max: usize,
    /// A larger image is [`Kind::TooLarge`].
    pub image_max: usize,
}

pub const CAPS: Caps = Caps { read_max: 2 << 20, text_max: 1 << 20, image_max: 1 << 20 };

/// The longest path accepted, and the most levels in it.
const PATH_MAX: usize = 4096;
const LEVELS_MAX: usize = 64;
/// How much of the start is searched for a NUL byte.
const SNIFF: usize = 8 << 10;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    /// The path is not a plain relative path (or names `.git`).
    #[error("{0}")]
    Invalid(String),
    /// A level or the file itself is a symlink.
    #[error("{0} is a symlink: not followed")]
    Symlink(String),
    /// Not a regular file (a directory, a FIFO, a device, a socket) or a hard link.
    #[error("{0}")]
    NotFile(String),
    #[error("{0} does not exist")]
    Missing(String),
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, FileError>;

/// What a file is, as far as showing it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Text { text: String, truncated: bool, lang: Option<&'static str> },
    Binary,
    Image { mime: &'static str, bytes: Vec<u8> },
    TooLarge,
}

/// One file read: its size on disk and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    pub size: u64,
    pub kind: Kind,
}

/// The levels of a client's relative path, checked (see the module docs).
pub fn check_rel(path: &str) -> Result<Vec<&str>> {
    let bad = |why: &str| Err(FileError::Invalid(format!("{path:?}: {why}")));
    if path.is_empty() {
        return bad("empty path");
    }
    if path.len() > PATH_MAX {
        return bad("path too long");
    }
    if path.starts_with('/') {
        return bad("not a relative path");
    }
    if path.contains('\0') || path.contains('\\') {
        return bad("contains NUL or a backslash");
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() > LEVELS_MAX {
        return bad("too many levels");
    }
    for s in &segs {
        if s.is_empty() || *s == "." || *s == ".." {
            return bad("only plain names, no empty level, '.' or '..'");
        }
        // Case-insensitive: ".GIT" is the same folder on macOS's default file system.
        if s.eq_ignore_ascii_case(".git") {
            return bad("git's own files are not shown");
        }
    }
    Ok(segs)
}

fn from_safe(e: SafeError, shown: &str) -> FileError {
    match e {
        SafeError::Unsafe(m) if m.contains("symlink") => FileError::Symlink(shown.to_string()),
        SafeError::Unsafe(m) => FileError::NotFile(m),
        SafeError::Missing(_) => FileError::Missing(shown.to_string()),
        other => FileError::Io(other.to_string()),
    }
}

fn is_symlink(mode: u32) -> bool {
    mode & 0o170000 == 0o120000
}

/// Reads `root/segs…` (levels from [`check_rel`]) by descriptor walk; see the module docs.
pub fn read_in(root: &Path, segs: &[&str], caps: Caps) -> Result<FileRead> {
    let Some((name, dirs)) = segs.split_last() else {
        return Err(FileError::Invalid("empty path".into()));
    };
    let shown = segs.join("/");
    let mut dir =
        SafeDir::open(root, &[]).map_err(|e| from_safe(e, &shown))?.ok_or_else(|| FileError::Missing(shown.clone()))?;
    for (i, d) in dirs.iter().enumerate() {
        let here = segs[..=i].join("/");
        match dir.stat(d).map_err(|e| from_safe(e, &here))? {
            None => return Err(FileError::Missing(shown)),
            Some(e) if is_symlink(e.mode) => return Err(FileError::Symlink(here)),
            Some(e) if !e.is_dir() => return Err(FileError::Missing(shown)),
            Some(_) => {}
        }
        dir = dir.open_dir(d).map_err(|e| from_safe(e, &here))?.ok_or_else(|| FileError::Missing(shown.clone()))?;
    }
    let entry = dir.stat(name).map_err(|e| from_safe(e, &shown))?.ok_or_else(|| FileError::Missing(shown.clone()))?;
    if is_symlink(entry.mode) {
        return Err(FileError::Symlink(shown));
    }
    if !entry.is_file() {
        return Err(FileError::NotFile(format!("{shown} is not a regular file")));
    }
    if entry.size > caps.read_max as u64 {
        return Ok(FileRead { size: entry.size, kind: Kind::TooLarge });
    }
    let bytes = match dir.read_bytes(name, caps.read_max) {
        Ok(b) => b,
        Err(SafeError::TooLarge(..)) => return Ok(FileRead { size: entry.size, kind: Kind::TooLarge }),
        Err(e) => return Err(from_safe(e, &shown)),
    };
    let size = bytes.len() as u64;
    Ok(FileRead { size, kind: sniff(bytes, name, caps) })
}

/// The image type of `b` by its magic bytes (never SVG, which is text that can run script).
pub fn image_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// What `bytes` (the file `name`) is: image, text or binary; see the module docs.
pub fn sniff(bytes: Vec<u8>, name: &str, caps: Caps) -> Kind {
    if let Some(mime) = image_mime(&bytes) {
        if bytes.len() > caps.image_max {
            return Kind::TooLarge;
        }
        return Kind::Image { mime, bytes };
    }
    if bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return Kind::Binary;
    }
    let Ok(mut text) = String::from_utf8(bytes) else {
        return Kind::Binary;
    };
    let truncated = text.len() > caps.text_max;
    if truncated {
        let mut cut = caps.text_max;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Kind::Text { text, truncated, lang: lang_of(name) }
}

/// The highlighter's language for a file name: a closed table, `None` when unknown.
pub fn lang_of(name: &str) -> Option<&'static str> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let by_name = match base {
        "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
        "Dockerfile" => Some("dockerfile"),
        "Cargo.lock" => Some("toml"),
        ".bashrc" | ".zshrc" | ".profile" => Some("sh"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let (_, ext) = base.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" | "zsh" => "sh",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "xml" | "svg" => "xml",
        "sql" => "sql",
        _ => return None,
    })
}

/// Standard base64 (with padding), for an image sent to the cockpit as a `data:` URL.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn relative_paths_only_no_dots_no_git() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            ".git/config",
            "src/.git/HEAD",
            "a\u{0}b",
            "a\\b",
            "./a",
            "a/",
            ".GIT/x",
        ] {
            assert!(check_rel(bad).is_err(), "{bad:?}");
        }
        assert_eq!(check_rel("docs/README.md").unwrap(), vec!["docs", "README.md"]);
        assert_eq!(check_rel(".github/x.yml").unwrap(), vec![".github", "x.yml"]);
        assert!(check_rel("a/".repeat(70).trim_end_matches('/')).is_err());
    }

    fn read(root: &Path, p: &str) -> Result<FileRead> {
        read_in(root, &check_rel(p)?, CAPS)
    }

    #[test]
    fn symlinks_are_never_followed_at_any_level() {
        let d = TempDir::new("wtf-link");
        let outside = TempDir::new("wtf-out");
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::fs::create_dir_all(d.path().join("d")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), d.path().join("d/f")).unwrap();
        assert!(
            matches!(read(d.path(), "link/secret"), Err(FileError::Symlink(_))),
            "{:?}",
            read(d.path(), "link/secret")
        );
        assert!(matches!(read(d.path(), "d/f"), Err(FileError::Symlink(_))));
        assert!(matches!(read(d.path(), "nope.txt"), Err(FileError::Missing(_))));
    }

    #[test]
    fn classifies_text_binary_image_and_too_large() {
        let d = TempDir::new("wtf-kind");
        let w = |n: &str, b: &[u8]| std::fs::write(d.path().join(n), b).unwrap();
        w("main.rs", b"fn main(){}");
        w("bin", b"\x00\x01");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend([0u8; 16]);
        w("p.png", &png);
        w("big.txt", &vec![b'a'; 3 << 20]);
        w("bad.txt", &[0xC3, 0x28]);
        w("x.svg", b"<svg onload=alert(1)></svg>");
        let k = |n: &str| read(d.path(), n).unwrap().kind;
        assert_eq!(k("main.rs"), Kind::Text { text: "fn main(){}".into(), truncated: false, lang: Some("rust") });
        assert_eq!(k("bin"), Kind::Binary);
        assert!(matches!(k("p.png"), Kind::Image { mime: "image/png", .. }));
        let big = read(d.path(), "big.txt").unwrap();
        assert_eq!((big.size, big.kind), (3 << 20, Kind::TooLarge));
        assert_eq!(k("bad.txt"), Kind::Binary);
        assert!(matches!(k("x.svg"), Kind::Text { .. }));
    }

    #[test]
    fn text_is_capped_at_one_mib_and_says_truncated() {
        let d = TempDir::new("wtf-cap");
        // A two-byte character straddles the cap: the cut lands on a boundary.
        let mut s = "a".repeat((1 << 20) - 1);
        s.push('é');
        s.push_str("tail");
        std::fs::write(d.path().join("t.md"), &s).unwrap();
        let Kind::Text { text, truncated, lang } = read(d.path(), "t.md").unwrap().kind else { panic!("text") };
        assert!(truncated);
        assert_eq!(text.len(), (1 << 20) - 1);
        assert_eq!(lang, Some("markdown"));
    }

    #[test]
    fn fifo_device_and_dirs_are_refused() {
        let d = TempDir::new("wtf-special");
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let fifo = std::ffi::CString::new(d.path().join("pipe").to_str().unwrap()).unwrap();
        // SAFETY: valid path string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(matches!(read(d.path(), "sub"), Err(FileError::NotFile(_))));
        assert!(matches!(read(d.path(), "pipe"), Err(FileError::NotFile(_))));
        assert!(matches!(read(Path::new("/dev"), "null"), Err(FileError::NotFile(_))));
        // A hard link to a file elsewhere is refused (it could be anyone's file).
        std::fs::write(d.path().join("orig"), "x").unwrap();
        std::fs::hard_link(d.path().join("orig"), d.path().join("hl")).unwrap();
        assert!(matches!(read(d.path(), "hl"), Err(FileError::NotFile(_))));
    }

    #[test]
    fn lang_from_extension_is_a_closed_table() {
        assert_eq!(lang_of("a.tsx"), Some("tsx"));
        assert_eq!(lang_of("Makefile"), Some("make"));
        assert_eq!(lang_of("src/lib.RS"), Some("rust"));
        assert_eq!(lang_of("x.zzz"), None);
        assert_eq!(lang_of("README"), None);
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFB, 0xFF]), "+/8=");
    }
}
