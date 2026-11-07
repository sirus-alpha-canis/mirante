//! Library half of the `mirante` CLI (the binary is `src/main.rs`): argument parsing,
//! rendering, terminal helpers and daemon start-up.
pub mod args;
pub mod daemon_ctl;
pub mod render;
pub mod term;
