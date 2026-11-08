//! Skills: reusable instructions Mirante gives each new agent session whose scope matches
//! the task repo's normalized origin. Pure library: origin normalization, the format,
//! lint, signing, the versioned library and the miner. No daemon, no network.
pub mod format;
pub mod index;
pub mod library;
pub mod lint;
pub mod mine;
pub mod origin;
pub mod sign;

/// The manifest's format tag.
pub const FORMAT: &str = "mirante.skill/v1";
/// The manifest's file name in a version folder.
pub const MANIFEST: &str = "mirante-skill.json";
/// The signature's file name in a version folder.
pub const SIGNATURE: &str = "mirante-skill.sig";
/// The SSHSIG namespace of skill signatures.
pub const NAMESPACE: &str = "mirante-skill";
/// The principal of the key Mirante generates.
pub const PRINCIPAL: &str = "mirante-local";
/// The skill's instructions file.
pub const SKILL_MD: &str = "SKILL.md";

pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 1024;
pub const MAX_BODY: usize = 16 * 1024;
/// Extra files besides `SKILL.md`.
pub const MAX_FILES: usize = 8;
pub const MAX_FILE: usize = 64 * 1024;
pub const MAX_TOTAL: usize = 128 * 1024;
/// Versions kept per skill (`library::prune`).
pub const KEEP_VERSIONS: usize = 20;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SkillError {
    #[error("origin: {0}")]
    Origin(String),
    #[error("scope: {0}")]
    Scope(String),
    #[error("format: {0}")]
    Format(String),
    #[error("signature: {0}")]
    Sign(String),
    #[error("unsafe skill directory: {0}")]
    Unsafe(String),
    /// Creating something whose name is taken.
    #[error("{0} already exists")]
    Exists(String),
    #[error("{0} does not exist")]
    Missing(String),
    /// A version folder that no longer matches its manifest or ledger hash.
    #[error("modified on disk: {0}")]
    Modified(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for SkillError {
    fn from(e: std::io::Error) -> Self {
        SkillError::Io(e.to_string())
    }
}
