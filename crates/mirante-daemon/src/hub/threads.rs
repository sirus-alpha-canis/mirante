//! Threads (Batch D): `task.create { link, parent }`, `task.thread` (the person's notes and
//! link), delivery of the thread block into the conversation, and `task.summarize`.
//!
//! A thread is an ordinary task (own worktree, agent session, permissions; manual mode, a
//! person answers) bound to one object. Its block (link + the person's notes, fenced:
//! [`mirante_core::threads::thread_block`]) reaches its agent with a person's delivery, never
//! the system prompt: the first prompt of a fresh conversation, the first message after a
//! resume, and the next message after the notes or link changed. The queue and the event
//! log keep the person's own words; the block is added when the message is handed over.
use std::path::Path;

use mirante_core::delivery::{label_operator, strip_operator_label, OPERATOR_LABEL};
use mirante_core::groups::child_branch;
use mirante_core::model::Lifecycle;
use mirante_core::threads::{
    check_link, check_notes, deterministic_summary, strip_thread_block, thread_block, NewThread, SummaryBy,
};
use mirante_ipc::types::{CreateThread, Summarize, SummarizeWith, SummaryView, TaskView, ThreadEdit, ThreadView};
use serde_json::json;

use super::{dna, groups::branch_taken, Hub, HubError, Placement, Result, State};

/// How many thread branch names are tried when git already has one.
const MAX_RENAMES: usize = 20;

/// A thread block owed to the agent with the next delivery.
#[derive(Debug, Clone)]
pub(crate) struct Owed {
    pub(crate) block: String,
    pub(crate) hash: String,
    pub(crate) at: &'static str,
}

/// A slash command must be the first thing the agent reads: it never carries the block
/// (the block stays owed to the next message).
pub(crate) fn is_command(text: &str) -> bool {
    text.trim_start().starts_with('/')
}

/// What the agent is handed: the block, then the text; behind the operator label when a
/// person sent it (the label stays the first thing on the line).
pub(crate) fn with_block(block: &str, text: &str, operator: bool) -> String {
    let joined = format!("{block}{text}");
    if operator {
        label_operator(&joined)
    } else {
        joined
    }
}

/// The sender's own words in what the agent was handed (no label, no thread block).
pub(crate) fn plain_words(wire: &str) -> &str {
    strip_thread_block(strip_operator_label(wire))
}

/// Whether the agent was handed `wire` as the person's (behind the operator label).
pub(crate) fn labelled(wire: &str) -> bool {
    wire.starts_with(OPERATOR_LABEL)
}

impl State {
    /// The thread's view (`None` for a task that is no thread).
    pub(crate) fn thread_view(&self, task_id: &str) -> Result<Option<ThreadView>> {
        Ok(self.store.thread_state(task_id)?.map(|s| ThreadView {
            delivered: s.sent_hash.as_deref() == Some(s.notes_hash.as_str()),
            link: s.link,
            notes_hash: s.notes_hash,
            summary: s.summary,
            summary_at: s.summary_at,
            summary_by: s.summary_by,
        }))
    }

    /// The block owed to `task_id`'s agent with its next delivery, if any: owed at a fresh
    /// start or a resume (until a delivery carries it), and whenever the notes or link
    /// differ from what was last handed over.
    pub(crate) fn thread_owed(&self, task_id: &str) -> Result<Option<Owed>> {
        let Some(th) = self.store.thread_state(task_id)? else {
            return Ok(None);
        };
        let at = match self.live.get(task_id).and_then(|l| l.thread_due) {
            Some(at) => at,
            None if th.sent_hash.as_deref() != Some(th.notes_hash.as_str()) => "changed",
            None => return Ok(None),
        };
        let notes = self.store.get_task(task_id)?.notes;
        Ok(Some(Owed { block: thread_block(&th.link, &notes), hash: th.notes_hash, at }))
    }

    /// The agent was handed `owed`: `thread_delivered`, and nothing is owed any more.
    pub(crate) fn thread_handed(&mut self, task_id: &str, owed: &Owed) -> Result<()> {
        if let Some(l) = self.live.get_mut(task_id) {
            l.thread_due = None;
            l.thread_last = Some(owed.at);
        }
        Ok(self.store.mark_thread_delivered(task_id, &owed.hash, owed.at)?)
    }

    /// A delivery that carried the block was handed back: it is owed again.
    pub(crate) fn thread_handed_back(&mut self, task_id: &str) {
        if let Some(l) = self.live.get_mut(task_id) {
            l.thread_due = l.thread_last.or(Some("changed"));
        }
    }

    /// A thread's agent finished an answer: its deterministic summary follows (no model).
    /// Best effort; a task that is no thread is left alone.
    pub(crate) fn refresh_summary(&mut self, task_id: &str, text: &str) {
        match self.store.thread_state(task_id) {
            Ok(Some(_)) => {}
            _ => return,
        }
        let summary = deterministic_summary(None, Some(text), None);
        if let Err(e) = self.store.set_summary(task_id, &summary, SummaryBy::Deterministic, json!({})) {
            self.log_error(Some(task_id), "thread summary", &e);
        }
    }
}

impl Hub {
    /// `task.create { link }`: a thread. Its notes are checked (redacted, at most 8 KiB),
    /// its parent is the conversation it came from (a thread's own parent when it is a
    /// thread: one level deep), and an open thread under the same parent bound to the same
    /// object is a conflict naming it (continue there). With a parent and no branch, the
    /// thread is named `<parent branch>--<n>-<slug>` and starts from the parent's branch.
    pub fn create_thread(&self, caller: &dna::Caller, c: CreateThread) -> Result<TaskView> {
        let CreateThread { task: mut p, link, parent, notes, moved } = c;
        let Some(link) = link else {
            return Err(HubError::Invalid("a thread needs a link (a PR, an issue, a branch or a message)".into()));
        };
        let link = check_link(&link).map_err(HubError::Invalid)?;
        let notes = check_notes(notes.as_deref().unwrap_or("")).map_err(HubError::Invalid)?;
        if p.group.is_some() {
            return Err(HubError::Invalid("a thread is not part of a fan-out group".into()));
        }
        if moved.is_some() && parent.is_none() {
            return Err(HubError::Invalid("a thread made from a message needs that message's task as parent".into()));
        }
        let (parent, made, repos) = {
            let st = self.lock();
            let parent = match parent.as_deref() {
                Some(r) => {
                    let t = st.resolve(r)?;
                    // One level deep: a thread of a thread belongs to the same conversation.
                    let t = match &t.parent_id {
                        Some(pp) => st.store.get_task(pp)?,
                        None => t,
                    };
                    if t.lifecycle == Lifecycle::Archived || t.lifecycle == Lifecycle::Archiving {
                        return Err(HubError::Conflict(format!("task {} is archived", t.branch)));
                    }
                    Some(t)
                }
                None => None,
            };
            if let Some(twin) = st.store.open_thread(parent.as_ref().map(|t| t.id.as_str()), &link)? {
                return Err(HubError::Conflict(format!(
                    "an open thread is already bound to {} {}: \"{}\" ({}, id {}); continue there",
                    link.kind.as_str(),
                    link.reference,
                    twin.title,
                    twin.branch,
                    twin.id
                )));
            }
            let made = match &parent {
                Some(t) => st.store.threads_of(&t.id)?.len(),
                None => 0,
            };
            let repos = match &parent {
                Some(t) if t.workspace_path.is_some() => st.store.task_repos(&t.id)?,
                _ => Vec::new(),
            };
            (parent, made, repos)
        };
        if let Some(pt) = &parent {
            if p.repo.trim().is_empty() {
                p.repo = pt.group_path.clone().unwrap_or_else(|| pt.repo_path.clone());
                if p.repos.is_empty() {
                    p.repos = repos.iter().map(|r| r.repo_path.clone()).collect();
                }
            }
        }
        let thread = NewThread { parent: parent.as_ref().map(|t| t.id.clone()), link: link.clone(), notes, moved };
        if !p.branch.trim().is_empty() {
            return self.create_in(caller, p, Placement::Thread(thread));
        }
        let Some(pt) = parent else {
            return Err(HubError::Invalid("name a branch for this thread (or start it from a task)".into()));
        };
        // From the parent's work: its branch (a workspace parent: the same repos at HEAD).
        p.base =
            Some(p.base.unwrap_or_else(|| if pt.workspace_path.is_some() { "HEAD".into() } else { pt.branch.clone() }));
        let label = link.title.clone().unwrap_or_else(|| format!("{} {}", link.kind.as_str(), link.reference));
        let mut n = made + 1;
        loop {
            p.branch = child_branch(&pt.branch, n, &label);
            match self.create_in(caller, p.clone(), Placement::Thread(thread.clone())) {
                Err(e) if branch_taken(&e) && n < made + MAX_RENAMES => n += 1,
                other => return other,
            }
        }
    }

    /// `task.thread`: the person changes a thread's notes and/or link against the hash they
    /// edited. The next message the person sends carries the change, once.
    pub fn thread_edit(&self, e: ThreadEdit) -> Result<TaskView> {
        let notes = match e.notes.as_deref() {
            Some(n) => Some(check_notes(n).map_err(HubError::Invalid)?),
            None => None,
        };
        let link = match &e.link {
            Some(l) => Some(check_link(l).map_err(HubError::Invalid)?),
            None => None,
        };
        let mut st = self.lock();
        let task = st.resolve(&e.task)?;
        if st.store.thread_state(&task.id)?.is_none() {
            return Err(HubError::Invalid(format!("task {} is not a thread", task.branch)));
        }
        let (task, _) = st.store.set_thread(&task.id, notes.as_deref(), link.as_ref(), &e.expect_hash)?;
        let v = st.view(task)?;
        st.publish();
        Ok(v)
    }

    /// `task.summarize`: without `with`, the deterministic summary now (status, last
    /// answer, diff stat; git runs without the hub lock). `with: claude`: the person's own
    /// CLI ([`super::summary`]), refused unless the setting allows it and the tool-less
    /// probe passed for the installed version.
    pub fn summarize(&self, s: Summarize) -> Result<SummaryView> {
        let (task, link, answers, repos) = {
            let st = self.lock();
            let task = st.resolve(&s.task)?;
            let Some(th) = st.store.thread_state(&task.id)? else {
                return Err(HubError::Invalid(format!("task {} is not a thread", task.branch)));
            };
            let answers = st.store.last_answers(&task.id, 3)?;
            let repos = st.store.task_repos(&task.id)?;
            (task, th.link, answers, repos)
        };
        let (text, by, cost): (String, SummaryBy, Option<f64>) = match s.with {
            None => {
                let diff = (task.lifecycle != Lifecycle::Archived)
                    .then(|| match task.workspace_path {
                        Some(_) => crate::diff::workspace_stat(&repos),
                        None => crate::diff::diff_stat(Path::new(&task.repo_path), Path::new(&task.worktree_path)),
                    })
                    .and_then(|r| r.ok())
                    .map(|d| (d.files, d.added, d.removed));
                (
                    deterministic_summary(Some(task.status), answers.first().map(String::as_str), diff),
                    SummaryBy::Deterministic,
                    None,
                )
            }
            Some(SummarizeWith::Claude) => {
                let run = self.summarize_with_claude(&task, &link, &answers)?;
                (run.text, SummaryBy::Claude, run.cost_usd)
            }
        };
        let mut st = self.lock();
        let extra = cost.map_or_else(|| json!({}), |c| json!({ "cost_usd": c }));
        st.store.set_summary(&task.id, &text, by, extra)?;
        st.publish();
        Ok(SummaryView { summary: text, by, cost_usd: cost })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access;
    use crate::hub::testkit::Kit;
    use mirante_core::model::AgentKind;
    use mirante_core::threads::{LinkKind, MoveMode, Moved, ThreadLink};
    use mirante_ipc::types::{CreateTask, SendMessage, SendMode, StartSession};
    use serde_json::{json, Value};

    fn pr(n: u32) -> ThreadLink {
        ThreadLink {
            kind: LinkKind::Pr,
            reference: format!("acme/api#{n}"),
            url: Some(format!("https://github.com/acme/api/pull/{n}")),
            title: None,
        }
    }

    fn create(k: &Kit, parent: Option<&str>, link: ThreadLink, notes: &str, branch: &str) -> Result<TaskView> {
        k.hub.create_thread(
            &dna::Caller::Person,
            CreateThread {
                task: CreateTask {
                    repo: if parent.is_some() { String::new() } else { k.repo.to_string_lossy().into_owned() },
                    repos: Vec::new(),
                    agent: AgentKind::Claude,
                    branch: branch.into(),
                    base: None,
                    goal: "review the PR".into(),
                    title: None,
                    group: None,
                },
                link: Some(link),
                parent: parent.map(str::to_string),
                notes: Some(notes.into()),
                moved: None,
            },
        )
    }

    fn sends(k: &Kit) -> Vec<String> {
        k.calls().into_iter().filter(|c| c.starts_with("send:")).collect()
    }

    fn delivered(k: &Kit, task: &str) -> Vec<String> {
        k.hub
            .lock()
            .store
            .task_events(task)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "thread_delivered")
            .map(|e| e.payload["at"].as_str().unwrap().to_string())
            .collect()
    }

    /// Started (or resumed) and booted.
    fn start(k: &Kit, task: &str, prompt: Option<&str>, resume: bool) {
        k.hub.start_session(StartSession { task: task.into(), prompt: prompt.map(str::to_string) }, resume).unwrap();
        k.emit(crate::driver::DriverEvent::Boot(mirante_ipc::types::BootState::Ready));
    }

    fn person(k: &Kit, task: &str, text: &str) {
        k.hub.send(SendMessage { task: task.into(), text: text.into(), mode: SendMode::Queue }).unwrap();
        k.drain();
    }

    /// The agent finished its turn (so the next queued message is delivered at once).
    fn turn_done(k: &Kit, task: &str) {
        k.hook(task, json!({"hook_event_name": "UserPromptSubmit", "prompt": "x"}));
        k.hook(task, json!({"hook_event_name": "Stop"}));
        k.drain();
    }

    #[test]
    fn a_thread_from_a_parent_is_named_after_it_and_starts_from_its_branch() {
        let k = Kit::new();
        let parent = k.create(AgentKind::Claude, "mirante/login");
        let th = create(&k, Some(&parent.task.id), pr(123), "Keep the API stable.", "").unwrap();
        assert_eq!(th.task.parent_id.as_deref(), Some(parent.task.id.as_str()));
        assert_eq!(th.task.branch, "mirante/login--1-pr-acme-api-123");
        let v = th.thread.unwrap();
        assert_eq!((v.link.reference.as_str(), v.delivered), ("acme/api#123", false));
        assert!(k.kinds(&parent.task.id).contains(&"thread_started".to_string()));
        // A thread of that thread joins the same conversation (one level deep).
        let sib = create(&k, Some(&th.task.id), pr(124), "", "").unwrap();
        assert_eq!(sib.task.parent_id.as_deref(), Some(parent.task.id.as_str()));
        assert_eq!(sib.task.branch, "mirante/login--2-pr-acme-api-124");
    }

    #[test]
    fn open_threads_dedupe_on_parent_kind_ref() {
        let k = Kit::new();
        let parent = k.create(AgentKind::Claude, "main-chat");
        let first = create(&k, Some(&parent.task.id), pr(123), "", "").unwrap();
        let e = create(&k, Some(&parent.task.id), pr(123), "", "").unwrap_err();
        assert!(matches!(e, HubError::Conflict(_)), "{e}");
        assert!(e.to_string().contains(&first.task.id) && e.to_string().contains("continue there"), "{e}");
        // A link without a thread, and a thread without a link, are refused.
        let e = k
            .hub
            .create_thread(
                &dna::Caller::Person,
                CreateThread {
                    task: CreateTask {
                        repo: k.repo.to_string_lossy().into_owned(),
                        repos: Vec::new(),
                        agent: AgentKind::Claude,
                        branch: "x".into(),
                        base: None,
                        goal: "g".into(),
                        title: None,
                        group: None,
                    },
                    link: None,
                    parent: None,
                    notes: None,
                    moved: Some(Moved { mode: MoveMode::Copied, seq: None }),
                },
            )
            .unwrap_err();
        assert!(matches!(e, HubError::Invalid(_)));
        // No parent, no branch: the person names one.
        assert!(matches!(create(&k, None, pr(9), "", "").unwrap_err(), HubError::Invalid(_)));
    }

    #[test]
    fn notes_are_redacted_capped_8k_and_need_the_expected_hash() {
        let k = Kit::new();
        let th = create(&k, None, pr(1), "token ghp_abcdefghijklmnopqrstuvwxyz0123456789", "t1").unwrap();
        assert!(!th.task.notes.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), "{}", th.task.notes);
        let hash = th.thread.unwrap().notes_hash;
        let edit = |notes: String, expect: &str| {
            k.hub.thread_edit(ThreadEdit {
                task: th.task.id.clone(),
                notes: Some(notes),
                link: None,
                expect_hash: expect.into(),
            })
        };
        assert!(matches!(edit("x".repeat(8 * 1024 + 1), &hash).unwrap_err(), HubError::Invalid(_)));
        let stale = edit("new".into(), "0000").unwrap_err();
        assert_eq!(mirante_ipc::rpc::RpcError::from(stale).code, mirante_ipc::rpc::code::CONFLICT);
        let v = edit("new".into(), &hash).unwrap();
        assert_eq!(v.task.notes, "new");
        assert_ne!(v.thread.unwrap().notes_hash, hash);
        // A task that is no thread has no notes to edit here.
        let plain = k.create(AgentKind::Claude, "plain");
        let e = k.hub.thread_edit(ThreadEdit {
            task: plain.task.id,
            notes: Some("n".into()),
            link: None,
            expect_hash: hash,
        });
        assert!(matches!(e.unwrap_err(), HubError::Invalid(_)));
    }

    #[test]
    fn agents_cannot_edit_notes_or_links() {
        use mirante_ipc::types::method;
        let agent = dna::Identity::Agent { task: Some("t".into()) };
        for m in [method::TASK_THREAD, method::TASK_SUMMARIZE, method::TASK_CREATE] {
            assert_eq!(access::access_of(m), access::Access::Person, "{m}");
            assert!(access::check(m, &agent).is_err(), "{m}");
        }
    }

    #[test]
    fn start_prompt_carries_the_block_and_marks_it_delivered() {
        let k = Kit::new();
        let th = create(&k, None, pr(123), "Keep the API stable.", "t-start").unwrap();
        start(&k, &th.task.id, Some("review it"), false);
        let s = sends(&k);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(s[0].starts_with("send:<mirante-thread") && s[0].contains("pull/123"), "{s:?}");
        assert!(s[0].contains("Keep the API stable.") && s[0].ends_with("</mirante-thread>\nreview it"), "{s:?}");
        assert_eq!(delivered(&k, &th.task.id), vec!["start"]);
        // The log keeps the person's words, not the block.
        let ev = k.hub.lock().store.task_events(&th.task.id).unwrap();
        assert!(ev
            .iter()
            .filter(|e| e.kind == "message_sent")
            .all(|e| !e.payload.to_string().contains("mirante-thread")));
        assert!(k.hub.get_task(&th.task.id).unwrap().thread.unwrap().delivered);
        // The next message carries nothing (nothing changed).
        turn_done(&k, &th.task.id);
        person(&k, &th.task.id, "and the tests");
        assert_eq!(sends(&k).last().unwrap(), "send:and the tests");
    }

    #[test]
    fn first_message_after_resume_is_prefixed_once() {
        let k = Kit::new();
        let th = create(&k, None, pr(7), "notes", "t-resume").unwrap();
        let id = th.task.id.clone();
        start(&k, &id, None, false);
        // A start with no prompt: the person's first message carries it.
        person(&k, &id, "hello");
        assert!(sends(&k).last().unwrap().contains("<mirante-thread"));
        // The session ends; it is resumed.
        k.hook(&id, json!({"hook_event_name": "SessionStart", "session_id": "native-1", "source": "startup"}));
        k.emit(crate::driver::DriverEvent::Exited { code: Some(0) });
        start(&k, &id, None, true);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("native-1"));
        person(&k, &id, "where were we");
        let s = sends(&k);
        let last = s.last().unwrap();
        assert!(last.contains("<mirante-thread") && last.ends_with("where were we"), "{s:?}");
        turn_done(&k, &id);
        person(&k, &id, "go on");
        assert!(!sends(&k).last().unwrap().contains("<mirante-thread"));
        let at = delivered(&k, &id);
        assert_eq!(at.first().map(String::as_str), Some("start"));
        assert_eq!(at.last().map(String::as_str), Some("resume"));
    }

    #[test]
    fn changed_notes_prefix_the_next_message_once() {
        let k = Kit::new();
        let th = create(&k, None, pr(5), "first", "t-changed").unwrap();
        let id = th.task.id.clone();
        start(&k, &id, Some("go"), false);
        turn_done(&k, &id);
        let hash = k.hub.get_task(&id).unwrap().thread.unwrap().notes_hash;
        k.hub
            .thread_edit(ThreadEdit { task: id.clone(), notes: Some("second".into()), link: None, expect_hash: hash })
            .unwrap();
        assert!(!k.hub.get_task(&id).unwrap().thread.unwrap().delivered);
        // A slash command never carries it (the command must come first).
        person(&k, &id, "/compact");
        assert_eq!(sends(&k).last().unwrap(), "send:/compact");
        turn_done(&k, &id);
        person(&k, &id, "continue");
        let last = sends(&k).last().unwrap().clone();
        assert!(last.contains("second") && !last.contains("first") && last.ends_with("continue"), "{last}");
        turn_done(&k, &id);
        person(&k, &id, "more");
        assert!(!sends(&k).last().unwrap().contains("<mirante-thread"));
        assert_eq!(delivered(&k, &id), vec!["start", "changed"]);
    }

    #[test]
    fn an_agents_message_never_carries_the_block() {
        let k = Kit::new();
        let th = create(&k, None, pr(8), "n", "t-agent").unwrap();
        let id = th.task.id.clone();
        start(&k, &id, None, false);
        k.hub
            .send_by(SendMessage { task: id.clone(), text: "from a peer".into(), mode: SendMode::Queue }, "agent")
            .unwrap();
        k.drain();
        assert_eq!(sends(&k).last().unwrap(), "send:from a peer");
        assert!(delivered(&k, &id).is_empty());
    }

    #[test]
    fn an_aborted_delivery_gives_back_the_words_and_owes_the_block_again() {
        let k = Kit::new();
        let th = create(&k, None, pr(11), "n", "t-abort").unwrap();
        let id = th.task.id.clone();
        start(&k, &id, Some("review it"), false);
        let wire = k.factory.driver.wire.lock().unwrap().last().cloned().unwrap();
        assert!(wire.starts_with("<mirante-thread"), "{wire}");
        k.emit(crate::driver::DriverEvent::DeliveryAborted { text: wire, reason: "dialog".into() });
        let q = k.hub.list_queued(&id).unwrap();
        assert_eq!(q.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["review it"]);
        assert!(k.hub.lock().thread_owed(&id).unwrap().is_some());
        k.emit(crate::driver::DriverEvent::DialogClosed);
        let again = k.factory.driver.wire.lock().unwrap().last().cloned().unwrap();
        assert!(again.starts_with("<mirante-thread") && again.ends_with("review it"), "{again}");
    }

    #[test]
    fn a_final_answer_refreshes_the_deterministic_summary() {
        let k = Kit::new();
        let th = create(&k, None, pr(3), "", "t-sum").unwrap();
        let id = th.task.id.clone();
        {
            let mut st = k.hub.lock();
            st.event(
                Some(&id),
                "step",
                json!({"type": "agent_message", "text": "Fixed it.\nAdded a test.", "final": true}),
            )
            .unwrap();
            st.event(Some(&id), "step", json!({"type": "agent_message", "text": "thinking", "final": false})).unwrap();
        }
        let v = k.hub.get_task(&id).unwrap().thread.unwrap();
        assert_eq!(v.summary.as_deref(), Some("Fixed it. / Added a test."));
        assert_eq!(v.summary_by, Some(SummaryBy::Deterministic));
        let s = k.hub.summarize(Summarize { task: id.clone(), with: None }).unwrap();
        assert!(s.summary.starts_with("idle · Fixed it. / Added a test."), "{}", s.summary);
    }

    /// A fake `claude` (env is cleared for it: its log path is written into it). It records
    /// each run's cwd, env, stdin and argv (NUL-separated, the last run's), and answers
    /// `--version`, the probe (`init_tools` in its init line) and a summary.
    fn fake_claude(k: &Kit, init_tools: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = k.hub.paths().home().join("fakebin");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let bin = dir.join("claude");
        let body = format!(
            r#"LOG='{log}'
{{ echo "cwd=$(pwd)"; env | sed 's/^/env=/'; echo "stdin=$(cat)"; echo "args=$*" | head -c 200; echo; }} >> "$LOG"
for a in "$@"; do printf '%s\0' "$a"; done > "$LOG.argv"
case "$*" in
  *--version*) echo "2.1.285 (Claude Code)";;
  *stream-json*) echo '{{"type":"system","subtype":"init","tools":{init_tools},"mcp_servers":[],"slash_commands":[],"skills":[]}}'; echo '{{"type":"result","subtype":"success","result":"OK"}}';;
  *) echo '{{"type":"result","subtype":"success","is_error":false,"result":"Rebased; tests pass.","total_cost_usd":0.0106}}';;
esac"#,
            log = log.display()
        );
        mirante_core::test_script::write_sh(&bin, &body, 0o755);
        k.hub.set_summary_bin(bin.clone());
        (bin, log)
    }

    fn summaries_on(k: &Kit) {
        let s = k
            .hub
            .settings(mirante_ipc::types::SettingsParams {
                summaries: Some(mirante_ipc::types::SummariesSetting::Claude),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.summaries, mirante_ipc::types::SummariesSetting::Claude);
    }

    #[test]
    fn summarize_with_is_refused_unless_the_s2_probe_passed() {
        let k = Kit::new();
        let th = create(&k, None, pr(21), "", "t-probe").unwrap();
        let (_, log) = fake_claude(&k, r#"["Bash"]"#);
        let with = || k.hub.summarize(Summarize { task: th.task.id.clone(), with: Some(SummarizeWith::Claude) });
        // Off by default: nothing runs.
        assert_eq!(k.hub.summaries_setting(), mirante_ipc::types::SummariesSetting::None);
        let e = with().unwrap_err();
        assert!(matches!(e, HubError::Conflict(_)) && e.to_string().contains("off"), "{e}");
        assert!(!log.exists());
        // On, but this Claude offers a tool: refused, recorded for this version, never summarized.
        summaries_on(&k);
        let e = with().unwrap_err().to_string();
        assert!(e.contains("did not pass the tool-less check"), "{e}");
        let probe: Value =
            serde_json::from_str(&std::fs::read_to_string(k.hub.paths().home().join("summaries-probe.json")).unwrap())
                .unwrap();
        assert_eq!((probe["version"].as_str(), probe["ok"].as_bool()), (Some("2.1.285 (Claude Code)"), Some(false)));
        let runs = std::fs::read_to_string(&log).unwrap().matches("cwd=").count();
        assert!(with().is_err());
        // The verdict is kept per version: no second probe, still no summary.
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().matches("cwd=").count(),
            runs + 1,
            "only --version ran again"
        );
        assert!(k.hub.get_task(&th.task.id).unwrap().thread.unwrap().summary.is_none());
        // Codex is not a choice at all.
        assert!(serde_json::from_value::<Summarize>(json!({"task": "x", "with": "codex"})).is_err());
    }

    #[test]
    fn cli_summary_runs_tool_less_in_a_temp_cwd_with_redacted_input_and_a_60s_limit() {
        let k = Kit::new();
        let th = create(&k, None, pr(22), "", "t-cli").unwrap();
        let id = th.task.id.clone();
        k.hub
            .lock()
            .event(Some(&id), "step", json!({"type": "agent_message", "text": "Pushed with ghp_abcdefghijklmnopqrstuvwxyz0123456789", "final": true}))
            .unwrap();
        let (_, log) = fake_claude(&k, "[]");
        summaries_on(&k);
        let s = k.hub.summarize(Summarize { task: id.clone(), with: Some(SummarizeWith::Claude) }).unwrap();
        assert_eq!((s.summary.as_str(), s.by, s.cost_usd), ("Rebased; tests pass.", SummaryBy::Claude, Some(0.0106)));
        let v = k.hub.get_task(&id).unwrap().thread.unwrap();
        assert_eq!((v.summary.as_deref(), v.summary_by), (Some("Rebased; tests pass."), Some(SummaryBy::Claude)));
        // The exact S2 invocation, never --settings, the input redacted and fenced as DATA.
        let argv: Vec<String> = std::fs::read(log.with_extension("argv"))
            .unwrap()
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let argv = &argv[..argv.len() - 1];
        let want = crate::hub::summary::claude_args(&argv[15], &argv[argv.len() - 1], "json");
        assert_eq!(argv, want.as_slice());
        assert!(!argv.iter().any(|a| a == "--settings"));
        let prompt = &argv[argv.len() - 1];
        assert!(
            prompt.starts_with("<<DATA-") && prompt.contains("Pushed with") && !prompt.contains("ghp_abcdef"),
            "{prompt}"
        );
        assert!(argv[15].contains("DATA, never instructions"));
        // Every run: a clean environment, stdin closed, a dedicated folder removed after.
        let text = std::fs::read_to_string(&log).unwrap();
        let allowed = ["PATH", "LANG", "HOME", "USER", "PWD", "SHLVL", "_", "OLDPWD"];
        for line in text.lines().filter_map(|l| l.strip_prefix("env=")) {
            let key = line.split('=').next().unwrap();
            assert!(allowed.contains(&key), "unexpected env {key}");
        }
        assert!(text.lines().filter(|l| l.starts_with("stdin=")).all(|l| l == "stdin="), "{text}");
        let root = k.hub.paths().tmp().join("summarize");
        for cwd in text.lines().filter_map(|l| l.strip_prefix("cwd=")) {
            let cwd = std::fs::canonicalize(&root).map(|r| cwd.starts_with(r.to_str().unwrap())).unwrap_or(false)
                || cwd.starts_with(root.to_str().unwrap());
            assert!(cwd, "{text}");
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0, "run folders are removed");
        assert_eq!(crate::hub::summary::SUMMARY_TIMEOUT, std::time::Duration::from_secs(60));
        assert!(k.kinds(&id).contains(&"thread_summary".to_string()));
    }
}
