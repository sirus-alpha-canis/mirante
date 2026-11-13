//! Agent-initiated fan-out (Batch E, `mirante-orchestrator`). A person turns orchestration
//! on for one task (`task.orchestrate`); that task's next sessions get the orchestrator MCP
//! server, through which its agent may propose child tasks (`task.spawn_propose`) and read
//! its own children (`task.children`, `task.child_result`). Every proposal is a card a
//! person decides (`task.spawn_review`; founder decision 1: no budget in v1). An approved
//! child is an ordinary task under its parent (one level deep): its own worktree, agent
//! session and permissions, in manual mode, answered by a person. Nothing here approves a
//! permission, and no agent decides on its own work.
use std::path::PathBuf;

use mirante_core::groups::child_branch;
use mirante_core::model::{Lifecycle, Task};
use mirante_core::orchestration::{check_goal, check_proposal, NewSpawn, SpawnProposal, MAX_PER_CALL, MAX_TITLE};
use mirante_ipc::types::{
    ChildResult, ChildResultParams, ChildView, ChildrenView, CreateTask, Orchestrate, SpawnProposalView,
    SpawnProposals, SpawnPropose, SpawnReview, SpawnReviewed, StartSession, TaskView,
};
use serde_json::json;

use super::dna::Identity;
use super::{dna, groups::branch_taken, title_from_goal, Hub, HubError, Placement, Result, State};

/// Characters of a child's last answer `task.children` shows its parent's agent.
pub const CHILD_ANSWER_CHARS: usize = 1500;
/// Bytes of a child's final answer `task.child_result` hands its parent's agent.
pub const CHILD_RESULT_BYTES: usize = 16 * 1024;
/// How many child branch names are tried when git already has one.
const MAX_RENAMES: usize = 20;

/// Agent text headed back to an agent: credentials masked again, at most `max` bytes (cut
/// on a character boundary, marked).
fn capped(text: &str, max: usize) -> String {
    let t = mirante_core::redact::redact(text);
    if t.len() <= max {
        return t;
    }
    let mut end = max;
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &t[..end])
}

impl State {
    fn proposal_view(&self, p: SpawnProposal) -> Result<SpawnProposalView> {
        let parent = self.store.get_task(&p.parent_id)?;
        let repos = self.store.task_repos(&parent.id)?.into_iter().map(|r| r.name).collect();
        Ok(SpawnProposalView { proposal: p, parent_title: parent.title, parent_branch: parent.branch, repos })
    }
}

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

    /// The task `named`, when `caller` may act on it as an orchestrating agent: a person
    /// any task; a Mirante-launched agent its own task only (found by its process tree and
    /// env marker, never by the parameter); anyone else nothing.
    fn own_task(&self, caller: &dna::Caller, named: &str, what: &str) -> Result<Task> {
        let own =
            |why: &str| HubError::Forbidden(format!("{why}: a Mirante-launched agent can only {what} its own task"));
        let agent_task = match self.identify(caller) {
            Identity::Person => None,
            Identity::Agent { task: Some(t) } => Some(t),
            Identity::Agent { task: None } => return Err(own("this agent's task is unknown")),
            Identity::Unverified(why) => return Err(HubError::Forbidden(why)),
        };
        let task = self.lock().resolve(named)?;
        if agent_task.is_some_and(|t| t != task.id) {
            return Err(own("another task was named"));
        }
        Ok(task)
    }

    /// `task.spawn_propose`: the task's agent proposes 1 to 5 children. Each waits for a
    /// person's decision; nothing is created. Refused while orchestration is off, for a
    /// child task, and past the caps. Goals, titles and reasons are masked and capped.
    pub fn spawn_propose(&self, caller: &dna::Caller, p: SpawnPropose) -> Result<Vec<SpawnProposalView>> {
        let task = self.own_task(caller, &p.task, "propose children for")?;
        if p.children.is_empty() || p.children.len() > MAX_PER_CALL {
            return Err(HubError::Invalid(format!("propose 1 to {MAX_PER_CALL} children at a time")));
        }
        let mut list = Vec::with_capacity(p.children.len());
        for (i, c) in p.children.iter().enumerate() {
            let agent = c.agent.unwrap_or(task.agent);
            let checked = check_proposal(&c.goal, agent, c.title.as_deref(), c.why.as_deref())
                .map_err(|e| HubError::Invalid(format!("child {}: {e}", i + 1)))?;
            list.push(checked);
        }
        let mut st = self.lock();
        let made = st.store.insert_proposals(&task.id, &list)?;
        let views = made.into_iter().map(|p| st.proposal_view(p)).collect::<Result<Vec<_>>>()?;
        st.publish();
        Ok(views)
    }

    /// `task.spawn_proposals`: a person's list (the cockpit's cards, the CLI's).
    pub fn spawn_proposals(&self, caller: &dna::Caller, p: SpawnProposals) -> Result<Vec<SpawnProposalView>> {
        self.require_person(caller)?;
        let st = self.lock();
        let parent = match &p.task {
            Some(t) => Some(st.resolve(t)?.id),
            None => None,
        };
        let rows = st.store.proposals(parent.as_deref(), p.status)?;
        rows.into_iter().map(|r| st.proposal_view(r)).collect()
    }

    /// `task.spawn_review`: a person decides a proposal. Rejected: nothing is made. Approved:
    /// the child is made under its parent with the person's edits (goal, agent, title, a
    /// workspace parent's repos), named `<parent branch>--<n>-<slug>`, from the parent's
    /// branch (a workspace parent: its repos at `HEAD`), its origin copied from the
    /// parent's; then (unless `start: false`) started with its goal as the first prompt, in
    /// manual mode like every task. A child that could not start stays made.
    pub fn spawn_review(&self, caller: &dna::Caller, r: SpawnReview) -> Result<SpawnReviewed> {
        self.require_person(caller)?;
        if !r.accept {
            let mut st = self.lock();
            let p = st.store.reject_proposal(&r.id)?;
            let proposal = st.proposal_view(p)?;
            st.publish();
            return Ok(SpawnReviewed { proposal, child: None, started: None, start_error: None });
        }
        let e = r.edits.clone().unwrap_or_default();
        let (proposal, parent, made, repos) = {
            let st = self.lock();
            let p = st.store.proposal(&r.id)?;
            if p.status != mirante_core::orchestration::ProposalStatus::Pending {
                return Err(HubError::Conflict(format!("that proposal was already decided ({})", p.status.as_str())));
            }
            let parent = st.store.get_task(&p.parent_id)?;
            if matches!(parent.lifecycle, Lifecycle::Archiving | Lifecycle::Archived) {
                return Err(HubError::Conflict(format!("task {} is archived", parent.branch)));
            }
            let made = st.store.children_of(&parent.id)?.len();
            let repos = st.store.task_repos(&parent.id)?;
            (p, parent, made, repos)
        };
        let goal = check_goal(e.goal.as_deref().unwrap_or(&proposal.goal)).map_err(HubError::Invalid)?;
        let agent = e.agent.unwrap_or(proposal.agent);
        let title = e
            .title
            .as_deref()
            .map(|t| t.trim().chars().take(MAX_TITLE).collect::<String>())
            .filter(|t| !t.is_empty())
            .or_else(|| proposal.title.clone())
            .unwrap_or_else(|| title_from_goal(&goal));
        let picked: Vec<String> = match (&e.repos, repos.is_empty()) {
            (None, _) => repos.iter().map(|r| r.repo_path.clone()).collect(),
            (Some(_), true) => {
                return Err(HubError::Invalid("repos can be picked only for a parent across several repos".into()))
            }
            (Some(names), false) => {
                if let Some(bad) = names.iter().find(|n| !repos.iter().any(|r| &r.name == *n)) {
                    return Err(HubError::Invalid(format!("{bad} is not one of the parent's repos")));
                }
                if !names.contains(&repos[0].name) {
                    return Err(HubError::Invalid(format!(
                        "the child keeps the parent's primary repo ({}): its origin comes from it",
                        repos[0].name
                    )));
                }
                repos.iter().filter(|r| names.contains(&r.name)).map(|r| r.repo_path.clone()).collect()
            }
        };
        let mut edits = json!({});
        if goal != proposal.goal {
            edits["goal"] = json!(goal);
        }
        if agent != proposal.agent {
            edits["agent"] = json!(agent);
        }
        if Some(&title) != proposal.title.as_ref() {
            edits["title"] = json!(title);
        }
        if e.repos.is_some() {
            edits["repos"] = json!(e.repos);
        }
        let spawn = NewSpawn { parent: parent.id.clone(), proposal: proposal.id.clone(), edits };
        let mut p = CreateTask {
            repo: parent.group_path.clone().unwrap_or_else(|| parent.repo_path.clone()),
            repos: picked,
            agent,
            branch: String::new(),
            base: Some(if parent.workspace_path.is_some() { "HEAD".into() } else { parent.branch.clone() }),
            goal: goal.clone(),
            title: Some(title.clone()),
            group: None,
        };
        let mut n = made + 1;
        let child = loop {
            p.branch = child_branch(&parent.branch, n, &title);
            match self.create_in(caller, p.clone(), Placement::Spawn(spawn.clone())) {
                Err(e) if branch_taken(&e) && n < made + MAX_RENAMES => n += 1,
                other => break other?,
            }
        };
        let (started, start_error) = if r.start.unwrap_or(true) {
            match self.start_session(StartSession { task: child.task.id.clone(), prompt: Some(goal) }, false) {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        let st = self.lock();
        let proposal = st.proposal_view(st.store.proposal(&proposal.id)?)?;
        let child = st.view(st.store.get_task(&child.task.id)?)?;
        Ok(SpawnReviewed { proposal, child: Some(child), started, start_error })
    }

    /// `task.children`: the children the task's agent proposed and a person approved, and
    /// its proposals. Agent text is masked again and capped.
    pub fn task_children(&self, caller: &dna::Caller, task: &str) -> Result<ChildrenView> {
        let task = self.own_task(caller, task, "read the children of")?;
        let st = self.lock();
        let mut children = Vec::new();
        for c in st.store.spawned_children(&task.id)? {
            let v = st.view(c)?;
            let last = st.store.last_answer(&v.task.id)?;
            let proposal = st
                .store
                .proposals(Some(&task.id), None)?
                .into_iter()
                .find(|p| p.child_id.as_deref() == Some(v.task.id.as_str()))
                .map(|p| p.id);
            children.push(ChildView {
                id: v.task.id.clone(),
                title: v.task.title.clone(),
                branch: v.task.branch.clone(),
                agent: v.task.agent,
                status: v.task.status,
                lifecycle: v.task.lifecycle,
                live: v.live,
                needs_you: v.needs_you > 0,
                proposal,
                last_answer: last.map(|a| {
                    let short: String = a.chars().take(CHILD_ANSWER_CHARS).collect();
                    let cut = short.len() < a.len();
                    let mut s = mirante_core::redact::redact(&short);
                    if cut {
                        s.push_str("… [truncated]");
                    }
                    s
                }),
            });
        }
        let proposals = st.store.proposals(Some(&task.id), None)?;
        Ok(ChildrenView { children, proposals })
    }

    /// `task.child_result`: one approved child of the task (by id, branch or suffix): its
    /// status, last finished answer (masked, capped) and diff stat (git without the lock).
    pub fn child_result(&self, caller: &dna::Caller, p: ChildResultParams) -> Result<ChildResult> {
        let task = self.own_task(caller, &p.task, "read the children of")?;
        let (child, answer, repos, live) = {
            let st = self.lock();
            let child = st.resolve(&p.child)?;
            if child.parent_id.as_deref() != Some(task.id.as_str())
                || child.child_kind != Some(mirante_core::model::ChildKind::Spawn)
            {
                return Err(HubError::Forbidden(format!(
                    "task {} is not a child this task's agent proposed: only those can be read",
                    child.branch
                )));
            }
            let answer = st.store.last_answer(&child.id)?;
            let repos = st.store.task_repos(&child.id)?;
            let live = st.live.contains_key(&child.id);
            (child, answer, repos, live)
        };
        let diff =
            (child.lifecycle != Lifecycle::Archived).then(|| self.diff_stat_of(&child, &repos)).and_then(|r| r.ok());
        Ok(ChildResult {
            ended: !live,
            child: child.id,
            title: child.title,
            branch: child.branch,
            status: child.status,
            lifecycle: child.lifecycle,
            final_answer: answer.map(|a| capped(&a, CHILD_RESULT_BYTES)),
            diff,
        })
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
    use mirante_ipc::types::{method, ArchiveTask, CreateTask, CreateThread, StartSession};
    use serde_json::json;
    use std::path::Path;

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

    use mirante_core::model::ChildKind;
    use mirante_core::orchestration::ProposalStatus;
    use mirante_ipc::types::{ProposedChild, SpawnEdits};

    fn kid(goal: &str) -> ProposedChild {
        ProposedChild { goal: goal.into(), agent: None, title: Some(format!("T {goal}")), why: Some("split".into()) }
    }

    fn propose(k: &Kit, caller: &dna::Caller, task: &str, goals: &[&str]) -> Result<Vec<SpawnProposalView>> {
        k.hub
            .spawn_propose(caller, SpawnPropose { task: task.into(), children: goals.iter().map(|g| kid(g)).collect() })
    }

    fn accept(k: &Kit, id: &str, edits: Option<SpawnEdits>) -> Result<SpawnReviewed> {
        k.hub.spawn_review(&dna::Caller::Person, SpawnReview { id: id.into(), accept: true, edits, start: None })
    }

    fn forbidden_with<T: std::fmt::Debug>(r: Result<T>, needle: &str) {
        match r {
            Err(HubError::Forbidden(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_agent_proposes_for_its_own_task_only() {
        let k = Kit::new();
        let mine = k.create(AgentKind::Claude, "mine").task;
        let other = k.create(AgentKind::Claude, "other").task;
        on(&k, &mine.id);
        on(&k, &other.id);
        let agent = Peer::spawn(&[(AGENT_ENV, &mine.id)], k.dir.path());
        forbidden_with(propose(&k, &agent.caller(), &other.id, &["x"]), "another task was named");
        // An agent whose task is unknown (in a worktree, no marker), and an unchecked caller.
        let orphan = Peer::spawn(&[], Path::new(&mine.worktree_path));
        forbidden_with(propose(&k, &orphan.caller(), &mine.id, &["x"]), "unknown");
        assert!(matches!(propose(&k, &dna::Caller::Unknown, &mine.id, &["x"]), Err(HubError::Forbidden(_))));
        let ok = propose(&k, &agent.caller(), &mine.id, &["write the tests"]).unwrap();
        assert_eq!((ok.len(), ok[0].proposal.agent, ok[0].parent_branch.as_str()), (1, AgentKind::Claude, "mine"));
        // Over the socket too: the table lets an agent in, the hub keeps it to its task.
        let p = json!({"task": other.id, "children": [{"goal": "x"}]});
        let e = crate::server::dispatch_from(&k.hub, &agent.caller(), method::TASK_SPAWN_PROPOSE, p).unwrap_err();
        assert_eq!(e.code, code::FORBIDDEN);
        assert_eq!(k.hub.get_task(&mine.id).unwrap().spawn_pending, 1, "a pending proposal waits on the person");
    }

    #[test]
    fn proposing_is_refused_while_orchestration_is_off() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "off").task;
        let e = propose(&k, &dna::Caller::Person, &t.id, &["x"]).unwrap_err();
        assert!(e.to_string().contains("orchestration is off"), "{e}");
        on(&k, &t.id);
        propose(&k, &dna::Caller::Person, &t.id, &["x"]).unwrap();
        k.hub.orchestrate(&dna::Caller::Person, Orchestrate { task: t.id.clone(), enabled: false }).unwrap();
        assert!(propose(&k, &dna::Caller::Person, &t.id, &["y"]).is_err());
    }

    #[test]
    fn proposals_over_the_caps_are_refused() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "caps").task;
        on(&k, &t.id);
        let six = ["a", "b", "c", "d", "e", "f"];
        assert!(matches!(propose(&k, &dna::Caller::Person, &t.id, &six), Err(HubError::Invalid(_))));
        assert!(matches!(propose(&k, &dna::Caller::Person, &t.id, &[]), Err(HubError::Invalid(_))));
        propose(&k, &dna::Caller::Person, &t.id, &six[..5]).unwrap();
        propose(&k, &dna::Caller::Person, &t.id, &six[..5]).unwrap();
        let e = propose(&k, &dna::Caller::Person, &t.id, &["k"]).unwrap_err();
        assert!(e.to_string().contains("wait for the person"), "{e}");
        assert!(matches!(propose(&k, &dna::Caller::Person, &t.id, &[" "]), Err(HubError::Invalid(_))));
    }

    #[test]
    fn review_is_person_only_and_edits_apply_before_creation() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "mirante/login").task;
        on(&k, &t.id);
        let agent = Peer::spawn(&[(AGENT_ENV, &t.id)], k.dir.path());
        let p = propose(&k, &agent.caller(), &t.id, &["add tests"]).unwrap().remove(0).proposal;
        // The agent cannot approve its own work: not the hub, not the socket.
        let mine = SpawnReview { id: p.id.clone(), accept: true, edits: None, start: None };
        assert!(matches!(k.hub.spawn_review(&agent.caller(), mine.clone()), Err(HubError::Forbidden(_))));
        let e = crate::server::dispatch_from(&k.hub, &agent.caller(), method::TASK_SPAWN_REVIEW, json!(mine));
        assert_eq!(e.unwrap_err().code, code::FORBIDDEN);
        let e = crate::server::dispatch_from(&k.hub, &agent.caller(), method::TASK_SPAWN_PROPOSALS, json!({}));
        assert_eq!(e.unwrap_err().code, code::FORBIDDEN);
        let edits = SpawnEdits {
            goal: Some("add login tests".into()),
            agent: Some(AgentKind::Codex),
            title: Some("Login tests".into()),
            repos: None,
        };
        let r = accept(&k, &p.id, Some(edits)).unwrap();
        let c = r.child.clone().unwrap();
        assert_eq!(
            (c.task.goal.as_str(), c.task.agent, c.task.title.as_str()),
            ("add login tests", AgentKind::Codex, "Login tests")
        );
        assert_eq!(c.task.branch, "mirante/login--1-login-tests");
        assert_eq!((c.task.parent_id.as_deref(), c.task.child_kind), (Some(t.id.as_str()), Some(ChildKind::Spawn)));
        assert!(r.started.is_some() && r.start_error.is_none(), "{r:?}");
        assert!(c.live, "started at once");
        assert_eq!(r.proposal.proposal.status, ProposalStatus::Accepted);
        let decided = k.hub.lock().store.task_events(&t.id).unwrap();
        let d = decided.iter().find(|e| e.kind == "spawn_decided").unwrap();
        assert_eq!(
            (d.payload["edits"]["agent"].as_str(), d.payload["accepted"].as_bool()),
            (Some("codex"), Some(true))
        );
        // The proposal stays as the agent wrote it.
        assert_eq!(k.hub.lock().store.proposal(&p.id).unwrap().goal, "add tests");
        // Decided once; a child cannot be orchestrated.
        assert!(accept(&k, &p.id, None).is_err());
        assert!(k
            .hub
            .orchestrate(&dna::Caller::Person, Orchestrate { task: c.task.id.clone(), enabled: true })
            .is_err());
        // Reject: nothing made.
        let q = propose(&k, &agent.caller(), &t.id, &["refactor"]).unwrap().remove(0).proposal;
        let r = k
            .hub
            .spawn_review(&dna::Caller::Person, SpawnReview { id: q.id, accept: false, edits: None, start: None })
            .unwrap();
        assert_eq!((r.proposal.proposal.status, r.child), (ProposalStatus::Rejected, None));
        let listed = k.hub.spawn_proposals(&dna::Caller::Person, SpawnProposals::default()).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn an_approved_child_starts_from_the_parents_branch() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "base").task;
        std::fs::write(Path::new(&t.worktree_path).join("work.txt"), "w\n").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "w"]] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&t.worktree_path)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        on(&k, &t.id);
        let p = propose(&k, &dna::Caller::Person, &t.id, &["x"]).unwrap().remove(0).proposal;
        let c = k
            .hub
            .spawn_review(&dna::Caller::Person, SpawnReview { id: p.id, accept: true, edits: None, start: Some(false) })
            .unwrap();
        let c = c.child.unwrap();
        assert!(!c.live);
        assert!(Path::new(&c.task.worktree_path).join("work.txt").exists(), "based on the parent's commit");
    }

    #[test]
    fn agent_proposed_children_are_not_by_person_and_inherit_the_origin() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "orig").task;
        on(&k, &t.id);
        let p = propose(&k, &dna::Caller::Person, &t.id, &["x"]).unwrap().remove(0).proposal;
        let c = accept(&k, &p.id, None).unwrap().child.unwrap();
        let mut st = k.hub.lock();
        let (po, co) = (st.store.skill_origin_of(&t.id).unwrap(), st.store.skill_origin_of(&c.task.id).unwrap());
        let (po, co) = (po.unwrap(), co.unwrap());
        assert_eq!((&po.origin, &po.tenant, &po.observed), (&co.origin, &co.tenant, &co.observed));
        // Mining reads only a person's goals: the child's is not one, even approved.
        st.store.skill_set_learning(&po.tenant, true).unwrap();
        let said = st.store.skill_history(&co.tenant, 0).unwrap();
        assert!(said.iter().any(|u| u.task == t.id), "the person's own task is mined: {said:?}");
        assert!(!said.iter().any(|u| u.task == c.task.id), "{said:?}");
    }

    #[test]
    fn children_and_child_result_are_own_children_only_and_agent_view() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "fan").task;
        let other = k.create(AgentKind::Claude, "elsewhere").task;
        on(&k, &t.id);
        let agent = Peer::spawn(&[(AGENT_ENV, &t.id)], k.dir.path());
        let p = propose(&k, &agent.caller(), &t.id, &["x", "y"]).unwrap();
        let c = accept(&k, &p[0].proposal.id, None).unwrap().child.unwrap().task;
        let secret = "token sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let long = format!("done: {secret}\n{}", "z".repeat(40_000));
        k.hub
            .lock()
            .store
            .append_event(Some(&c.id), "step", &json!({"type": "agent_message", "text": long, "final": true}))
            .unwrap();
        let v = k.hub.task_children(&agent.caller(), &t.id).unwrap();
        assert_eq!(v.children.len(), 1);
        assert_eq!(v.proposals.len(), 2);
        let last = v.children[0].last_answer.clone().unwrap();
        assert!(!last.contains("sk-ant-api03-AAAA") && last.ends_with("[truncated]"), "{last}");
        assert_eq!(v.children[0].proposal.as_deref(), Some(p[0].proposal.id.as_str()));
        let r =
            k.hub.child_result(&agent.caller(), ChildResultParams { task: t.id.clone(), child: c.id.clone() }).unwrap();
        let fin = r.final_answer.unwrap();
        assert!(fin.len() <= CHILD_RESULT_BYTES + 20 && !fin.contains("sk-ant-api03-AAAA"));
        assert!(r.diff.is_some());
        // Another task's children, a task that is not a child, a thread: refused.
        forbidden_with(k.hub.task_children(&agent.caller(), &other.id), "another task was named");
        let not_mine = ChildResultParams { task: t.id.clone(), child: other.id.clone() };
        forbidden_with(k.hub.child_result(&agent.caller(), not_mine), "not a child");
        let th = thread_of(&k, &t.id);
        let thread = ChildResultParams { task: t.id.clone(), child: th.task.id.clone() };
        forbidden_with(k.hub.child_result(&agent.caller(), thread), "not a child");
        // The child's own agent cannot read its siblings through its parent.
        let child_agent = Peer::spawn(&[(AGENT_ENV, &c.id)], k.dir.path());
        forbidden_with(k.hub.task_children(&child_agent.caller(), &t.id), "another task was named");
        let e = crate::server::dispatch_from(
            &k.hub,
            &child_agent.caller(),
            method::TASK_SPAWN_PROPOSE,
            json!({"task": c.id, "children": [{"goal": "x"}]}),
        );
        assert_eq!(e.unwrap_err().code, code::CONFLICT, "a child cannot propose");
    }

    /// A parent with an open thread and an approved (started) child.
    fn family(k: &Kit) -> (TaskView, TaskView, TaskView) {
        let p = k.create(AgentKind::Claude, "chat");
        on(k, &p.task.id);
        let th = thread_of(k, &p.task.id);
        let prop = propose(k, &dna::Caller::Person, &p.task.id, &["split the tests"]).unwrap().remove(0).proposal;
        let c = accept(k, &prop.id, None).unwrap().child.unwrap();
        (p, th, c)
    }

    fn lifecycle(k: &Kit, id: &str) -> Lifecycle {
        k.hub.get_task(id).unwrap().task.lifecycle
    }

    fn archive(k: &Kit, task: &str, cascade: bool, stop: bool) -> Result<TaskView> {
        k.hub.archive_with(ArchiveTask { task: task.into(), cascade, stop })
    }

    /// Dogfood finding 2: archiving a parent neither refused nor cascaded.
    #[test]
    fn archiving_a_parent_with_open_children_is_refused_naming_them() {
        let k = Kit::new();
        let (p, th, c) = family(&k);
        let e = k.hub.archive(&p.task.id).unwrap_err();
        let HubError::Conflict(m) = &e else { panic!("{e:?}") };
        assert!(m.contains(&th.task.branch) && m.contains(&c.task.branch), "{m}");
        assert!(m.contains("thread") && m.contains("sub-task") && m.contains("cascade"), "{m}");
        assert!(m.contains("nothing was archived"), "{m}");
        // Over the socket: a conflict, and the plain `{task}` params are the same call.
        let e = crate::server::dispatch(&k.hub, method::TASK_ARCHIVE, json!({"task": p.task.id})).unwrap_err();
        assert_eq!(e.code, code::CONFLICT);
        for id in [&p.task.id, &th.task.id, &c.task.id] {
            assert_ne!(lifecycle(&k, id), Lifecycle::Archived, "nothing archived");
        }
        // An archived child no longer counts.
        k.hub.stop(&c.task.id).unwrap();
        archive(&k, &c.task.id, false, false).unwrap();
        archive(&k, &th.task.id, false, false).unwrap();
        assert_eq!(archive(&k, &p.task.id, false, false).unwrap().task.lifecycle, Lifecycle::Archived);
    }

    #[test]
    fn cascade_archives_the_children_first_then_the_parent() {
        let k = Kit::new();
        let (p, th, c) = family(&k);
        // The child is live: refused without `stop`, nothing archived.
        assert!(k.hub.get_task(&c.task.id).unwrap().live);
        let e = archive(&k, &p.task.id, true, false).unwrap_err();
        let HubError::Conflict(m) = &e else { panic!("{e:?}") };
        assert!(m.contains(&c.task.branch) && m.contains("nothing was archived"), "{m}");
        assert_ne!(lifecycle(&k, &th.task.id), Lifecycle::Archived);
        // With `stop` over the socket: every session ended, all archived, branches kept.
        let v = crate::server::dispatch(
            &k.hub,
            method::TASK_ARCHIVE,
            json!({"task": p.task.id, "cascade": true, "stop": true}),
        )
        .unwrap();
        assert_eq!(v["lifecycle"], "archived");
        for id in [&p.task.id, &th.task.id, &c.task.id] {
            assert_eq!(lifecycle(&k, id), Lifecycle::Archived);
            assert!(!k.hub.get_task(id).unwrap().live);
        }
        let seq = |id: &str| {
            k.hub
                .lock()
                .store
                .task_events(id)
                .unwrap()
                .into_iter()
                .find(|e| e.kind == "lifecycle" && e.payload["to"] == "archived")
                .map(|e| e.seq)
        };
        let (ps, ts, cs) = (seq(&p.task.id).unwrap(), seq(&th.task.id).unwrap(), seq(&c.task.id).unwrap());
        assert!(ts < ps && cs < ps, "children first");
    }

    #[test]
    fn a_dirty_child_refuses_the_cascade_and_leaves_everything_untouched() {
        let k = Kit::new();
        let (p, th, c) = family(&k);
        std::fs::write(Path::new(&th.task.worktree_path).join("wip.txt"), "x").unwrap();
        let e = archive(&k, &p.task.id, true, true).unwrap_err();
        let HubError::Conflict(m) = &e else { panic!("{e:?}") };
        assert!(m.contains(&th.task.branch) && m.contains("uncommitted") && m.contains("nothing was archived"), "{m}");
        assert!(!m.contains(&c.task.branch), "{m}");
        for id in [&p.task.id, &th.task.id, &c.task.id] {
            assert_ne!(lifecycle(&k, id), Lifecycle::Archived, "nothing archived");
        }
        assert!(k.hub.get_task(&c.task.id).unwrap().live, "nothing stopped either");
        // A dirty parent refuses the same way.
        std::fs::remove_file(Path::new(&th.task.worktree_path).join("wip.txt")).unwrap();
        std::fs::write(Path::new(&p.task.worktree_path).join("wip.txt"), "x").unwrap();
        let e = archive(&k, &p.task.id, true, true).unwrap_err();
        assert!(e.to_string().contains(&p.task.branch) && e.to_string().contains("nothing was archived"), "{e}");
        assert_ne!(lifecycle(&k, &th.task.id), Lifecycle::Archived);
    }

    #[test]
    fn archiving_a_parent_withdraws_what_waits() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        on(&k, &t.id);
        let p = propose(&k, &dna::Caller::Person, &t.id, &["x"]).unwrap().remove(0).proposal;
        k.hub.archive(&t.id).unwrap();
        assert_eq!(k.hub.lock().store.proposal(&p.id).unwrap().status, ProposalStatus::Withdrawn);
        assert!(accept(&k, &p.id, None).is_err());
    }
}
