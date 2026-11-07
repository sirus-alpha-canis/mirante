//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod claude_launch;
pub mod delivery;
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;
pub mod transcript_tail;
pub mod worktree;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
