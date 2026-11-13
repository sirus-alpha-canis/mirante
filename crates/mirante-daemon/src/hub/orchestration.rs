//! Agent-initiated fan-out (Batch E, `mirante-orchestrator`). A person turns orchestration
//! on for one task (`task.orchestrate`); that task's next sessions get the orchestrator MCP
//! server, through which its agent may propose child tasks (`task.spawn_propose`) and read
//! its own children (`task.children`, `task.child_result`). Every proposal is a card a
//! person decides (`task.spawn_review`; founder decision 1: no budget in v1). An approved
//! child is an ordinary task under its parent (one level deep): its own worktree, agent
//! session and permissions, in manual mode, answered by a person. Nothing here approves a
//! permission, and no agent decides on its own work.
use std::path::PathBuf;

use mirante_core::model::Task;
use mirante_ipc::types::{Orchestrate, TaskView};

use super::{dna, Hub, Result};

impl Hub {
    /// The orchestrator MCP server for a new session of `task`: only when a person turned
    /// orchestration on for it, never for a child (one level deep). When the binary cannot
    /// be found the session starts without it, and the error says why.
    pub(crate) fn orchestrator_server_for(&self, task: &Task) -> Option<PathBuf> {
        if task.parent_id.is_some() {
            return None;
        }
        if !self.lock().store.orchestration(&task.id).unwrap_or(false) {
            return None;
        }
        match self.factory.memory_server() {
            Ok(bin) => Some(bin),
            Err(reason) => {
                let mut st = self.lock();
                st.log_error(
                    Some(&task.id),
                    "orchestrator server",
                    &format!("task_spawn is off for this session: {reason}"),
                );
                st.publish();
                None
            }
        }
    }

    /// `task.orchestrate`: a person turns orchestration on or off for a task. MCP config is
    /// fixed at launch, so it applies from the task's next session (start, or resume).
    pub fn orchestrate(&self, caller: &dna::Caller, p: Orchestrate) -> Result<TaskView> {
        self.require_person(caller)?;
        let mut st = self.lock();
        let task = st.resolve(&p.task)?;
        st.store.set_orchestration(&task.id, p.enabled)?;
        let v = st.view(st.store.get_task(&task.id)?)?;
        st.publish();
        Ok(v)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hub::dna::tests::Peer;
    use crate::hub::testkit::Kit;
    use crate::hub::HubError;
    use mirante_core::claude_launch::AGENT_ENV;
    use mirante_core::model::AgentKind;
    use mirante_core::threads::{LinkKind, ThreadLink};
    use mirante_ipc::rpc::code;
    use mirante_ipc::types::{method, CreateTask, CreateThread, StartSession};
    use serde_json::json;

    pub(crate) fn on(k: &Kit, task: &str) -> TaskView {
        k.hub.orchestrate(&dna::Caller::Person, Orchestrate { task: task.into(), enabled: true }).unwrap()
    }

    fn thread_of(k: &Kit, parent: &str) -> TaskView {
        k.hub
            .create_thread(
                &dna::Caller::Person,
                CreateThread {
                    task: CreateTask {
                        repo: String::new(),
                        repos: Vec::new(),
                        agent: AgentKind::Claude,
                        branch: String::new(),
                        base: None,
                        goal: "review".into(),
                        title: None,
                        group: None,
                    },
                    link: Some(ThreadLink {
                        kind: LinkKind::Pr,
                        reference: "acme/api#1".into(),
                        url: None,
                        title: None,
                    }),
                    parent: Some(parent.into()),
                    notes: None,
                    moved: None,
                },
            )
            .unwrap()
    }

    #[test]
    fn orchestrate_is_person_only_and_shows_in_the_view() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "orch");
        assert!(!t.orchestration);
        let v = on(&k, &t.task.id);
        assert!(v.orchestration);
        assert!(k.kinds(&t.task.id).contains(&"orchestration".to_string()));
        // An agent, even for its own task, is refused (the socket's table, then the hub).
        let agent = Peer::spawn(&[(AGENT_ENV, &t.task.id)], k.dir.path());
        let p = json!({"task": t.task.id, "enabled": false});
        let e = crate::server::dispatch_from(&k.hub, &agent.caller(), method::TASK_ORCHESTRATE, p).unwrap_err();
        assert_eq!(e.code, code::FORBIDDEN);
        let off = Orchestrate { task: t.task.id.clone(), enabled: false };
        assert!(matches!(k.hub.orchestrate(&agent.caller(), off.clone()), Err(HubError::Forbidden(_))));
        assert!(k.hub.get_task(&t.task.id).unwrap().orchestration, "unchanged");
        let v = crate::server::dispatch(&k.hub, method::TASK_ORCHESTRATE, json!(off)).unwrap();
        assert_eq!(v.get("orchestration"), None, "off is omitted from the view");
    }

    #[test]
    fn the_orchestrator_is_given_from_the_next_session_only_while_on() {
        let k = Kit::new();
        *k.factory.mcp_bin.lock().unwrap() = Some("/opt/m/mirante-mcp".into());
        let t = k.started(AgentKind::Claude, "live");
        assert_eq!(t.orchestrator_live, Some(false));
        let v = on(&k, &t.task.id);
        // The live session keeps its config: it applies from the next one.
        assert_eq!((v.orchestration, v.orchestrator_live), (true, Some(false)));
        k.hub.stop(&t.task.id).unwrap();
        k.hub.start_session(StartSession { task: t.task.id.clone(), prompt: None }, true).unwrap();
        assert_eq!(k.hub.get_task(&t.task.id).unwrap().orchestrator_live, Some(true));
        let given = k.factory.orchestrators.lock().unwrap().clone();
        assert_eq!(given, [None, Some(PathBuf::from("/opt/m/mirante-mcp"))]);
    }

    #[test]
    fn a_child_never_gets_the_orchestrator() {
        let k = Kit::new();
        *k.factory.mcp_bin.lock().unwrap() = Some("/opt/m/mirante-mcp".into());
        let parent = k.create(AgentKind::Claude, "chat");
        on(&k, &parent.task.id);
        let th = thread_of(&k, &parent.task.id);
        let e = k.hub.orchestrate(&dna::Caller::Person, Orchestrate { task: th.task.id.clone(), enabled: true });
        assert!(matches!(&e, Err(HubError::Core(_))), "{e:?}");
        assert!(e.unwrap_err().to_string().contains("one level deep"));
        k.hub.start_session(StartSession { task: th.task.id.clone(), prompt: None }, false).unwrap();
        assert_eq!(k.factory.orchestrators.lock().unwrap().clone(), [None]);
        assert!(k.hub.orchestrator_server_for(&th.task).is_none());
        assert!(k.hub.orchestrator_server_for(&parent.task).is_some());
    }

    #[test]
    fn a_missing_binary_starts_the_session_without_it_and_says_why() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "nobin");
        on(&k, &t.task.id);
        assert!(k.hub.orchestrator_server_for(&t.task).is_none());
        let errors = k.hub.lock().store.task_events(&t.task.id).unwrap();
        assert!(errors.iter().any(|e| e.kind == "daemon_error" && e.payload["context"] == "orchestrator server"));
    }
}
