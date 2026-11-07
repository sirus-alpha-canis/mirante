//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;
pub mod model;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
