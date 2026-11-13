//! Which socket methods each kind of caller may call, checked before a request is parsed.
//!
//! The daemon socket is reachable by anything running as the user, Mirante-launched agents
//! included. [`Hub::identify`](crate::hub::Hub::identify) tells a person from an agent; this
//! table says what each may do:
//!
//! - **Person**: everything.
//! - **Agent** (a Mirante-launched agent's process tree: the agent, `mirante-hook`,
//!   `mirante-mcp`, anything they start): only what those need: `hook.event` (the hook's
//!   delivery, for its own task only), `dna.search` / `dna.get` / `dna.propose` (the memory
//!   MCP server, own task only, checked by the hub), `skill.propose` (the skills MCP server,
//!   own task only), and `daemon.status` (pid and version, nothing about tasks). Never a
//!   lifecycle method (`task.create`, `session.start`, `message.send`, `attach`,
//!   `permission.answer`, …), a read of other tasks, or a decision.
//! - **Unverified** (the peer could not be checked: gone, its pid reused, or its
//!   environment or working directory unreadable, as for macOS platform binaries like
//!   `nc` or `/usr/bin/python3`): `daemon.status` only. It may be an agent, so it gets
//!   less than one. The cockpit and the `mirante` CLI are ordinary binaries whose
//!   environment and working directory are readable, so they are never Unverified while
//!   they wait for a reply; a script that talks to the socket through a platform binary is
//!   refused with what to do instead.
//!
//! A method missing from [`ACCESS`] is a person's only (fail closed), and the tests fail
//! until it is classified.
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::method;

use crate::hub::dna::Identity;

/// Who may call a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A person, an agent, and a caller that could not be checked.
    Anyone,
    /// A person, or a Mirante-launched agent (for its own task: the hub checks which).
    Agents,
    /// A person only.
    Person,
}

/// Every method the socket serves, with who may call it.
pub const ACCESS: &[(&str, Access)] = &[
    (method::TASK_CREATE, Access::Person),
    (method::TASK_LIST, Access::Person),
    (method::TASK_GET, Access::Person),
    (method::TASK_ARCHIVE, Access::Person),
    (method::TASK_EVENTS, Access::Person),
    (method::TASK_DIFF, Access::Person),
    (method::SESSION_START, Access::Person),
    (method::SESSION_RESUME, Access::Person),
    (method::SESSION_TRUST, Access::Person),
    (method::SESSION_STOP, Access::Person),
    (method::SESSION_INTERRUPT, Access::Person),
    (method::SESSION_RESIZE, Access::Person),
    (method::SESSION_HIBERNATE, Access::Person),
    (method::SESSION_WAKE, Access::Person),
    (method::MESSAGE_SEND, Access::Person),
    (method::MESSAGE_LIST_QUEUED, Access::Person),
    (method::MESSAGE_EDIT, Access::Person),
    (method::MESSAGE_REMOVE, Access::Person),
    (method::MESSAGE_REORDER, Access::Person),
    (method::PERMISSION_LIST, Access::Person),
    (method::PERMISSION_ANSWER, Access::Person),
    (method::EVENTS_SUBSCRIBE, Access::Person),
    (method::ATTACH, Access::Person),
    (method::HOOK_EVENT, Access::Agents),
    (method::DAEMON_STATUS, Access::Anyone),
    (method::DAEMON_SHUTDOWN, Access::Person),
    (method::DAEMON_DOCTOR, Access::Person),
    (method::DAEMON_KILL_ORPHAN, Access::Person),
    (method::DAEMON_SETTINGS, Access::Person),
    (method::REPO_FILES, Access::Person),
    // A task's worktree files and their contents (the cockpit's side pane): never an agent's.
    (method::TASK_FILES, Access::Person),
    (method::TASK_FILE, Access::Person),
    // Claude's own agents in a task (their texts, read from side transcripts): never an
    // agent's, not even for its own task.
    (method::TASK_SUBAGENTS, Access::Person),
    // Fan-out groups: made, read, archived and synthesized by a person only.
    (method::GROUP_CREATE, Access::Person),
    (method::GROUP_GET, Access::Person),
    (method::GROUP_ARCHIVE, Access::Person),
    (method::GROUP_SYNTHESIZE, Access::Person),
    // A thread's notes and link: the person's context for it, never an agent's to change.
    (method::TASK_THREAD, Access::Person),
    // A thread's summary: deterministic, or the person's own CLI on their click (it spends
    // their quota): never an agent's.
    (method::TASK_SUMMARIZE, Access::Person),
    // Orchestration (whether a task's agent gets the orchestrator MCP server): a person's
    // decision, never an agent's, not even for its own task.
    (method::TASK_ORCHESTRATE, Access::Person),
    (method::DNA_STATUS, Access::Person),
    (method::DNA_ENABLE, Access::Person),
    (method::DNA_DISABLE, Access::Person),
    (method::DNA_GRAPH, Access::Person),
    (method::DNA_SEARCH, Access::Agents),
    (method::DNA_GET, Access::Agents),
    (method::DNA_CHECK, Access::Person),
    (method::DNA_PROPOSE, Access::Agents),
    (method::DNA_PROPOSALS, Access::Person),
    (method::DNA_REVIEW, Access::Person),
    (method::DNA_TASK_CONTEXT, Access::Person),
    (method::DNA_NOTE_ACTION, Access::Person),
    (method::DNA_MAY_REVIEW, Access::Person),
    (method::SKILL_TENANTS, Access::Person),
    (method::SKILL_LEARN, Access::Person),
    (method::SKILL_LIST, Access::Person),
    (method::SKILL_GET, Access::Person),
    (method::SKILL_CONFIRM_ORIGIN, Access::Person),
    (method::SKILL_LINT, Access::Person),
    (method::SKILL_PROPOSE, Access::Agents),
    (method::SKILL_ADD, Access::Person),
    (method::SKILL_REREVIEW, Access::Person),
    (method::SKILL_PENDING, Access::Person),
    (method::SKILL_PROPOSALS, Access::Person),
    (method::SKILL_REVIEW, Access::Person),
    (method::SKILL_ACTION, Access::Person),
    (method::SKILL_MINE, Access::Person),
    (method::SKILL_TASK_SKILLS, Access::Person),
    (method::SKILL_MAY_REVIEW, Access::Person),
    (method::SKILL_REPO_ORIGIN, Access::Person),
    // Reads the person's own agent configuration (home folder): never an agent's.
    (method::RULES_LAYERS, Access::Person),
];

/// Who may call `m`: its [`ACCESS`] entry, or a person only when it has none.
pub fn access_of(m: &str) -> Access {
    ACCESS.iter().find(|(name, _)| *name == m).map_or(Access::Person, |(_, a)| *a)
}

/// Ok when `who` may call `m`, else `FORBIDDEN` with what to do instead.
pub fn check(m: &str, who: &Identity) -> Result<(), RpcError> {
    let ok = match (access_of(m), who) {
        (_, Identity::Person) | (Access::Anyone, _) => true,
        (Access::Agents, Identity::Agent { .. }) => true,
        (Access::Agents | Access::Person, _) => false,
    };
    if ok {
        return Ok(());
    }
    let msg = match who {
        Identity::Agent { .. } => format!(
            "Mirante-launched agents are refused: {m} is a person's action. Use the cockpit, or the mirante CLI \
             in a terminal you opened yourself"
        ),
        Identity::Unverified(why) => format!(
            "{why}: {m} needs a caller Mirante can check. Use the cockpit, or the mirante CLI in a terminal \
             you opened yourself"
        ),
        Identity::Person => unreachable!("a person may call every method"),
    };
    Err(RpcError::new(code::FORBIDDEN, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// `NAME => "value"` for every constant of `mirante_ipc::types::method`, read from its
    /// source: a constant added there without a classification here fails the tests.
    pub(crate) fn method_constants() -> BTreeMap<String, String> {
        let src = include_str!("../../mirante-ipc/src/types.rs");
        let start = src.find("pub mod method {").expect("the method module");
        let body = &src[start..start + src[start..].find("\n}\n").expect("its end")];
        body.lines()
            .filter_map(|l| {
                let l = l.trim().strip_prefix("pub const ")?;
                let (name, rest) = l.split_once(": &str = \"")?;
                Some((name.to_string(), rest.strip_suffix("\";")?.to_string()))
            })
            .collect()
    }

    /// The method constants `server.rs` dispatches on (`method::NAME =>` arms).
    pub(crate) fn dispatched_constants() -> BTreeSet<String> {
        let src = include_str!("server.rs");
        let body = &src[..src.find("#[cfg(test)]").expect("the tests")];
        body.split("method::")
            .skip(1)
            .filter_map(|rest| {
                let name: String = rest.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
                let after = rest[name.len()..].trim_start();
                (!name.is_empty() && after.starts_with("=>")).then_some(name)
            })
            .collect()
    }

    #[test]
    fn every_method_and_every_dispatch_arm_is_classified() {
        let consts = method_constants();
        assert!(consts.len() > 50, "parsed {} constants", consts.len());
        let classified: BTreeSet<&str> = ACCESS.iter().map(|(m, _)| *m).collect();
        assert_eq!(classified.len(), ACCESS.len(), "a method is listed twice");
        for (name, value) in &consts {
            // `event` is the notification the daemon sends, not a method it serves.
            if name == "EVENT" {
                assert!(!classified.contains(value.as_str()));
                continue;
            }
            assert!(classified.contains(value.as_str()), "method::{name} ({value}) has no classification");
        }
        let values: BTreeSet<&str> = consts.values().map(String::as_str).collect();
        for m in &classified {
            assert!(values.contains(m), "{m} is classified but is no method constant");
        }
        let arms = dispatched_constants();
        assert!(arms.len() > 50, "parsed {} arms", arms.len());
        for name in &arms {
            let value = consts.get(name).unwrap_or_else(|| panic!("method::{name} is no constant"));
            assert!(classified.contains(value.as_str()), "dispatched {value} has no classification");
        }
    }

    /// The whole table, spelled out: changing who may call a method changes this test too.
    #[test]
    fn agents_may_call_only_these_methods_and_unverified_callers_only_status() {
        let agents: BTreeSet<&str> = ACCESS.iter().filter(|(_, a)| *a != Access::Person).map(|(m, _)| *m).collect();
        let expected: BTreeSet<&str> = [
            method::HOOK_EVENT,
            method::DAEMON_STATUS,
            method::DNA_SEARCH,
            method::DNA_GET,
            method::DNA_PROPOSE,
            method::SKILL_PROPOSE,
        ]
        .into();
        assert_eq!(agents, expected);
        let anyone: Vec<&str> = ACCESS.iter().filter(|(_, a)| *a == Access::Anyone).map(|(m, _)| *m).collect();
        assert_eq!(anyone, [method::DAEMON_STATUS]);
    }

    /// `rules.layers` shows the person's own agent configuration (their home folder): an
    /// agent, or a caller Mirante cannot check, never gets it.
    #[test]
    fn rules_layers_is_a_persons_only() {
        assert_eq!(access_of(method::RULES_LAYERS), Access::Person);
        let agent = Identity::Agent { task: Some("t".into()) };
        assert_eq!(check(method::RULES_LAYERS, &agent).unwrap_err().code, code::FORBIDDEN);
        let unverified = Identity::Unverified("x".into());
        assert_eq!(check(method::RULES_LAYERS, &unverified).unwrap_err().code, code::FORBIDDEN);
        assert!(check(method::RULES_LAYERS, &Identity::Person).is_ok());
    }

    #[test]
    fn check_follows_the_table_for_each_identity() {
        let agent = Identity::Agent { task: Some("t".into()) };
        let orphan = Identity::Agent { task: None };
        let unverified = Identity::Unverified("could not read the calling process".into());
        for (m, a) in ACCESS {
            assert!(check(m, &Identity::Person).is_ok(), "{m}");
            for who in [&agent, &orphan] {
                assert_eq!(check(m, who).is_ok(), *a != Access::Person, "{m} {who:?}");
            }
            assert_eq!(check(m, &unverified).is_ok(), *a == Access::Anyone, "{m}");
        }
        let e = check(method::TASK_CREATE, &agent).unwrap_err();
        assert_eq!(e.code, code::FORBIDDEN);
        assert!(e.message.contains("Mirante-launched agents are refused"), "{}", e.message);
        let e = check(method::TASK_LIST, &unverified).unwrap_err();
        assert!(e.message.starts_with("could not read the calling process: task.list"), "{}", e.message);
        // Unclassified: a person's only.
        assert!(check("task.someday", &Identity::Person).is_ok());
        assert_eq!(check("task.someday", &agent).unwrap_err().code, code::FORBIDDEN);
    }
}
