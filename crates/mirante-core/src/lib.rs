//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod agent_bin;
pub mod claude_launch;
pub mod delivery;
pub mod dna;
pub mod error;
pub mod git;
pub mod hook;
pub mod model;
pub mod redact;
pub mod skills;
pub mod status;
pub mod store;
pub mod transcript_tail;
pub mod worktree;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
