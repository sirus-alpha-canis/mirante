//! Which files the webview may ask the daemon to copy in (`attachment.import`): only paths
//! the person just gave the app themself, recorded here in Rust, where page script cannot
//! add one. A path is granted when a Finder drop lands on the window (the native drag-drop
//! event, seen by the shell before the webview hears of it) or when the person picks it in
//! the native file picker the shell opens itself (`pick_attachments`). Each grant is used
//! once and expires after [`TTL`]; at most [`MAX`] are kept (the oldest go first).
//!
//! The daemon still refuses its own home, credential folders and key files whatever the
//! shell grants (defence in depth, see `mirante-daemon` `attachments::read_source`).
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mirante_bridge::BridgeError;
use mirante_ipc::rpc::code;
use mirante_ipc::types::method;
use serde_json::Value;

/// How long a dropped or picked path stays attachable.
pub const TTL: Duration = Duration::from_secs(10 * 60);
/// Most grants kept at once.
pub const MAX: usize = 256;

#[derive(Debug, Default)]
pub struct Grants(Mutex<Vec<(PathBuf, Instant)>>);

impl Grants {
    /// Records `paths` (absolute ones only) as attachable once each, from `now`.
    pub fn grant(&self, paths: impl IntoIterator<Item = PathBuf>, now: Instant) {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|(_, at)| now.saturating_duration_since(*at) < TTL);
        g.extend(paths.into_iter().filter(|p| p.is_absolute()).map(|p| (p, now)));
        let over = g.len().saturating_sub(MAX);
        g.drain(..over);
    }

    /// Uses up the grant of `path` when there is a live one.
    pub fn take(&self, path: &str, now: Instant) -> bool {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|(_, at)| now.saturating_duration_since(*at) < TTL);
        match g.iter().position(|(p, _)| p.as_os_str() == path) {
            Some(i) => {
                g.remove(i);
                true
            }
            None => false,
        }
    }
}

/// What the `rpc` command checks before forwarding `method`: an `attachment.import` must
/// name a path the person dropped or picked just now (its grant is used up); any other
/// method passes (the bridge's allowlist applies next).
pub fn check(grants: &Grants, method: &str, params: &Value, now: Instant) -> Result<(), BridgeError> {
    if method != method::ATTACHMENT_IMPORT {
        return Ok(());
    }
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    if grants.take(path, now) {
        return Ok(());
    }
    Err(BridgeError::Rpc {
        code: code::INVALID_PARAMS,
        message: format!(
            "{path} was not dropped on Mirante or picked in its file picker just now: drop or pick it again"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn import(path: &str) -> Value {
        json!({ "task": "t1", "path": path })
    }

    #[test]
    fn an_import_needs_a_fresh_grant_used_once() {
        let g = Grants::default();
        let t0 = Instant::now();
        let m = method::ATTACHMENT_IMPORT;
        // Nothing granted: refused, whatever the path.
        assert!(matches!(check(&g, m, &import("/Users/me/.zsh_history"), t0), Err(BridgeError::Rpc { .. })));
        assert!(check(&g, m, &json!({ "task": "t1" }), t0).is_err());
        g.grant([PathBuf::from("/Users/me/trace.har"), PathBuf::from("relative.txt")], t0);
        assert!(check(&g, m, &import("/Users/me/trace.har"), t0).is_ok());
        // Once only.
        assert!(check(&g, m, &import("/Users/me/trace.har"), t0).is_err());
        // A relative path is never granted.
        assert!(check(&g, m, &import("relative.txt"), t0).is_err());
        // Dropped twice: two uses.
        g.grant([PathBuf::from("/a"), PathBuf::from("/a")], t0);
        assert!(check(&g, m, &import("/a"), t0).is_ok() && check(&g, m, &import("/a"), t0).is_ok());
        assert!(check(&g, m, &import("/a"), t0).is_err());
        // Other methods are not this check's business.
        assert!(check(&g, "task.list", &json!({}), t0).is_ok());
    }

    #[test]
    fn a_grant_expires_and_the_list_is_bounded() {
        let g = Grants::default();
        let t0 = Instant::now();
        g.grant([PathBuf::from("/old")], t0);
        assert!(!g.take("/old", t0 + TTL));
        g.grant([PathBuf::from("/fresh")], t0);
        assert!(g.take("/fresh", t0 + TTL - Duration::from_secs(1)));
        g.grant((0..MAX + 10).map(|i| PathBuf::from(format!("/f{i}"))), t0);
        assert!(!g.take("/f0", t0), "the oldest went first");
        assert!(g.take(&format!("/f{}", MAX + 9), t0));
        assert_eq!(g.0.lock().unwrap().len(), MAX - 1);
    }
}
