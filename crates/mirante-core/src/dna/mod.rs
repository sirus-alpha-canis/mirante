//! DNA, Mirante's repo memory: cited Markdown notes in `<repo>/.mirante/memory/`, checked
//! against the code they cite. Pure library: no daemon, no network. Every git call goes
//! through [`crate::git::Git`].
pub mod anchor;
pub mod check;
pub mod fsio;
pub mod note;

use sha2::{Digest, Sha256};

pub const MEMORY_DIR: &str = ".mirante/memory";
pub const MAX_NOTE_BYTES: usize = 64 * 1024;
pub const MAX_BODY_BYTES: usize = 16 * 1024;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_ANCHORS: usize = 8;
pub const MAX_NOTES: usize = 2000;
pub const MAX_CITED_FILE: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DnaError {
    #[error("invalid anchor {0:?}: {1}")]
    Anchor(String, &'static str),
    #[error("path {0:?} cannot be cited: {1}")]
    Path(String, &'static str),
    #[error("note: {0}")]
    Note(String),
    #[error("unsafe memory directory: {0}")]
    Unsafe(String),
    #[error("{0}")]
    Git(String),
    #[error("the note changed on disk since it was read; reload and try again")]
    Changed,
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for DnaError {
    fn from(e: std::io::Error) -> Self {
        DnaError::Io(e.to_string())
    }
}

/// Full SHA-256 hex (trust ledger, content hashes).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
