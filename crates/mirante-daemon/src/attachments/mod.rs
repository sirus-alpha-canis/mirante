//! What a person attaches to a message or a goal: images and files stored in
//! `<home>/sessions/<task>/attachments/` (0700, files 0600, written through
//! [`SafeDir`]), so the agent reads them by path.
//!
//! - Names: the basename only, `[A-Za-z0-9._-]` (anything else becomes `_`), at most
//!   [`MAX_NAME`] characters, stored as `<n>-<name>` so two never collide.
//! - Kinds: an image is PNG, JPEG, GIF or WebP by its magic bytes (never its extension), at
//!   most [`MAX_IMAGE`] bytes; any other file at most [`MAX_FILE`]. A task holds at most
//!   [`MAX_TASK_BYTES`] of them.
//! - Redaction ([`redact`]): a HAR loses its credentials; any other UTF-8 text goes through
//!   `redact_counted`; binary files and images are stored as they are. The original never
//!   lands in Mirante's home.
//! - Import ([`read_source`]): the source is opened with `O_NOFOLLOW` (its last component
//!   is never a symlink) and must be a regular file with a single link that is not inside
//!   Mirante's home (checked on the opened descriptor's path); at most limit + 1 bytes are
//!   read.
pub mod redact;

use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

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

/// Text Mirante redacts: UTF-8 without NUL, not a PDF or a zip.
fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) || bytes.starts_with(b"%PDF-") || bytes.starts_with(b"PK\x03\x04") {
        return None;
    }
    std::str::from_utf8(bytes).ok()
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
    let Some(text) = as_text(&raw) else {
        return Ok(Prepared { bytes: raw, kind: AttachmentKind::File, redacted: 0 });
    };
    let har = name.to_ascii_lowercase().ends_with(".har");
    let (out, redacted) = redact::redact_text(text, har);
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

/// Reads the file a person dropped or picked: `(its name, its bytes)`, at most `max` bytes.
/// Refused: a relative path, a symlink (last component), anything but a regular file with
/// a single link, and anything inside `home` (Mirante's own files).
pub fn read_source(path: &str, home: &Path, max: usize) -> Result<(String, Vec<u8>), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or(format!("{path} names no file"))?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
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
    let canon = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if [&canon, &home.to_path_buf()].iter().any(|h| real.starts_with(h) || p.starts_with(h)) {
        return Err(format!("{path} is inside Mirante's own folder ({}): it is never attached", home.display()));
    }
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
        let (name, bytes) = read_source(file.to_str().unwrap(), &home, 100).unwrap();
        assert_eq!((name.as_str(), bytes.as_slice()), ("trace.har", b"{}".as_slice()));
        // Too big: never more than limit + 1 bytes read.
        std::fs::write(src.join("big.log"), vec![b'x'; 101]).unwrap();
        assert!(read_source(src.join("big.log").to_str().unwrap(), &home, 100).unwrap_err().contains("at most"));
        // Relative, a symlink, a directory, a FIFO, a hard link.
        assert!(read_source("drop/trace.har", &home, 100).unwrap_err().contains("absolute"));
        std::os::unix::fs::symlink(&file, src.join("link.har")).unwrap();
        assert!(read_source(src.join("link.har").to_str().unwrap(), &home, 100).unwrap_err().contains("symlink"));
        assert!(read_source(src.to_str().unwrap(), &home, 100).unwrap_err().contains("not a regular file"));
        let fifo = src.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: plain mkfifo on a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_source(fifo.to_str().unwrap(), &home, 100).unwrap_err().contains("not a regular file"));
        std::fs::hard_link(&file, src.join("twin.har")).unwrap();
        assert!(read_source(file.to_str().unwrap(), &home, 100).unwrap_err().contains("hard links"));
        // Inside Mirante's home: refused, through a symlinked parent too.
        let db = home.join("mirante.sqlite");
        std::fs::write(&db, b"db").unwrap();
        assert!(read_source(db.to_str().unwrap(), &home, 100).unwrap_err().contains("Mirante's own folder"));
        std::os::unix::fs::symlink(&home, d.path().join("alias")).unwrap();
        let via = d.path().join("alias/mirante.sqlite");
        assert!(read_source(via.to_str().unwrap(), &home, 100).unwrap_err().contains("Mirante's own folder"));
    }
}
