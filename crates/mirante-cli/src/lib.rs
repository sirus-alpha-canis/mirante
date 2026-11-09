//! Library half of the `mirante` CLI (the binary is `src/main.rs`): argument parsing,
//! rendering, terminal helpers and daemon start-up.
pub mod app;
pub mod args;
pub mod confirm;
pub mod daemon_ctl;
pub mod memcheck;
pub mod render;
pub mod skilllint;
pub mod term;
