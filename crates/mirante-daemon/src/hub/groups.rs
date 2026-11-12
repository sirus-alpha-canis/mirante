//! Fan-out groups (founder decision 2): `group.create`, children through `task.create
//! { group }`, `group.get` (each task's last answer and diff stat), `group.archive` (all or
//! nothing) and `group.synthesize`. A group owns no worktree and no session; every child is
//! an ordinary task with its own worktree, agent session and permissions (manual mode, a
//! person answers). Nothing here approves a permission or sends one agent's text to another
//! on its own: a synthesis is a person's request, previewed first, with the agents' answers
//! fenced as data.
use std::path::Path;

use mirante_core::groups::{
    child_branch, marker_for, results_hash, synth_branch, synthesis_prompt, NewGroup, Sibling, TaskGroup,
};
use mirante_core::model::{GroupKind, GroupRole, Lifecycle, Task};
use mirante_core::CoreError;
use mirante_ipc::types::{
    CreateGroup, CreateTask, GroupMember, GroupView, SendMessage, SendMode, Synthesis, Synthesize, TaskView,
};

use super::{dna, title_from_goal, Hub, HubError, Result, State};

/// Who sent a synthesis request (its `message_sent` / `message_queued` `by`): it reaches the
/// agent behind the operator label (a person asked for it), but mining never reads it as the
/// person's words (it quotes agents).
pub(crate) const BY_SYNTHESIS: &str = "synthesis";
/// Longest shared goal a group keeps (UTF-8 bytes).
pub const MAX_GROUP_GOAL: usize = 16 * 1024;
/// How many child or synthesizer branch names are tried when git already has one.
const MAX_RENAMES: usize = 20;

/// A task being created into a group.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub(crate) group: String,
    pub(crate) role: GroupRole,
    /// Its goal is the person's own words (a child), not agents' answers (a synthesizer).
    pub(crate) by_person: bool,
}

/// git already has this branch (a base was given, so nothing was checked out), or its
/// worktree folder exists.
fn branch_taken(e: &HubError) -> bool {
    match e {
        HubError::Core(CoreError::InvalidBranch { reason, .. }) => reason.starts_with("branch already exists"),
        HubError::Core(CoreError::Git { args, stderr }) => args == "worktree add" && stderr.ends_with("already exists"),
        _ => false,
    }
}

/// The last task of `group` was archived: the group is archived too (best effort).
pub(crate) fn archive_when_done(st: &mut State, group: &str) {
    let done = st.store.get_group(group).is_ok_and(|g| g.archived_at.is_none())
        && st.store.group_members(group).is_ok_and(|m| m.iter().all(|t| t.lifecycle == Lifecycle::Archived));
    if done {
        if let Err(e) = st.store.archive_group(group) {
            st.log_error(None, "archiving a group with its last task", &e);
        }
    }
}

impl Hub {
    /// A person's fan-out group. Its children are created with `task.create { group }`.
    pub fn create_group(&self, p: CreateGroup) -> Result<TaskGroup> {
        let goal = p.goal.trim_end().to_string();
        if goal.trim().is_empty() {
            return Err(HubError::Invalid("the goal is empty".into()));
        }
        if goal.len() > MAX_GROUP_GOAL {
            return Err(HubError::Invalid(format!("the shared goal is over {MAX_GROUP_GOAL} bytes")));
        }
        mirante_core::worktree::validate_branch_name(&p.branch)?;
        // The children's names must be valid too (`<branch>--1-x`).
        mirante_core::worktree::validate_branch_name(&child_branch(&p.branch, 1, "x"))?;
        let title = p.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_from_goal(&goal));
        let mut st = self.lock();
        let g = st.store.create_group(NewGroup { kind: p.kind, title, goal, branch: p.branch })?;
        st.publish();
        Ok(g)
    }

    /// `task.create { group }`: a child of a group. With no branch, the daemon names it
    /// `<group branch>--<n>-<slug>` (the agent's name in a comparison, else the child's
    /// title or goal) and starts it from the repo's `HEAD`, moving past names git has.
    pub(crate) fn create_group_child(&self, caller: &dna::Caller, mut p: CreateTask) -> Result<TaskView> {
        let gid = p.group.take().unwrap_or_default();
        let (group, made) = {
            let st = self.lock();
            let g = st.store.get_group(&gid)?;
            if g.archived_at.is_some() {
                return Err(HubError::Conflict(format!("the group \"{}\" is archived", g.title)));
            }
            let made = st.store.group_members(&gid)?.iter().filter(|t| t.group_role == Some(GroupRole::Child)).count();
            (g, made)
        };
        let member = Member { group: group.id.clone(), role: GroupRole::Child, by_person: true };
        if !p.branch.trim().is_empty() {
            return self.create_in(caller, p, Some(member));
        }
        let label = match group.kind {
            GroupKind::Compare => p.agent.as_str().to_string(),
            GroupKind::Fanout => p.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| p.goal.clone()),
        };
        p.base = Some(p.base.unwrap_or_else(|| "HEAD".into()));
        let mut n = made + 1;
        loop {
            p.branch = child_branch(&group.branch, n, &label);
            match self.create_in(caller, p.clone(), Some(member.clone())) {
                Err(e) if branch_taken(&e) && n < made + MAX_RENAMES => n += 1,
                other => return other,
            }
        }
    }

    /// The group and its tasks, each with its last answer and diff stat (git runs without
    /// the hub lock).
    pub fn group_get(&self, id: &str) -> Result<GroupView> {
        let (group, members) = {
            let st = self.lock();
            let g = st.store.get_group(id)?;
            let mut out = Vec::new();
            for t in st.store.group_members(id)? {
                let last_answer = st.store.last_answer(&t.id)?;
                out.push((st.view(t)?, last_answer));
            }
            (g, out)
        };
        let members = members
            .into_iter()
            .map(|(view, last_answer)| {
                let diff = (view.task.lifecycle != Lifecycle::Archived)
                    .then(|| match view.task.workspace_path {
                        Some(_) => crate::diff::workspace_stat(&view.repos),
                        None => {
                            crate::diff::diff_stat(Path::new(&view.task.repo_path), Path::new(&view.task.worktree_path))
                        }
                    })
                    .and_then(|r| r.ok());
                GroupMember { view, last_answer, diff }
            })
            .collect();
        Ok(GroupView { group, members })
    }

    /// Archives every task of the group, then the group. Checked first, before anything is
    /// archived: none may be live or being archived, and none may hold uncommitted work
    /// (each is named). Branches are kept.
    pub fn group_archive(&self, id: &str) -> Result<GroupView> {
        let open: Vec<Task> = {
            let st = self.lock();
            let g = st.store.get_group(id)?;
            if g.archived_at.is_some() {
                return Err(HubError::Conflict(format!("the group \"{}\" is already archived", g.title)));
            }
            let open: Vec<Task> =
                st.store.group_members(id)?.into_iter().filter(|t| t.lifecycle != Lifecycle::Archived).collect();
            let live: Vec<&str> =
                open.iter().filter(|t| st.live.contains_key(&t.id)).map(|t| t.branch.as_str()).collect();
            if !live.is_empty() {
                return Err(HubError::Conflict(format!(
                    "stop these tasks first (nothing was archived): {}",
                    live.join(", ")
                )));
            }
            if let Some(t) = open.iter().find(|t| t.lifecycle == Lifecycle::Archiving) {
                return Err(HubError::Conflict(format!("task {} is being archived", t.branch)));
            }
            open
        };
        let mut dirty = Vec::new();
        for t in &open {
            let clean = match &t.workspace_path {
                Some(_) => {
                    let repos = self.lock().store.task_repos(&t.id)?;
                    repos.iter().try_for_each(|r| self.worktrees.check_clean(Path::new(&r.worktree_path)))
                }
                None => self.worktrees.check_clean(Path::new(&t.worktree_path)),
            };
            match clean {
                Ok(()) => {}
                Err(CoreError::DirtyWorktree { .. }) => dirty.push(t.branch.clone()),
                Err(e) => return Err(e.into()),
            }
        }
        if !dirty.is_empty() {
            return Err(HubError::Conflict(format!(
                "these tasks hold uncommitted work (nothing was archived; commit or discard it first): {}",
                dirty.join(", ")
            )));
        }
        for t in &open {
            self.archive(&t.id).map_err(|e| {
                HubError::Conflict(format!("archiving {} failed: {e} (the tasks before it are archived)", t.branch))
            })?;
        }
        {
            let mut st = self.lock();
            if st.store.get_group(id)?.archived_at.is_none() {
                st.store.archive_group(id)?;
            }
            st.publish();
        }
        self.group_get(id)
    }

    /// `group.synthesize`: the request reading the children's results. A preview returns
    /// it with its `expect`; the call that acts must pass that `expect` back, and is refused
    /// when any answer or status changed meanwhile. Into a child: a queued operator message
    /// (the child's own answer is left out). Else: a new task in the group (role `synth`,
    /// not started) whose goal is the request, on the first child's repo or folder.
    pub fn group_synthesize(&self, p: Synthesize) -> Result<Synthesis> {
        let (group, into, siblings, first) = {
            let st = self.lock();
            let g = st.store.get_group(&p.group)?;
            if g.archived_at.is_some() {
                return Err(HubError::Conflict(format!("the group \"{}\" is archived", g.title)));
            }
            let children: Vec<Task> = st
                .store
                .group_members(&g.id)?
                .into_iter()
                .filter(|t| t.group_role == Some(GroupRole::Child) && t.lifecycle != Lifecycle::Archived)
                .collect();
            let into = match &p.into {
                Some(r) => {
                    let t = st.resolve(r)?;
                    if !children.iter().any(|c| c.id == t.id) {
                        return Err(HubError::Invalid(format!("task {} is not a child of this group", t.branch)));
                    }
                    Some(t)
                }
                None => None,
            };
            let mut siblings = Vec::new();
            for c in children.iter().filter(|c| into.as_ref().is_none_or(|i| i.id != c.id)) {
                siblings.push(Sibling {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    agent: c.agent,
                    branch: c.branch.clone(),
                    worktree: c.cwd().to_string(),
                    status: c.status,
                    lifecycle: c.lifecycle,
                    answer: st.store.last_answer(&c.id)?,
                });
            }
            (g, into, siblings, children.into_iter().next())
        };
        if siblings.is_empty() {
            return Err(HubError::Invalid("there is no other child whose results could be read".into()));
        }
        let expect = results_hash(&group, into.as_ref().map(|t| t.id.as_str()), &siblings);
        let marker = marker_for(siblings.iter().filter_map(|s| s.answer.as_deref()).chain([group.goal.as_str()]));
        let text = synthesis_prompt(&group, &siblings, into.is_some(), &marker);
        if p.preview {
            return Ok(Synthesis { text, expect, task: None, sent: None });
        }
        match &p.expect {
            None => return Err(HubError::Invalid("preview the synthesis first and pass back its expect".into())),
            Some(e) if *e != expect => {
                return Err(HubError::Conflict(
                    "the children's results changed since the preview: review the new text".into(),
                ))
            }
            Some(_) => {}
        }
        if let Some(child) = into {
            let sent =
                self.send_by(SendMessage { task: child.id, text: text.clone(), mode: SendMode::Queue }, BY_SYNTHESIS)?;
            return Ok(Synthesis { text, expect, task: None, sent: Some(sent) });
        }
        let first = first.expect("siblings are children");
        let (repo, repos) = match &first.group_path {
            Some(folder) => {
                let repos = self.lock().store.task_repos(&first.id)?;
                (folder.clone(), repos.into_iter().map(|r| r.repo_path).collect())
            }
            None => (first.repo_path.clone(), Vec::new()),
        };
        let member = Member { group: group.id.clone(), role: GroupRole::Synth, by_person: false };
        let title: String = format!("Synthesize: {}", group.title).chars().take(60).collect();
        let base = synth_branch(&group.branch);
        let mut branch = base.clone();
        for n in 2.. {
            let p = CreateTask {
                repo: repo.clone(),
                repos: repos.clone(),
                agent: p.agent.unwrap_or(first.agent),
                branch: branch.clone(),
                base: Some("HEAD".into()),
                goal: text.clone(),
                title: Some(title.clone()),
                group: None,
            };
            match self.create_in(&dna::Caller::Person, p, Some(member.clone())) {
                Err(e) if branch_taken(&e) && n < MAX_RENAMES => branch = format!("{base}-{n}"),
                Err(e) => return Err(e),
                Ok(v) => return Ok(Synthesis { text, expect, task: Some(v), sent: None }),
            }
        }
        unreachable!("the loop returns")
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use mirante_core::model::{AgentKind, GroupKind, GroupRole, Lifecycle, Status};
    use mirante_ipc::rpc::{code, RpcError};
    use mirante_ipc::types::{CreateGroup, CreateTask, StartSession, Synthesize};
    use serde_json::json;

    use super::super::testkit::Kit;
    use super::*;

    fn group(k: &Kit, kind: GroupKind) -> TaskGroup {
        k.hub
            .create_group(CreateGroup {
                kind,
                goal: "fix login".into(),
                title: None,
                branch: "mirante/fix-login".into(),
            })
            .unwrap()
    }

    fn child(k: &Kit, g: &TaskGroup, agent: AgentKind, goal: &str) -> Result<TaskView> {
        k.hub.create_task(CreateTask {
            group: Some(g.id.clone()),
            repo: k.repo.to_string_lossy().into_owned(),
            repos: Vec::new(),
            agent,
            branch: String::new(),
            base: None,
            goal: goal.into(),
            title: None,
        })
    }

    fn answer(k: &Kit, task: &str, text: &str) {
        k.hub
            .lock()
            .store
            .append_event(Some(task), "step", &json!({"type": "agent_message", "text": text, "final": true}))
            .unwrap();
    }

    #[test]
    fn a_compare_groups_children_are_named_after_their_agent_and_start_at_head() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        assert_eq!(g.title, "fix login");
        let a = child(&k, &g, AgentKind::Claude, "fix login").unwrap();
        let b = child(&k, &g, AgentKind::Codex, "fix login").unwrap();
        assert_eq!(a.task.branch, "mirante/fix-login--1-claude");
        assert_eq!(b.task.branch, "mirante/fix-login--2-codex");
        assert_eq!(a.task.group_role, Some(GroupRole::Child));
        assert_eq!(a.group.as_ref().map(|x| x.id.as_str()), Some(g.id.as_str()));
        assert!(a.origin.is_some(), "a person's child fixes its origin like any task");
        assert!(k.kinds(&a.task.id).contains(&"group_child_added".to_string()));
        // Listed with its group.
        assert!(k.hub.list_tasks(false).unwrap().iter().all(|v| v.group.is_some()));
    }

    #[test]
    fn a_taken_child_branch_moves_on_and_a_fanout_child_is_named_after_its_goal() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Fanout);
        let git =
            |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&k.repo).args(args).status().unwrap().success());
        git(&["branch", "mirante/fix-login--1-add-tests"]);
        let a = child(&k, &g, AgentKind::Claude, "add tests").unwrap();
        assert_eq!(a.task.branch, "mirante/fix-login--2-add-tests");
    }

    #[test]
    fn a_child_can_target_a_folder_of_repos_and_a_synthesizer_follows_it() {
        let k = Kit::new();
        let folder = k.dir.path().join("gol");
        for name in ["site", "bff"] {
            let r = folder.join(name);
            std::fs::create_dir_all(&r).unwrap();
            let git = |args: &[&str]| {
                let ok = Command::new("git")
                    .arg("-C")
                    .arg(&r)
                    .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
                    .args(args)
                    .status()
                    .unwrap();
                assert!(ok.success());
            };
            git(&["init", "-q", "-b", "main"]);
            std::fs::write(r.join("README.md"), "x\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-q", "-m", "init"]);
        }
        let folder = std::fs::canonicalize(folder).unwrap();
        let g = group(&k, GroupKind::Compare);
        let mk = |agent| {
            k.hub
                .create_task(CreateTask {
                    group: Some(g.id.clone()),
                    repo: folder.to_string_lossy().into_owned(),
                    repos: vec![
                        folder.join("site").to_string_lossy().into_owned(),
                        folder.join("bff").to_string_lossy().into_owned(),
                    ],
                    agent,
                    branch: String::new(),
                    base: None,
                    goal: "fix login".into(),
                    title: None,
                })
                .unwrap()
        };
        let a = mk(AgentKind::Claude);
        let b = mk(AgentKind::Codex);
        assert_eq!(a.repos.len(), 2);
        assert_eq!(a.repos[1].branch, "mirante/fix-login--1-claude");
        assert_eq!(b.task.branch, "mirante/fix-login--2-codex");
        let v = k.hub.group_get(&g.id).unwrap();
        assert!(v.members.iter().all(|m| m.diff.is_some()), "a workspace child's stat sums its repos");
        let pv = k
            .hub
            .group_synthesize(Synthesize { group: g.id.clone(), into: None, agent: None, preview: true, expect: None })
            .unwrap();
        let s = k
            .hub
            .group_synthesize(Synthesize {
                group: g.id.clone(),
                into: None,
                agent: None,
                preview: false,
                expect: Some(pv.expect),
            })
            .unwrap()
            .task
            .unwrap();
        assert_eq!(s.task.group_path.as_deref(), Some(folder.to_str().unwrap()));
        assert_eq!(s.repos.len(), 2);
        assert_eq!(s.task.agent, AgentKind::Claude, "the first child's agent by default");
    }

    #[test]
    fn the_ninth_child_is_refused_with_why() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        for _ in 0..8 {
            child(&k, &g, AgentKind::Codex, "x").unwrap();
        }
        let e = child(&k, &g, AgentKind::Codex, "x").unwrap_err();
        assert!(RpcError::from(e).message.contains("already has 8 children"));
    }

    #[test]
    fn a_bad_group_is_refused_before_any_worktree() {
        let k = Kit::new();
        let mut p = CreateGroup { kind: GroupKind::Compare, goal: " ".into(), title: None, branch: "mirante/x".into() };
        assert!(matches!(k.hub.create_group(p.clone()), Err(HubError::Invalid(_))));
        p.goal = "go".into();
        p.branch = "bad..name".into();
        assert!(k.hub.create_group(p).is_err());
        let e = k
            .hub
            .create_task(CreateTask {
                group: Some("nope".into()),
                repo: k.repo.to_string_lossy().into_owned(),
                repos: Vec::new(),
                agent: AgentKind::Claude,
                branch: String::new(),
                base: None,
                goal: "x".into(),
                title: None,
            })
            .unwrap_err();
        assert_eq!(RpcError::from(e).code, code::NOT_FOUND);
    }

    #[test]
    fn group_get_has_each_tasks_last_answer_and_diff_stat() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        let a = child(&k, &g, AgentKind::Claude, "fix login").unwrap();
        let b = child(&k, &g, AgentKind::Codex, "fix login").unwrap();
        answer(&k, &a.task.id, "Fixed the form.");
        std::fs::write(Path::new(&b.task.worktree_path).join("README"), "hi\nthere\n").unwrap();
        std::fs::write(Path::new(&b.task.worktree_path).join("new.txt"), "x\n").unwrap();
        let v = k.hub.group_get(&g.id).unwrap();
        assert_eq!(v.group, g);
        assert_eq!(v.members.len(), 2);
        assert_eq!(v.members[0].last_answer.as_deref(), Some("Fixed the form."));
        let d = v.members[1].diff.unwrap();
        assert_eq!((d.files, d.added, d.removed, d.untracked), (1, 1, 0, 1));
        assert_eq!(v.members[0].diff.unwrap().files, 0);
    }

    #[test]
    fn group_archive_checks_every_task_first_then_archives_them_and_the_group() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        let a = child(&k, &g, AgentKind::Claude, "x").unwrap();
        let b = child(&k, &g, AgentKind::Codex, "x").unwrap();
        // Live: refused, nothing archived.
        k.hub.start_session(StartSession { task: a.task.id.clone(), prompt: None }, false).unwrap();
        let e = k.hub.group_archive(&g.id).unwrap_err();
        assert!(e.to_string().contains(&a.task.branch) && e.to_string().contains("nothing was archived"), "{e}");
        k.hub.stop(&a.task.id).unwrap();
        // Dirty: refused naming it, nothing archived.
        std::fs::write(Path::new(&b.task.worktree_path).join("wip.txt"), "x").unwrap();
        let e = k.hub.group_archive(&g.id).unwrap_err();
        assert!(e.to_string().contains(&b.task.branch) && !e.to_string().contains(&a.task.branch), "{e}");
        assert_eq!(k.hub.get_task(&a.task.id).unwrap().task.lifecycle, Lifecycle::Active);
        std::fs::remove_file(Path::new(&b.task.worktree_path).join("wip.txt")).unwrap();
        let v = k.hub.group_archive(&g.id).unwrap();
        assert!(v.group.archived_at.is_some());
        assert!(v.members.iter().all(|m| m.view.task.lifecycle == Lifecycle::Archived));
        assert!(k.hub.list_tasks(false).unwrap().is_empty());
        assert!(child(&k, &g, AgentKind::Claude, "x").is_err(), "an archived group takes no children");
    }

    #[test]
    fn archiving_the_last_task_one_by_one_archives_the_group() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        let a = child(&k, &g, AgentKind::Claude, "x").unwrap();
        let b = child(&k, &g, AgentKind::Codex, "x").unwrap();
        k.hub.archive(&a.task.id).unwrap();
        assert!(k.hub.group_get(&g.id).unwrap().group.archived_at.is_none());
        k.hub.archive(&b.task.id).unwrap();
        assert!(k.hub.group_get(&g.id).unwrap().group.archived_at.is_some());
    }

    #[test]
    fn synthesize_previews_then_creates_a_synth_task_that_is_not_the_persons_words() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        let a = child(&k, &g, AgentKind::Claude, "x").unwrap();
        child(&k, &g, AgentKind::Codex, "x").unwrap();
        answer(&k, &a.task.id, "Claude: done, see branch.");
        let req = |preview: bool, expect: Option<String>| Synthesize {
            group: g.id.clone(),
            into: None,
            agent: Some(AgentKind::Claude),
            preview,
            expect,
        };
        let pv = k.hub.group_synthesize(req(true, None)).unwrap();
        assert!(pv.task.is_none() && pv.text.contains("Claude: done, see branch."), "{}", pv.text);
        assert!(pv.text.contains("mirante/fix-login--2-codex") && pv.text.contains("(no answer yet)"));
        // Acting needs the preview's hash, and a fresh one.
        assert!(matches!(k.hub.group_synthesize(req(false, None)), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.group_synthesize(req(false, Some("stale".into()))), Err(HubError::Conflict(_))));
        let made = k.hub.group_synthesize(req(false, Some(pv.expect.clone()))).unwrap();
        let t = made.task.unwrap();
        assert_eq!((t.task.branch.as_str(), t.task.group_role), ("mirante/fix-login--synth", Some(GroupRole::Synth)));
        assert!(t.task.goal.contains("Claude: done, see branch.") && t.task.title.starts_with("Synthesize: "));
        assert_eq!(t.task.status, Status::Idle);
        // Agents' text: not the person's words (no mining; its origin stays unconfirmed).
        let o = k.hub.lock().store.skill_origin_of(&t.task.id).unwrap().unwrap();
        assert!(o.observed.is_some(), "{o:?}");
        // A second synthesizer moves on to -2; it never counts as a child.
        let pv = k.hub.group_synthesize(req(true, None)).unwrap();
        let again = k.hub.group_synthesize(req(false, Some(pv.expect))).unwrap().task.unwrap();
        assert_eq!(again.task.branch, "mirante/fix-login--synth-2");
    }

    #[test]
    fn synthesize_into_a_child_queues_an_operator_message_without_its_own_answer() {
        let k = Kit::new();
        let g = group(&k, GroupKind::Compare);
        let a = child(&k, &g, AgentKind::Codex, "x").unwrap();
        let b = child(&k, &g, AgentKind::Codex, "x").unwrap();
        answer(&k, &a.task.id, "answer of a");
        answer(&k, &b.task.id, "answer of b");
        let req = |preview: bool, expect: Option<String>| Synthesize {
            group: g.id.clone(),
            into: Some(a.task.id.clone()),
            agent: None,
            preview,
            expect,
        };
        let pv = k.hub.group_synthesize(req(true, None)).unwrap();
        assert!(pv.text.contains("answer of b") && !pv.text.contains("answer of a"), "{}", pv.text);
        // Not live: says how to resume, nothing queued.
        assert!(k.hub.group_synthesize(req(false, Some(pv.expect.clone()))).is_err());
        k.hub.start_session(StartSession { task: a.task.id.clone(), prompt: None }, false).unwrap();
        let sent = k.hub.group_synthesize(req(false, Some(pv.expect))).unwrap();
        assert!(sent.sent.is_some());
        let ev = k.hub.lock().store.task_events(&a.task.id).unwrap();
        let msg = ev.iter().rev().find(|e| e.kind.starts_with("message_")).unwrap();
        assert_eq!(msg.payload["by"], "synthesis", "not mined as the person's words");
        // A task outside the group is refused.
        let other = k.create(AgentKind::Codex, "feat/other");
        let mut r = req(true, None);
        r.into = Some(other.task.id);
        assert!(matches!(k.hub.group_synthesize(r), Err(HubError::Invalid(_))));
    }
}
