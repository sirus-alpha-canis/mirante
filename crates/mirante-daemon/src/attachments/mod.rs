//! What a person attaches to a message or a goal: images and files stored in
//! `<home>/sessions/<task>/attachments/` (0700, files 0600, written through
//! [`SafeDir`]), so the agent reads them by path.
//!
//! - Names: the basename only, `[A-Za-z0-9._-]` (anything else becomes `_`), at most
//!   [`MAX_NAME`] characters, stored as `<n>-<name>` so two never collide.
//! - Kinds: an image is PNG, JPEG, GIF or WebP by its magic bytes (never its extension), at
//!   most [`MAX_IMAGE`] bytes; any other file at most [`MAX_FILE`]. A task holds at most
//!   [`MAX_TASK_BYTES`] of them.
//! - Redaction ([`redact`]): a HAR loses its credentials; any other text (UTF-8, UTF-16 by
//!   its mark or its zero bytes, other text-looking bytes read lossily) goes through
//!   `redact_counted`; binary files and images are stored as they are, and a `.har` that is
//!   not text or not JSON is refused. At most [`MAX_PREPARING`] are prepared at once. The
//!   original never lands in Mirante's home.
//! - Import ([`read_source`]): the source is opened with `O_NOFOLLOW` (its last component
//!   is never a symlink), `O_NOCTTY` and `O_NONBLOCK`, and must be a regular file with a
//!   single link that is not inside Mirante's home (a task's worktree excepted), not in a
//!   credential folder of the person's home and not a key file (checked on the given path
//!   and on the opened descriptor's); at most limit + 1 bytes are read.
pub mod redact;

use std::borrow::Cow;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use mirante_core::safedir::SafeDir;
use mirante_ipc::types::AttachmentKind;

/// The folder under a task's session dir.
pub const DIR: &str = "attachments";
/// Largest image (bytes).
pub const MAX_IMAGE: usize = 10 << 20;
/// Largest other file (bytes).
pub const MAX_FILE: usize = 25 << 20;
/// Most attachments one message (or goal) carries.
pub const MAX_PER_MESSAGE: usize = 20;
/// Most bytes a task's attachments take together.
pub const MAX_TASK_BYTES: u64 = 200 << 20;
/// Longest stored name, before its `<n>-` prefix.
pub const MAX_NAME: usize = 80;
/// Most files a task's attachments folder is listed with.
const MAX_ENTRIES: usize = 10_000;

const DIR_MODE: libc::mode_t = 0o700;
const FILE_MODE: libc::c_uint = 0o600;

/// `name`'s basename with every character outside `[A-Za-z0-9._-]` made `_`, at most
/// [`MAX_NAME`] characters (the extension kept when it is cut); `attachment` when nothing
/// is left.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String =
        base.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim_matches(|c| c == '_' || c == ' ').to_string();
    if clean.chars().all(|c| c == '.' || c == '_') {
        return "attachment".to_string();
    }
    if clean.len() <= MAX_NAME {
        return clean;
    }
    match clean.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 10 => {
            format!("{}.{ext}", &stem[..MAX_NAME - ext.len() - 1])
        }
        _ => clean[..MAX_NAME].to_string(),
    }
}

/// Whether `bytes` start like a PNG, JPEG, GIF or WebP image.
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
}

/// The bytes as text Mirante redacts, or `None` (binary: stored as it is). Not a PDF or a
/// zip. UTF-16 by its byte-order mark (or, without one, by its zero bytes) is transcoded;
/// a UTF-8 mark is dropped; UTF-8 is read as it is. Other bytes without NUL that look like
/// text (few control characters: Latin-1, a log with a stray byte) are read lossily, so
/// what was valid comes through and is redacted. `har`: a `.har` is text or refused.
fn as_text<'a>(bytes: &'a [u8], har: bool) -> Result<Option<Cow<'a, str>>, String> {
    let binary = || if har { Err("it is not text, so it cannot be a HAR".to_string()) } else { Ok(None) };
    if bytes.starts_with(b"%PDF-") || bytes.starts_with(b"PK\x03\x04") {
        return binary();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Ok(Some(Cow::Owned(utf16(rest, false))));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Ok(Some(Cow::Owned(utf16(rest, true))));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if bytes.contains(&0) {
        return match utf16_without_mark(bytes) {
            Some(be) => Ok(Some(Cow::Owned(utf16(bytes, be)))),
            None => binary(),
        };
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(Some(Cow::Borrowed(s)));
    }
    let control = bytes.iter().filter(|b| **b < 0x20 && !b"\t\n\r\x0c\x1b".contains(b)).count();
    if har || control <= bytes.len() / 100 {
        return Ok(Some(String::from_utf8_lossy(bytes)));
    }
    binary()
}

/// UTF-16 code units of `bytes` (`be`: big-endian) as a string, unpaired ones replaced.
fn utf16(bytes: &[u8], be: bool) -> String {
    let units =
        bytes.as_chunks::<2>().0.iter().map(|c| if be { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) });
    char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// Whether `bytes` (they hold NUL) look like UTF-16 text without a mark: of their first
/// 4 KiB, most odd bytes are zero and almost no even ones (little-endian, ASCII-heavy
/// text), or the reverse (big-endian). `Some(big_endian)`.
fn utf16_without_mark(bytes: &[u8]) -> Option<bool> {
    let head = &bytes[..bytes.len().min(4096) & !1];
    let pairs = head.len() / 2;
    if pairs == 0 {
        return None;
    }
    let even = head.iter().step_by(2).filter(|b| **b == 0).count();
    let odd = head.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    let most = |z: usize| z * 10 >= pairs * 7;
    let few = |z: usize| z * 20 <= pairs;
    match (most(odd) && few(even), most(even) && few(odd)) {
        (true, _) => Some(false),
        (_, true) => Some(true),
        _ => None,
    }
}

/// How many attachments are prepared at once: a HAR is parsed into a tree several times
/// its size, so two at most (a third waits).
const MAX_PREPARING: usize = 2;

static PREPARING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// A place among the [`MAX_PREPARING`]; freed when dropped.
struct Permit;

impl Permit {
    fn take() -> Permit {
        let (m, cv) = &PREPARING;
        let mut n = m.lock().unwrap_or_else(|p| p.into_inner());
        while *n >= MAX_PREPARING {
            n = cv.wait(n).unwrap_or_else(|p| p.into_inner());
        }
        *n += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let (m, cv) = &PREPARING;
        let mut n = m.lock().unwrap_or_else(|p| p.into_inner());
        *n -= 1;
        cv.notify_one();
    }
}

/// What is stored for an attachment named `name` with `raw` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub kind: AttachmentKind,
    pub redacted: u32,
}

/// Checks the size limits and redacts (see the module docs). `Err`: why it is refused.
pub fn prepare(name: &str, raw: Vec<u8>) -> Result<Prepared, String> {
    if raw.is_empty() {
        return Err(format!("{name} is empty"));
    }
    if is_image(&raw) {
        if raw.len() > MAX_IMAGE {
            return Err(format!(
                "{name} is {}; an image may be at most {}",
                size(raw.len() as u64),
                size(MAX_IMAGE as u64)
            ));
        }
        return Ok(Prepared { bytes: raw, kind: AttachmentKind::Image, redacted: 0 });
    }
    if raw.len() > MAX_FILE {
        return Err(format!("{name} is {}; a file may be at most {}", size(raw.len() as u64), size(MAX_FILE as u64)));
    }
    let har = name.to_ascii_lowercase().ends_with(".har");
    let _permit = Permit::take();
    let Some(text) = as_text(&raw, har).map_err(|e| format!("{name}: {e}"))? else {
        return Ok(Prepared { bytes: raw, kind: AttachmentKind::File, redacted: 0 });
    };
    let (out, redacted) = redact::redact_text(&text, har).map_err(|e| format!("{name}: {e}"))?;
    Ok(Prepared { bytes: out.into_bytes(), kind: AttachmentKind::File, redacted })
}

/// `n` bytes for a person: `512 B`, `3.4 KB`, `12.0 MB`.
pub fn size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else {
        format!("{:.1} MB", f / (K * K))
    }
}

/// Standard base64 (padding optional, no whitespace) of at most `max` decoded bytes.
pub fn decode_b64(s: &str, max: usize) -> Result<Vec<u8>, String> {
    let s = s.trim_end_matches('=');
    if s.len() / 4 * 3 > max + 3 {
        return Err(format!("the attachment is larger than {}", size(max as u64)));
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let b = s.as_bytes();
    if b.len() % 4 == 1 {
        return Err("the attachment is not valid base64".into());
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3 + 3);
    for chunk in b.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = val(*c).ok_or("the attachment is not valid base64")?;
            acc |= v << (18 - 6 * i);
        }
        let bytes = [(acc >> 16) as u8, (acc >> 8) as u8, acc as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    if out.len() > max {
        return Err(format!("the attachment is larger than {}", size(max as u64)));
    }
    Ok(out)
}

/// The path the descriptor `f` is open on.
#[cfg(target_os = "macos")]
fn fd_path(f: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes (NUL included) into `buf`.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..end])))
}

#[cfg(not(target_os = "macos"))]
fn fd_path(f: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd()))
}

/// Folders and files under the person's home whose contents are credentials: never
/// attached, whoever asks (compared without case: macOS folders are case-insensitive).
const PRIVATE: [&str; 22] = [
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".config/gh",
    ".kube",
    ".docker/config.json",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".password-store",
    "Library/Keychains",
    "Library/Cookies",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Arc",
    ".config/google-chrome",
    ".mozilla",
];

/// File names that are private keys or certificates (compared without case).
fn private_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".pem", ".key", ".p12", ".pfx"].iter().any(|e| n.ends_with(e))
        || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"].iter().any(|k| n.starts_with(k))
}

/// Whether `p` is inside (or is) `base`, without case.
fn under(p: &Path, base: &Path) -> bool {
    let (p, base) = (p.to_string_lossy().to_lowercase(), base.to_string_lossy().to_lowercase());
    let base = base.trim_end_matches('/');
    p == base || p.starts_with(&format!("{base}/"))
}

/// `base` as given and canonical (when it resolves).
fn both(base: &Path) -> Vec<PathBuf> {
    let mut v = vec![base.to_path_buf()];
    if let Ok(c) = std::fs::canonicalize(base) {
        if c != base {
            v.push(c);
        }
    }
    v
}

/// Why `path` (given or opened) may not be attached, if it may not: inside Mirante's home
/// (except a task's worktree, `<home>/worktrees/`), a credential folder or file of the
/// person's home ([`PRIVATE`]), or a key file by its name.
fn refusal(path: &Path, home: &Path, user_home: Option<&Path>) -> Option<String> {
    let shown = path.display();
    if path.file_name().is_some_and(|n| private_name(&n.to_string_lossy())) {
        return Some(format!("{shown} looks like a private key or certificate: it is never attached"));
    }
    let homes = both(home);
    let in_home = homes.iter().any(|h| under(path, h));
    let in_worktree = homes.iter().any(|h| under(path, &h.join("worktrees")));
    if in_home && !in_worktree {
        return Some(format!("{shown} is inside Mirante's own folder ({}): it is never attached", home.display()));
    }
    for uh in user_home.map(both).unwrap_or_default() {
        if let Some(dir) = PRIVATE.iter().find(|d| under(path, &uh.join(d))) {
            return Some(format!("{shown} is inside ~/{dir}, which holds credentials: it is never attached"));
        }
    }
    None
}

/// Reads the file a person dropped or picked: `(its name, its bytes)`, at most `max` bytes.
/// Refused: a relative path, a symlink (last component), anything but a regular file with
/// a single link, anything inside `home` (Mirante's own files) but a task's worktree, and
/// credential folders and key files of the person's home `user_home` (see [`refusal`]),
/// by the given path and by the opened descriptor's. Opened with `O_NOCTTY | O_NONBLOCK`
/// (a terminal never becomes the daemon's, a FIFO never blocks the open), checked with
/// `fstat` before a byte is read.
pub fn read_source(path: &str, home: &Path, user_home: Option<&Path>, max: usize) -> Result<(String, Vec<u8>), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or(format!("{path} names no file"))?;
    if let Some(why) = refusal(p, home, user_home) {
        return Err(why);
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(p)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => format!("{path} is a symlink: attach the file it points to"),
            _ => format!("{path}: {e}"),
        })?;
    let meta = f.metadata().map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    if meta.nlink() != 1 {
        return Err(format!("{path} has other hard links: attach a copy of it"));
    }
    let real = fd_path(&f).map_err(|e| format!("{path}: {e}"))?;
    if let Some(why) = refusal(&real, home, user_home) {
        return Err(why);
    }
    clear_nonblock(&f).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > max as u64 {
        return Err(format!("{name} is {}; at most {} is attached", size(meta.len()), size(max as u64)));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.take(max as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() > max {
        return Err(format!("{name} is larger than {}", size(max as u64)));
    }
    Ok((name, bytes))
}

/// Clears `O_NONBLOCK` on `f` (a regular file by now: reads block as usual).
fn clear_nonblock(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = f.as_raw_fd();
    // SAFETY: F_GETFL / F_SETFL on a descriptor this function borrows open.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// One stored attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The stored file's name (`<n>-<name>`).
    pub file: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// A task's attachments folder, made when missing (`<home>/sessions/<task>/attachments`).
pub fn task_dir(home: &Path, task_id: &str) -> Result<SafeDir, String> {
    SafeDir::create(home, &["sessions", task_id, DIR], DIR_MODE).map_err(|e| e.to_string())
}

/// Stores `p` as `<n>-<name>` (n one more than the highest stored), refused when the
/// task's attachments would pass [`MAX_TASK_BYTES`].
pub fn store(home: &Path, task_id: &str, name: &str, p: &Prepared) -> Result<Stored, String> {
    let dir = task_dir(home, task_id)?;
    let entries = dir.entries(MAX_ENTRIES).map_err(|e| e.to_string())?;
    let used: u64 = entries.iter().filter(|e| e.is_file()).map(|e| e.size).sum();
    if used + p.bytes.len() as u64 > MAX_TASK_BYTES {
        return Err(format!(
            "this task's attachments would take {}; at most {} are kept",
            size(used + p.bytes.len() as u64),
            size(MAX_TASK_BYTES)
        ));
    }
    let mut n = entries.iter().filter_map(|e| e.name.split_once('-')?.0.parse::<u64>().ok()).max().unwrap_or(0) + 1;
    for _ in 0..8 {
        let file = format!("{n}-{name}");
        match dir.write_new(&file, &p.bytes, FILE_MODE) {
            Ok(()) => {
                dir.sync().map_err(|e| e.to_string())?;
                let path = dir.path().join(&file);
                return Ok(Stored { file, path, bytes: p.bytes.len() as u64 });
            }
            Err(mirante_core::safedir::SafeError::Exists(_)) => n += 1,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("could not name the attachment".into())
}

/// Deletes the stored file `file` of the task (nothing when it is gone).
pub fn unlink(home: &Path, task_id: &str, file: &str) -> Result<(), String> {
    let Some(dir) = SafeDir::open(home, &["sessions", task_id, DIR]).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    match dir.stat(file).map_err(|e| e.to_string())? {
        Some(_) => dir.unlink(file).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn names_keep_only_the_basename_and_safe_characters() {
        assert_eq!(sanitize("/Users/x/Desktop/Screen Shot 2026.png"), "Screen_Shot_2026.png");
        assert_eq!(sanitize("..\\..\\evil.har"), "evil.har");
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("café ünïcode.txt"), "caf___n_code.txt");
        assert_eq!(sanitize(".."), "attachment");
        assert_eq!(sanitize(""), "attachment");
        assert_eq!(sanitize("a/"), "attachment");
        assert_eq!(sanitize(".env"), ".env");
        let long = format!("{}.log", "x".repeat(200));
        let s = sanitize(&long);
        assert_eq!(s.len(), MAX_NAME);
        assert!(s.ends_with(".log"), "{s}");
        assert_eq!(sanitize(&"y".repeat(200)).len(), MAX_NAME);
    }

    #[test]
    fn images_are_known_by_their_magic_bytes_not_their_extension() {
        assert!(is_image(PNG));
        assert!(is_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_image(b"GIF89a.."));
        assert!(is_image(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_image(b"RIFF\0\0\0\0WAVE"));
        assert!(!is_image(b"hello"));
        let p = prepare("notes.png", b"plain text".to_vec()).unwrap();
        assert_eq!(p.kind, AttachmentKind::File);
        let p = prepare("shot.txt", PNG.to_vec()).unwrap();
        assert_eq!((p.kind, p.bytes.as_slice()), (AttachmentKind::Image, PNG));
    }

    #[test]
    fn limits_refuse_big_images_big_files_and_empty_ones() {
        let mut img = PNG.to_vec();
        img.resize(MAX_IMAGE + 1, 0);
        assert!(prepare("a.png", img).unwrap_err().contains("an image may be at most 10.0 MB"));
        let mut ok = PNG.to_vec();
        ok.resize(MAX_IMAGE, 0);
        assert!(prepare("a.png", ok).is_ok());
        assert!(prepare("big.bin", vec![1u8; MAX_FILE + 1]).unwrap_err().contains("a file may be at most 25.0 MB"));
        assert!(prepare("e.txt", Vec::new()).unwrap_err().contains("empty"));
    }

    #[test]
    fn text_is_redacted_and_binary_is_kept() {
        let token = format!("{}{}_{}", "gh", "p", "Q1w2E3r4T5y6U7i8O9p0A1s2D3f4G5h6J7k8");
        let p = prepare("app.log", format!("auth ok {token}\n").into_bytes()).unwrap();
        assert!(!String::from_utf8(p.bytes).unwrap().contains(&token));
        assert!(p.redacted >= 1);
        let pdf = format!("%PDF-1.4 {token}").into_bytes();
        let p = prepare("doc.pdf", pdf.clone()).unwrap();
        assert_eq!((p.bytes, p.redacted), (pdf, 0));
        let bin = vec![0u8, 1, 2, 3];
        assert_eq!(prepare("x.bin", bin.clone()).unwrap().bytes, bin);
    }

    fn utf16_bytes(s: &str, be: bool, mark: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if mark {
            out.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
        }
        for u in s.encode_utf16() {
            out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        out
    }

    #[test]
    fn utf16_latin1_and_bom_text_is_redacted_never_stored_raw() {
        let token = format!("{}{}_{}", "gh", "p", "A9s8D7f6G5h4J3k2L1q0W9e8R7t6Y5u4I3o2");
        let line = format!("Authorization: Bearer {token}\n");
        for (be, mark) in [(false, true), (true, true), (false, false), (true, false)] {
            let p = prepare("app.log", utf16_bytes(&line, be, mark)).unwrap();
            let out = String::from_utf8(p.bytes).unwrap();
            assert!(!out.contains(&token) && out.starts_with("Authorization:"), "{be} {mark}: {out}");
            assert!(p.redacted >= 1);
        }
        // Latin-1 (invalid UTF-8, no NUL): read lossily, then redacted.
        let mut latin1 = b"caf\xe9 ".to_vec();
        latin1.extend_from_slice(line.as_bytes());
        let p = prepare("app.log", latin1).unwrap();
        assert!(!String::from_utf8(p.bytes).unwrap().contains(&token));
        // A UTF-8 mark is dropped.
        let p = prepare("a.txt", format!("\u{feff}{line}").into_bytes()).unwrap();
        let out = String::from_utf8(p.bytes).unwrap();
        assert!(!out.contains(&token) && out.starts_with("Authorization:"), "{out}");
        // A UTF-16 HAR is read as a HAR.
        let har = format!(
            "{{\"log\":{{\"entries\":[{{\"request\":{{\"headers\":[{{\"name\":\"Cookie\",\"value\":\"sid={token}\"}}]}}}}]}}}}"
        );
        let p = prepare("trace.har", utf16_bytes(&har, false, true)).unwrap();
        let out = String::from_utf8(p.bytes).unwrap();
        assert!(!out.contains(&token) && out.contains(redact::MARK), "{out}");
        // A .har that is binary, or text that is not JSON, is refused; other binary is kept.
        assert!(prepare("trace.har", vec![0u8, 1, 2, 0, 0, 7]).unwrap_err().contains("not text"));
        assert!(prepare("trace.har", b"\x01\x02\x03 not json".to_vec()).unwrap_err().contains("not valid JSON"));
        assert_eq!(prepare("x.bin", vec![0u8, 1, 2, 0, 0, 7]).unwrap().bytes, [0u8, 1, 2, 0, 0, 7]);
        let mostly_control: Vec<u8> = (0..200u8).map(|i| if i % 3 == 0 { 0x01 } else { 0xC3 }).collect();
        assert_eq!(prepare("x.bin", mostly_control.clone()).unwrap().bytes, mostly_control);
    }

    #[test]
    fn at_most_two_attachments_are_prepared_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let now = std::sync::Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (peak, now) = (peak.clone(), now.clone());
                std::thread::spawn(move || {
                    let _p = Permit::take();
                    peak.fetch_max(now.fetch_add(1, SeqCst) + 1, SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    now.fetch_sub(1, SeqCst);
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert!(peak.load(SeqCst) <= MAX_PREPARING);
    }

    #[test]
    fn base64_decodes_strictly_and_within_the_limit() {
        assert_eq!(decode_b64("aGVsbG8=", 100).unwrap(), b"hello");
        assert_eq!(decode_b64("aGVsbG8", 100).unwrap(), b"hello");
        assert_eq!(decode_b64("", 100).unwrap(), b"");
        assert_eq!(decode_b64("/+8=", 100).unwrap(), [0xFF, 0xEF]);
        assert!(decode_b64("aGV sbG8=", 100).is_err());
        assert!(decode_b64("a", 100).is_err());
        assert!(decode_b64("aGVsbG8=", 4).unwrap_err().contains("larger"));
        assert!(decode_b64(&"A".repeat(4000), 100).unwrap_err().contains("larger"));
    }

    #[test]
    fn stored_files_are_private_numbered_and_counted_against_the_task_total() {
        let d = TestDir::new();
        let home = d.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let p = prepare("a.txt", b"one".to_vec()).unwrap();
        let a = store(&home, "t1", "a.txt", &p).unwrap();
        let b = store(&home, "t1", "a.txt", &p).unwrap();
        assert_eq!((a.file.as_str(), b.file.as_str()), ("1-a.txt", "2-a.txt"));
        let canon = std::fs::canonicalize(&home).unwrap();
        assert_eq!(a.path, canon.join("sessions/t1/attachments/1-a.txt"));
        assert_eq!(std::fs::read(&a.path).unwrap(), b"one");
        assert_eq!(std::fs::metadata(&a.path).unwrap().permissions().mode() & 0o777, 0o600);
        let dir = home.join("sessions/t1/attachments");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // Near the task total: refused.
        let f = std::fs::File::create(dir.join("9-big.bin")).unwrap();
        f.set_len(MAX_TASK_BYTES - 8).unwrap();
        assert!(store(&home, "t1", "a.txt", &prepare("a.txt", b"12345".to_vec()).unwrap())
            .unwrap_err()
            .contains("at most 200.0 MB"));
        let c = store(&home, "t1", "c.txt", &prepare("c.txt", b"1".to_vec()).unwrap()).unwrap();
        assert_eq!(c.file, "10-c.txt");
        unlink(&home, "t1", &c.file).unwrap();
        assert!(!c.path.exists());
        unlink(&home, "t1", &c.file).unwrap();
    }

    #[test]
    fn a_symlinked_task_dir_is_never_written_through() {
        let d = TestDir::new();
        let home = d.path().join("home");
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir_all(home.join("sessions/t1")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join("sessions/t1/attachments")).unwrap();
        let p = prepare("a.txt", b"x".to_vec()).unwrap();
        assert!(store(&home, "t1", "a.txt", &p).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn import_reads_regular_files_only_never_symlinks_links_or_mirantes_home() {
        let d = TestDir::new();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        let src = d.path().join("drop");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("trace.har");
        std::fs::write(&file, b"{}").unwrap();
        let (name, bytes) = read_source(file.to_str().unwrap(), &home, None, 100).unwrap();
        assert_eq!((name.as_str(), bytes.as_slice()), ("trace.har", b"{}".as_slice()));
        // Too big: never more than limit + 1 bytes read.
        std::fs::write(src.join("big.log"), vec![b'x'; 101]).unwrap();
        assert!(read_source(src.join("big.log").to_str().unwrap(), &home, None, 100).unwrap_err().contains("at most"));
        // Relative, a symlink, a directory, a FIFO, a hard link.
        assert!(read_source("drop/trace.har", &home, None, 100).unwrap_err().contains("absolute"));
        std::os::unix::fs::symlink(&file, src.join("link.har")).unwrap();
        assert!(read_source(src.join("link.har").to_str().unwrap(), &home, None, 100).unwrap_err().contains("symlink"));
        assert!(read_source(src.to_str().unwrap(), &home, None, 100).unwrap_err().contains("not a regular file"));
        let fifo = src.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: plain mkfifo on a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_source(fifo.to_str().unwrap(), &home, None, 100).unwrap_err().contains("not a regular file"));
        std::fs::hard_link(&file, src.join("twin.har")).unwrap();
        assert!(read_source(file.to_str().unwrap(), &home, None, 100).unwrap_err().contains("hard links"));
        // Inside Mirante's home: refused, through a symlinked parent too.
        let db = home.join("mirante.sqlite");
        std::fs::write(&db, b"db").unwrap();
        assert!(read_source(db.to_str().unwrap(), &home, None, 100).unwrap_err().contains("Mirante's own folder"));
        std::os::unix::fs::symlink(&home, d.path().join("alias")).unwrap();
        let via = d.path().join("alias/mirante.sqlite");
        assert!(read_source(via.to_str().unwrap(), &home, None, 100).unwrap_err().contains("Mirante's own folder"));
    }

    #[test]
    fn a_task_s_worktree_is_attachable_but_not_through_a_link_into_the_rest_of_home() {
        let d = TestDir::new();
        let home = d.path().join("home");
        let wt = home.join("worktrees/app/feat-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("err.log"), b"boom").unwrap();
        let (name, bytes) = read_source(wt.join("err.log").to_str().unwrap(), &home, None, 100).unwrap();
        assert_eq!((name.as_str(), bytes.as_slice()), ("err.log", b"boom".as_slice()));
        // A folder in the worktree that links back into the home: refused by the real path.
        std::fs::write(home.join("settings.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(&home, wt.join("up")).unwrap();
        let via = wt.join("up/settings.json");
        assert!(read_source(via.to_str().unwrap(), &home, None, 100).unwrap_err().contains("Mirante's own folder"));
        // `worktrees` itself is no prefix match for a sibling.
        std::fs::create_dir_all(home.join("worktrees-old")).unwrap();
        std::fs::write(home.join("worktrees-old/a.txt"), b"a").unwrap();
        let sib = home.join("worktrees-old/a.txt");
        assert!(read_source(sib.to_str().unwrap(), &home, None, 100).unwrap_err().contains("Mirante's own folder"));
    }

    #[test]
    fn credential_folders_and_key_files_of_the_person_s_home_are_never_attached() {
        let d = TestDir::new();
        let home = d.path().join("mirante");
        let user = d.path().join("user");
        for (dir, file) in [
            (".ssh", "config"),
            (".aws", "credentials"),
            (".config/gcloud", "application_default_credentials.json"),
            (".kube", "config"),
            (".docker", "config.json"),
            ("Library/Keychains", "login.keychain-db"),
            ("Library/Cookies", "Cookies.binarycookies"),
            ("Library/Application Support/Google/Chrome/Default", "Cookies"),
            ("Library/Application Support/Firefox/Profiles/x", "cookies.sqlite"),
            ("Library/Application Support/Arc/User Data", "Login Data"),
        ] {
            std::fs::create_dir_all(user.join(dir)).unwrap();
            let f = user.join(dir).join(file);
            std::fs::write(&f, b"secret").unwrap();
            let e = read_source(f.to_str().unwrap(), &home, Some(&user), 100).unwrap_err();
            assert!(e.contains("holds credentials"), "{dir}/{file}: {e}");
        }
        // Without case (macOS), and through a symlinked parent (the real path is checked).
        let e = read_source(user.join(".SSH/config").to_str().unwrap(), &home, Some(&user), 100);
        assert!(e.unwrap_err().contains("never attached"));
        std::os::unix::fs::symlink(user.join(".ssh"), d.path().join("keys")).unwrap();
        let e = read_source(d.path().join("keys/config").to_str().unwrap(), &home, Some(&user), 100);
        assert!(e.unwrap_err().contains("holds credentials"));
        // Key files anywhere, by name.
        std::fs::create_dir_all(user.join("Desktop")).unwrap();
        for name in ["server.pem", "tls.KEY", "id_ed25519", "id_rsa.pub", "cert.p12"] {
            let f = user.join("Desktop").join(name);
            std::fs::write(&f, b"k").unwrap();
            let e = read_source(f.to_str().unwrap(), &home, Some(&user), 100).unwrap_err();
            assert!(e.contains("private key"), "{name}: {e}");
        }
        // Next to them, ordinary files are fine.
        for name in ["keyboard.txt", "notes.md", ".docker-compose.yml"] {
            let f = user.join("Desktop").join(name);
            std::fs::write(&f, b"ok").unwrap();
            assert!(read_source(f.to_str().unwrap(), &home, Some(&user), 100).is_ok(), "{name}");
        }
        std::fs::write(user.join(".docker/daemon.json"), b"{}").unwrap();
        assert!(read_source(user.join(".docker/daemon.json").to_str().unwrap(), &home, Some(&user), 100).is_ok());
    }

    #[test]
    fn the_source_is_opened_without_a_controlling_terminal_and_reads_block_again() {
        let d = TestDir::new();
        let home = d.path().join("home");
        let f = d.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let src = include_str!("mod.rs");
        let open = &src[src.find("pub fn read_source").unwrap()..];
        let open = &open[..open.find("fn clear_nonblock").unwrap()];
        assert!(open.contains("libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY"), "the open flags changed");
        assert!(open.contains("clear_nonblock(&f)"));
        let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&f).unwrap();
        clear_nonblock(&file).unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: F_GETFL on an open descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        assert_eq!(read_source(f.to_str().unwrap(), &home, None, 100).unwrap().1, b"x");
    }
}
