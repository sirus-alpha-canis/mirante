//! The Mirante daemon: the single owner of state. It keeps the `Store`, one live session per
//! task (agent process + status engine), serves the Unix socket, and turns hook events,
//! transcript lines and Codex notifications into persisted status, turns and permissions.
pub mod access;
pub mod diff;
pub mod driver;
pub mod hub;
pub mod memguard;
pub mod recover;
pub mod server;
