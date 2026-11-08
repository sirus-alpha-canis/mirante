//! The desktop cockpit's side of the daemon socket. Plain std threads and blocking I/O
//! (like the rest of Mirante), no Tauri types: the Tauri shell in `apps/cockpit/src-tauri`
//! only forwards these calls and messages, so everything here is tested against a fake
//! daemon without a window.
//!
//! - [`rpc`]: one request per connection, restricted to an allowlist of methods.
//! - [`stream`]: `events.subscribe` in a thread that reconnects and resumes with
//!   `since_seq` + `boot_id`.
//! - [`attach`]: a task's live terminal as raw bytes both ways.
//! - [`daemon`]: finding and starting `mirante-daemon`.
//! - [`login_env`]: the login shell's PATH and `MIRANTE_*` for a daemon the app starts.
pub mod attach;
pub mod daemon;
pub mod login_env;
pub mod rpc;
pub mod stream;

#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(any(test, feature = "fake"))]
pub mod fake_dna;

pub use rpc::{call, BridgeError};
