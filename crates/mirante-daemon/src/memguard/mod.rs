//! Memory guard: keep a handful of agents from taking the machine down.
//!
//! Measure and act, never limit: macOS does not enforce `RLIMIT_AS` / `RLIMIT_DATA`
//! (`setrlimit` fails with `EINVAL` or has no effect), so there are no hard limits. Instead
//! each live session's process tree is sampled ([`sampler`]: `phys_footprint` on macOS, PSS
//! on Linux) every 2 s (500 ms under pressure), the machine's pressure is read from the OS,
//! and a pure [`policy`] decides what to do:
//! - a session over 80 % / 100 % of its budget (default `min(4 GiB, 25 % of RAM)`) → a
//!   warning;
//! - the machine under pressure (`Elevated`) → new dispatches and queued deliveries pause,
//!   and the user is offered to hibernate the largest idle session;
//! - sustained `Critical` pressure → the largest IDLE session is hibernated, at most one per
//!   60 s, everything logged. Never a working session, never one with a pending permission
//!   request (the refusal is reported instead);
//! - an idle session that keeps growing → reported as a leak, with an offer;
//! - optional (off by default): hibernate sessions idle for a configured time.
//!
//! Hibernating is stopping the agent's process tree and resuming its native conversation
//! later; the hub does it (Task 19). Processes left behind by ended sessions are found by
//! [`orphans`] and only reported; killing one is the user's call.
pub mod orphans;
pub mod policy;
pub mod sampler;

pub use orphans::{find_orphans, kill_orphan, Orphan};
pub use policy::{Action, Config, Level, MemState, Policy, SessionInfo, SessionMem};
pub use sampler::{os_sampler, FakeSampler, MemSampler, Pressure, SystemSample, TreeSample};
