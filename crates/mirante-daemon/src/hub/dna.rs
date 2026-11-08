//! DNA, the repo memory: the daemon side of `dna.*`.
//!
//! The index lives in the store and is rebuilt from `<repo>/.mirante/memory` lazily: every
//! read checks HEAD and the directory's stat signature first ([`index::needs_refresh`]).
//! git and file reads run without the hub lock; only the store writes take it.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mirante_core::claude_launch::{AGENT_ENV, TASK_ENV};
use mirante_core::dna::anchor::check_path;
use mirante_core::dna::anchor::Anchor;
use mirante_core::dna::check::{check_all, cite, is_dirty, rev_parse, AnchorState};
use mirante_core::dna::index::{self, IndexedAnchor, NewProposal, NoteRow, ProposalCaps, ProposalQuery, RefreshReport};
use mirante_core::dna::note::{Kind, Note, Status};
use mirante_core::dna::untrusted::{self, field};
use mirante_core::dna::{fsio, note, sha256_hex, DnaError, MAX_ANCHORS, MAX_BODY_BYTES, MAX_TITLE_CHARS};
use mirante_core::git::Git;
use mirante_core::model::{new_id, now_ms, Lifecycle};
use mirante_core::redact::redact_counted;
use mirante_core::CoreError;
use mirante_ipc::types::{
    AnchorView, AtRisk, CheckReport, DnaGet, DnaGraph, DnaNoteAction, DnaProposals, DnaPropose, DnaRef, DnaReview,
    DnaSearch, DnaStatus, DnaTaskContext, GraphLink, NoteAction, NoteSummary, NoteView, ProposalView,
};
use serde_json::json;

use super::{Hub, HubError, Result};
use crate::driver::proctree::{self, Proc};

/// Who is calling: a person (in-process, tests), a socket peer (its process, pid AND start
/// time, read once when it connected), or a peer whose process could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Person,
    Peer(Proc),
    Unknown,
}

/// What [`Hub::identify`] makes of a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// In-process, or a live peer that shows no sign of being a Mirante-launched agent.
    Person,
    /// A Mirante-launched agent or a process it started: inside a live session's process
    /// tree, carrying the agent env marker, or working inside a task worktree. `task` when
    /// every sign names the same task.
    Agent { task: Option<String> },
    /// The peer could not be checked: gone, its pid reused, or its environment or working
    /// directory unreadable. Treated as an agent (fail closed).
    Unverified(String),
}

/// The refusal for Mirante-launched agents.
const AGENT_REFUSED: &str = "Mirante-launched agents are refused: reviewing memory is a person's decision. \
Review it in the cockpit, or with the mirante CLI in a terminal you opened yourself";

/// What a person whose call could not be checked can do instead.
const HOW_TO_REVIEW: &str = "memory review is refused when the caller cannot be checked; review it in the \
cockpit, or with the mirante CLI in a terminal you opened yourself";

/// The task the agent env markers name: `None` without markers, `Some(None)` when they
/// disagree or are empty.
fn marker_task(env: &[String]) -> Option<Option<String>> {
    let values: Vec<&str> = env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| *k == AGENT_ENV || *k == TASK_ENV)
        .map(|(_, v)| v)
        .collect();
    let first = *values.first()?;
    Some((!first.is_empty() && values.iter().all(|v| *v == first)).then(|| first.to_string()))
}

/// Pending proposals one task, and one repo, may have at once.
const CAPS: ProposalCaps = ProposalCaps { per_task: 20, per_repo: 200 };

/// Proposals per `dna.proposals` page by default and at most; a task context lists at most
/// the default.
const PAGE_DEFAULT: usize = 50;
const PAGE_MAX: usize = 200;

/// Most notes `dna.task_context` lists as relevant.
const RELEVANT_MAX: usize = 10;

/// Search results by default and at most.
const SEARCH_DEFAULT: usize = 10;
const SEARCH_MAX: usize = 25;

/// A DNA failure as the hub reports it: bad input is `Invalid`, a missing note is
/// `NotFound`, everything else (unsafe directory, git, I/O, a file that changed or appeared
/// meanwhile) is `Conflict`.
pub(crate) fn dna_err(e: DnaError) -> HubError {
    match e {
        DnaError::Anchor(..) | DnaError::Path(..) | DnaError::Note(_) => HubError::Invalid(e.to_string()),
        DnaError::Missing(_) => HubError::NotFound(e.to_string()),
        DnaError::Unsafe(_) | DnaError::Git(_) | DnaError::Changed | DnaError::Exists(_) | DnaError::Io(_) => {
            HubError::Conflict(e.to_string())
        }
    }
}

fn state_name(s: &AnchorState) -> &'static str {
    match s {
        AnchorState::Unchecked => "unchecked",
        AnchorState::Fresh => "fresh",
        AnchorState::Relocated { .. } => "relocated",
        AnchorState::Stale { .. } => "stale",
        AnchorState::Unverifiable { .. } => "unverifiable",
    }
}

/// Longest path, symbol and reason an agent is shown (chars).
const MAX_FIELD: usize = 512;

/// Which projection of memory text a caller gets. Both strip control and invisible
/// characters from every one-line field; they differ on `<`.
///
/// - `Person` (the cockpit, the CLI): `<` as written ([`untrusted::line`]). React renders
///   text, never markup, so `&lt;` would show literally. Bodies are the file's text.
/// - `Agent`: every `<` look-alike escaped ([`untrusted::field`], [`untrusted::body`]), so
///   nothing can open a tag or close a `<mirante-memory>` wrapper.
///
/// Who gets which ([`Hub::view_for`]): every `dna.*` read answers a Mirante-launched agent,
/// or a caller that could not be checked, with the agent view; `dna.task_context` also
/// takes `for_agent` (what the daemon hands an agent, and the MCP server, ask for it). In
/// process (tests, the hub itself) the `dna_*` methods give the person view and the
/// `dna_*_in` ones take the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Person,
    Agent,
}

impl View {
    fn line(self, text: &str, max: usize) -> String {
        match self {
            View::Person => untrusted::line(text, max),
            View::Agent => field(text, max),
        }
    }

    fn body(self, text: &str) -> String {
        match self {
            View::Person => text.to_string(),
            View::Agent => untrusted::body(text),
        }
    }
}

/// A proposal as `view` shows it (its title, body and decision note).
fn proposal_view(p: ProposalView, view: View) -> ProposalView {
    ProposalView {
        title: view.line(&p.title, MAX_TITLE_CHARS * 4),
        body: view.body(&p.body),
        decision_note: view.body(&p.decision_note),
        ..p
    }
}

/// Every text field of a note view goes through `view` (see [`View`]).
fn anchor_view(a: &IndexedAnchor, view: View) -> AnchorView {
    let (reason, now) = match &a.state {
        AnchorState::Stale { reason } | AnchorState::Unverifiable { reason } => {
            (Some(view.line(reason, MAX_FIELD)), None)
        }
        AnchorState::Relocated { start, end } => (None, Some((*start, *end))),
        _ => (None, None),
    };
    AnchorView {
        path: view.line(&a.anchor.path, MAX_FIELD),
        symbol: a.anchor.symbol.as_deref().map(|s| view.line(s, MAX_FIELD)),
        start: a.anchor.start,
        end: a.anchor.end,
        commit: a.anchor.commit.clone(),
        state: state_name(&a.state).to_string(),
        reason,
        now_start: now.map(|n| n.0),
        now_end: now.map(|n| n.1),
    }
}

/// The worst anchor state: stale, then unverifiable, then unchecked, else fresh.
fn worst(anchors: &[IndexedAnchor]) -> &'static str {
    let has = |f: fn(&AnchorState) -> bool| anchors.iter().any(|a| f(&a.state));
    if has(|s| matches!(s, AnchorState::Stale { .. })) {
        "stale"
    } else if has(|s| matches!(s, AnchorState::Unverifiable { .. })) {
        "unverifiable"
    } else if anchors.is_empty() || has(|s| matches!(s, AnchorState::Unchecked)) {
        "unchecked"
    } else {
        "fresh"
    }
}

pub(crate) fn summary(n: &NoteRow, reviewed: bool, view: View) -> NoteSummary {
    NoteSummary {
        slug: n.slug.clone(),
        title: view.line(&n.title, MAX_TITLE_CHARS * 4),
        kind: n.kind.clone(),
        status: n.status.clone(),
        state: worst(&n.anchors).to_string(),
        reviewed,
        anchors: n.anchors.iter().map(|a| anchor_view(a, view)).collect(),
    }
}

fn key_of(repo: &Path) -> String {
    repo.to_string_lossy().into_owned()
}

/// The checkout `path` is in: git's top level, canonical (a task may have been created from
/// a subdirectory).
fn toplevel(path: &str) -> Result<PathBuf> {
    let p = std::fs::canonicalize(path).map_err(|e| HubError::Invalid(format!("repo {path}: {e}")))?;
    let out = Git::new(&p).args(["rev-parse", "--show-toplevel"]).output();
    let top = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return Err(HubError::Invalid(format!("{path} is not inside a git work tree"))),
    };
    std::fs::canonicalize(&top).map_err(|e| HubError::Invalid(format!("repo {top}: {e}")))
}

/// Each anchor re-cited at the checkout's HEAD when its code is there (fresh or relocated):
/// an accepted note then cites a commit of the checkout, not one only a task branch has
/// (which a squash merge and gc would make unverifiable). Anchors whose code is not in the
/// checkout yet keep the task-branch citation (stale or unverifiable until the merge, then
/// a Confirm away from fresh).
fn recite_at_checkout(repo: &Path, anchors: &[Anchor]) -> Vec<Anchor> {
    let Ok(head) = rev_parse(repo, "HEAD") else { return anchors.to_vec() };
    let Ok(checked) = check_all(repo, &head, anchors) else { return anchors.to_vec() };
    anchors
        .iter()
        .zip(checked)
        .map(|(a, c)| {
            let (start, end) = match c.state {
                AnchorState::Fresh => (a.start, a.end),
                AnchorState::Relocated { start, end } => (start, end),
                _ => return a.clone(),
            };
            cite(repo, &head, &a.path, a.symbol.as_deref(), start, end).unwrap_or_else(|_| a.clone())
        })
        .collect()
}

/// Today (UTC) as `YYYY-MM-DD`.
fn today() -> String {
    // Howard Hinnant's civil_from_days.
    let z = now_ms().div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A title and body as a note takes them: redacted, title trimmed and bounded, body
/// bounded and ending in a newline. Returns them with the number of redactions.
fn clean_text(title: &str, body: &str) -> Result<(String, String, usize)> {
    let (title, a) = redact_counted(title.trim());
    let (mut body, b) = redact_counted(body);
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(HubError::Invalid(format!("the title must be 1..={MAX_TITLE_CHARS} characters")));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(HubError::Invalid(format!("the body is larger than {MAX_BODY_BYTES} bytes")));
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok((title, body, a + b))
}

/// Creates the note under `base`, or `base-2`, `base-3`… past existing files. Never
/// overwrites.
fn create_unique(repo: &Path, base: &str, text: &str) -> std::result::Result<String, DnaError> {
    let base = if base == "readme" { "readme-note" } else { base };
    for n in 1..=100 {
        let slug = if n == 1 {
            base.to_string()
        } else {
            let suffix = format!("-{n}");
            format!("{}{suffix}", base[..base.len().min(64 - suffix.len())].trim_end_matches('-'))
        };
        match fsio::create_note(repo, &slug, text) {
            Ok(()) => return Ok(slug),
            Err(DnaError::Exists(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(DnaError::Note(format!("too many notes named {base}")))
}

impl Hub {
    /// The repo checkout `at` names, canonical: a task's repo, or a path inside a git work
    /// tree (its top level). Agent worktrees are refused: they are never indexed.
    pub(crate) fn dna_repo_of(&self, at: &DnaRef) -> Result<PathBuf> {
        let path = match (&at.task, &at.repo) {
            (Some(task), _) => self.lock().resolve(task)?.repo_path,
            (None, Some(repo)) => repo.clone(),
            (None, None) => return Err(HubError::Invalid("name a repo or a task".into())),
        };
        self.checkout(&path)
    }

    /// [`toplevel`], refusing an agent worktree.
    pub(crate) fn checkout(&self, path: &str) -> Result<PathBuf> {
        let top = toplevel(path)?;
        let worktrees = self.paths.worktrees();
        let worktrees = std::fs::canonicalize(&worktrees).unwrap_or(worktrees);
        if top.starts_with(&worktrees) {
            return Err(HubError::Invalid(format!(
                "{} is an agent worktree; repo memory lives in the repo checkout",
                top.display()
            )));
        }
        Ok(top)
    }

    /// The repo's refresh gate: one refresh (or freshness check) at a time per repo.
    fn dna_gate(&self, repo: &Path) -> Arc<Mutex<()>> {
        let mut gates = self.dna_gates.lock().unwrap_or_else(|p| p.into_inner());
        gates.entry(key_of(repo)).or_default().clone()
    }

    /// Reindexes `repo` (git and file reads without the hub lock), one refresh at a time.
    pub(crate) fn dna_refresh(&self, repo: &Path) -> Result<RefreshReport> {
        let gate = self.dna_gate(repo);
        let _one = gate.lock().unwrap_or_else(|p| p.into_inner());
        self.dna_refresh_gated(repo)
    }

    /// The refresh itself (the caller holds the repo's gate). Emits `dna.indexed` only when
    /// the head or a count changed since the last one.
    fn dna_refresh_gated(&self, repo: &Path) -> Result<RefreshReport> {
        let scan = index::scan(repo).map_err(dna_err)?;
        let checks = index::check_scan(repo, &scan).map_err(dna_err)?;
        let key = key_of(repo);
        let mut st = self.lock();
        let rep = index::apply(&mut st.store, &key, &scan, &checks)?;
        st.dna_sigs.insert(key.clone(), scan.signature);
        let payload = json!({
            "repo": key, "head": rep.head, "notes": rep.notes, "stale": rep.stale,
            "unreviewed": rep.unreviewed, "invalid": rep.invalid.len(),
        });
        if st.dna_indexed.get(&key) != Some(&payload) {
            st.event(None, "dna.indexed", payload.clone())?;
            st.dna_indexed.insert(key, payload);
            st.publish();
        }
        Ok(rep)
    }

    /// Reindexes `repo` when HEAD or the memory directory changed since the last time. The
    /// check runs under the repo's gate, so concurrent readers wait for one refresh instead
    /// of each running their own.
    pub(crate) fn dna_ensure_fresh(&self, repo: &Path) -> Result<()> {
        let gate = self.dna_gate(repo);
        let _one = gate.lock().unwrap_or_else(|p| p.into_inner());
        let probe = index::probe(repo).map_err(dna_err)?;
        let key = key_of(repo);
        let stale = {
            let st = self.lock();
            index::needs_refresh(&st.store, &key, &probe, st.dna_sigs.get(&key).map(String::as_str))?
        };
        if stale {
            self.dna_refresh_gated(repo)?;
        }
        Ok(())
    }

    /// The memory MCP server for a new session of `task`: only when its repo's memory was
    /// explicitly enabled. When it is enabled but the binary cannot be found, the session
    /// still starts without memory (memory is not worth blocking a task) and a
    /// `dna.unavailable` event says why.
    pub(crate) fn memory_server_for(&self, task: &mirante_core::model::Task) -> Option<PathBuf> {
        let Ok(repo) = self.checkout(&task.repo_path) else {
            return None;
        };
        let enabled = matches!(self.lock().store.dna_repo(&key_of(&repo)), Ok(Some(r)) if r.enabled);
        if !enabled {
            return None;
        }
        match self.factory.memory_server() {
            Ok(bin) => Some(bin),
            Err(reason) => {
                let mut st = self.lock();
                let payload = json!({ "reason": format!("repo memory is off for this session: {reason}") });
                if let Err(e) = st.event(Some(&task.id), "dna.unavailable", payload) {
                    st.log_error(Some(&task.id), "dna.unavailable", &e);
                }
                st.publish();
                None
            }
        }
    }

    /// `Conflict` unless memory is enabled for the repo `key`.
    fn require_enabled(&self, key: &str) -> Result<()> {
        if self.lock().store.dna_repo(key)?.is_some_and(|r| r.enabled) {
            Ok(())
        } else {
            Err(HubError::Conflict("memory is not enabled for this repo".into()))
        }
    }

    fn dna_status_of(&self, repo: &Path) -> Result<DnaStatus> {
        let dir = fsio::memory_dir(repo).is_ok();
        if dir {
            self.dna_ensure_fresh(repo)?;
        }
        let key = key_of(repo);
        let st = self.lock();
        let row = st.store.dna_repo(&key)?;
        let c = st.store.dna_status(&key)?;
        drop(st);
        let head = match row.as_ref().and_then(|r| r.checked_head.clone()) {
            Some(h) => Some(h),
            None => mirante_core::dna::check::rev_parse(repo, "HEAD").ok(),
        };
        Ok(DnaStatus {
            repo: key,
            enabled: row.is_some_and(|r| r.enabled),
            dir,
            notes: c.notes,
            stale: c.stale,
            unreviewed: c.unreviewed,
            pending: c.pending,
            invalid: c.invalid,
            head,
        })
    }

    pub fn dna_status(&self, at: &DnaRef) -> Result<DnaStatus> {
        let repo = self.dna_repo_of(at)?;
        self.dna_status_of(&repo)
    }

    /// Who `caller` is. A peer is a Mirante-launched agent when it is a live session's
    /// process-tree root or below it, when its environment carries the agent marker
    /// ([`AGENT_ENV`], or Claude's [`TASK_ENV`]; inherited by everything an agent starts, so
    /// it survives a double fork, `setsid`, a tmux server or a daemon restart), or when it
    /// works inside the worktree root. Fails closed: a peer that is gone, whose pid now
    /// names another process, or whose environment or working directory cannot be read is
    /// `Unverified`. The process is re-checked after the reads, so they were its own.
    ///
    /// Defense in depth, not a boundary: agents run as the same uid, and one that scrubs its
    /// environment, leaves the tree and the worktree before connecting is not recognized.
    pub fn identify(&self, caller: &Caller) -> Identity {
        let p = match caller {
            Caller::Person => return Identity::Person,
            Caller::Unknown => return Identity::Unverified("could not read the calling process".into()),
            Caller::Peer(p) => *p,
        };
        let gone = || Identity::Unverified("the calling process exited, or its pid now names another process".into());
        if !proctree::alive(p) {
            return gone();
        }
        let by_tree = self.task_of_tree(p);
        let by_tty = self.task_of_tty(p);
        let env = proctree::environ(p.pid);
        let cwd = crate::memguard::orphans::process_cwd(p.pid);
        if !proctree::alive(p) {
            return gone();
        }
        let by_env = env.as_deref().and_then(marker_task);
        if by_tree.is_some() || by_tty.is_some() || by_env.is_some() {
            let signs: Vec<Option<String>> = by_tree.into_iter().chain(by_tty).map(Some).chain(by_env).collect();
            let task = match signs.split_first() {
                Some((first, rest)) if rest.iter().all(|t| t == first) => first.clone(),
                _ => None,
            };
            return Identity::Agent { task };
        }
        if env.is_none() {
            return Identity::Unverified("could not read the calling process's environment".into());
        }
        let Some(cwd) = cwd else {
            return Identity::Unverified("could not read the calling process's working directory".into());
        };
        let root = self.paths.worktrees();
        let canonical = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if cwd.starts_with(&root) || cwd.starts_with(&canonical) {
            return Identity::Agent { task: None };
        }
        Identity::Person
    }

    /// The task whose live session's terminal is `p`'s controlling terminal: started from
    /// the agent's session, even if it left the process tree and scrubbed its environment.
    fn task_of_tty(&self, p: Proc) -> Option<String> {
        let tty = proctree::controlling_tty(p.pid)?;
        let st = self.lock();
        st.live.iter().find(|(_, l)| l.driver.as_ref().and_then(|d| d.tty()) == Some(tty)).map(|(t, _)| t.clone())
    }

    /// The task whose live session's process tree holds `p` (its root or a descendant).
    fn task_of_tree(&self, p: Proc) -> Option<String> {
        let roots: Vec<(String, Proc)> = {
            let st = self.lock();
            st.live.iter().filter_map(|(t, l)| Some((t.clone(), l.driver.as_ref()?.root()?))).collect()
        };
        if let Some((t, _)) = roots.iter().find(|(_, r)| *r == p) {
            return Some(t.clone());
        }
        roots.into_iter().find(|(_, root)| proctree::descendants(*root).contains(&p)).map(|(t, _)| t)
    }

    /// Ok for a person; `Forbidden` for a Mirante-launched agent and for a caller that could
    /// not be checked (see [`Hub::identify`]).
    pub(crate) fn require_person(&self, c: &Caller) -> Result<()> {
        match self.identify(c) {
            Identity::Person => Ok(()),
            Identity::Agent { .. } => Err(HubError::Forbidden(AGENT_REFUSED.into())),
            Identity::Unverified(why) => Err(HubError::Forbidden(format!("{why}: {HOW_TO_REVIEW}"))),
        }
    }

    /// Creates `.mirante/memory/README.md` (kept if present) and turns injection on for new
    /// sessions of this repo's tasks.
    pub fn dna_enable(&self, at: &DnaRef, caller: &Caller) -> Result<DnaStatus> {
        self.require_person(caller)?;
        let repo = self.dna_repo_of(at)?;
        fsio::init(&repo).map_err(dna_err)?;
        {
            let mut st = self.lock();
            st.store.dna_set_enabled(&key_of(&repo), true)?;
        }
        self.dna_refresh(&repo)?;
        self.dna_status_of(&repo)
    }

    /// Turns injection off for new sessions. The files are left alone.
    pub fn dna_disable(&self, at: &DnaRef, caller: &Caller) -> Result<DnaStatus> {
        self.require_person(caller)?;
        let repo = self.dna_repo_of(at)?;
        self.lock().store.dna_set_enabled(&key_of(&repo), false)?;
        self.dna_status_of(&repo)
    }

    /// Reindexes and rechecks every anchor at the checkout's HEAD.
    pub fn dna_check(&self, at: &DnaRef) -> Result<CheckReport> {
        let repo = self.dna_repo_of(at)?;
        self.dna_refresh(&repo)
    }

    pub(crate) fn dna_summaries(&self, key: &str, rows: &[NoteRow], view: View) -> Result<Vec<NoteSummary>> {
        let st = self.lock();
        rows.iter().map(|n| Ok(summary(n, st.store.dna_is_reviewed(key, &n.slug, &n.content_hash)?, view))).collect()
    }

    /// The projection `caller` gets (see [`View`]): the agent view when `for_agent` asks for
    /// it, or for any caller [`Hub::identify`] does not call a person.
    pub fn view_for(&self, caller: &Caller, for_agent: bool) -> View {
        if for_agent || self.identify(caller) != Identity::Person {
            View::Agent
        } else {
            View::Person
        }
    }

    /// The projection a socket caller's read of `at` gets, or why it may not read. A person
    /// reads any repo or task. A Mirante-launched agent reads only through its own task
    /// (named as `task`, never as a `repo` path), and only while that repo's memory is
    /// enabled, so disabling it stops a running session's reads too. An agent whose task is
    /// unknown, and a caller that could not be checked, are refused (like `dna.propose`).
    pub fn read_view(&self, caller: &Caller, at: &DnaRef) -> Result<View> {
        let own = |why: &str| {
            HubError::Forbidden(format!("{why}: a Mirante-launched agent can only read memory for its own task"))
        };
        let task = match self.identify(caller) {
            Identity::Person => return Ok(View::Person),
            Identity::Agent { task: Some(t) } => t,
            Identity::Agent { task: None } => return Err(own("this agent's task is unknown")),
            Identity::Unverified(why) => return Err(HubError::Forbidden(why)),
        };
        let named = match (&at.task, &at.repo) {
            (Some(n), None) => self.lock().resolve(n)?.id,
            _ => return Err(own("name the task, not a repo")),
        };
        if named != task {
            return Err(own("another task was named"));
        }
        let repo = self.dna_repo_of(at)?;
        self.require_enabled(&key_of(&repo))?;
        Ok(View::Agent)
    }

    /// `dna.search` for a socket caller (see [`Hub::read_view`]).
    pub fn dna_search_from(&self, caller: &Caller, p: DnaSearch) -> Result<Vec<NoteSummary>> {
        let view = self.read_view(caller, &p.at)?;
        self.dna_search_in(view, p)
    }

    /// `dna.get` for a socket caller (see [`Hub::read_view`]).
    pub fn dna_get_from(&self, caller: &Caller, p: DnaGet) -> Result<NoteView> {
        let view = self.read_view(caller, &p.at)?;
        self.dna_get_in(view, p)
    }

    pub fn dna_search(&self, p: DnaSearch) -> Result<Vec<NoteSummary>> {
        self.dna_search_in(View::Person, p)
    }

    pub fn dna_search_in(&self, view: View, p: DnaSearch) -> Result<Vec<NoteSummary>> {
        let repo = self.dna_repo_of(&p.at)?;
        self.dna_ensure_fresh(&repo)?;
        let Some(fts) = index::fts_query(&p.query) else {
            return Ok(Vec::new());
        };
        let key = key_of(&repo);
        let limit = p.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX);
        let rows = self.lock().store.dna_search(&key, &fts, limit)?;
        self.dna_summaries(&key, &rows, view)
    }

    pub(crate) fn dna_view(&self, repo: &Path, slug: &str, view: View) -> Result<NoteView> {
        let key = key_of(repo);
        let st = self.lock();
        let n = st.store.dna_note(&key, slug)?.ok_or_else(|| HubError::NotFound(format!("no note {slug}")))?;
        if let Some(e) = &n.parse_error {
            return Err(HubError::Conflict(format!("note {slug} does not parse: {e}")));
        }
        let reviewed = st.store.dna_is_reviewed(&key, slug, &n.content_hash)?;
        let backlinks = st.store.dna_backlinks(&key, slug)?;
        Ok(NoteView {
            summary: summary(&n, reviewed, view),
            links: note::links(&n.body),
            backlinks,
            content_hash: n.content_hash.clone(),
            source: view.line(&n.source, MAX_FIELD),
            path: view.line(&fsio::note_path(repo, slug).to_string_lossy(), MAX_FIELD * 8),
            body: view.body(&n.body),
        })
    }

    pub fn dna_get(&self, p: DnaGet) -> Result<NoteView> {
        self.dna_get_in(View::Person, p)
    }

    pub fn dna_get_in(&self, view: View, p: DnaGet) -> Result<NoteView> {
        let repo = self.dna_repo_of(&p.at)?;
        self.dna_ensure_fresh(&repo)?;
        self.dna_view(&repo, &p.slug, view)
    }

    pub fn dna_graph(&self, at: &DnaRef) -> Result<DnaGraph> {
        self.dna_graph_in(View::Person, at)
    }

    pub fn dna_graph_in(&self, view: View, at: &DnaRef) -> Result<DnaGraph> {
        let repo = self.dna_repo_of(at)?;
        self.dna_ensure_fresh(&repo)?;
        let key = key_of(&repo);
        let (rows, edges) = self.lock().store.dna_graph(&key)?;
        let nodes = self.dna_summaries(&key, &rows, view)?;
        let links = edges
            .into_iter()
            .map(|(src, dst)| GraphLink { dangling: !nodes.iter().any(|n| n.slug == dst), src, dst })
            .collect();
        Ok(DnaGraph { nodes, links })
    }
}

impl Hub {
    /// A proposed note, cited at the task worktree's HEAD, queued for a person's review.
    /// A Mirante-launched agent may only propose for its own task (found by its process tree
    /// and env marker, never by the parameter); a person (the CLI) for any task.
    pub fn dna_propose(&self, caller: &Caller, p: DnaPropose) -> Result<ProposalView> {
        let own = |why: &str| {
            HubError::Forbidden(format!("{why}: a Mirante-launched agent can only propose memory for its own task"))
        };
        let agent_task = match self.identify(caller) {
            Identity::Person => None,
            Identity::Agent { task: Some(t) } => Some(t),
            Identity::Agent { task: None } => return Err(own("this agent's task is unknown")),
            Identity::Unverified(why) => return Err(HubError::Forbidden(why)),
        };
        let task = self.lock().resolve(&p.task)?;
        if agent_task.as_ref().is_some_and(|t| *t != task.id) {
            return Err(own("another task was named"));
        }
        if task.lifecycle == Lifecycle::Archived {
            return Err(HubError::Conflict(format!("task {} is archived", task.branch)));
        }
        let repo = self.checkout(&task.repo_path)?;
        let key = key_of(&repo);
        self.require_enabled(&key)?;
        let kind = Kind::parse(&p.kind)
            .ok_or_else(|| HubError::Invalid(format!("kind {:?}: use fact, decision, pattern or gotcha", p.kind)))?;
        let (title, body, mut redactions) = clean_text(&p.title, &p.body)?;
        if p.anchors.is_empty() || p.anchors.len() > MAX_ANCHORS {
            return Err(HubError::Invalid(format!("a note cites 1..={MAX_ANCHORS} code ranges")));
        }
        // git, unlocked: cite committed lines at the worktree's HEAD.
        let worktree = Path::new(&task.worktree_path);
        let invalid = |e: DnaError| HubError::Invalid(e.to_string());
        let commit = rev_parse(worktree, "HEAD").map_err(dna_err)?;
        let mut anchors = Vec::with_capacity(p.anchors.len());
        for a in &p.anchors {
            check_path(&a.path).map_err(invalid)?;
            if is_dirty(worktree, &a.path).map_err(dna_err)? {
                return Err(HubError::Invalid(format!(
                    "{} has uncommitted changes in the task's worktree: commit first, then cite",
                    a.path
                )));
            }
            let symbol = a.symbol.as_deref().map(redact_counted);
            redactions += symbol.as_ref().map_or(0, |s| s.1);
            anchors.push(
                cite(worktree, &commit, &a.path, symbol.as_ref().map(|s| s.0.as_str()), a.start, a.end)
                    .map_err(invalid)?,
            );
        }
        // The note it would become must be valid now, so accepting it cannot fail on format.
        let probe = Note {
            id: new_id(),
            title: title.clone(),
            kind,
            status: Status::Active,
            created: today(),
            source: format!("task:{}", task.id),
            anchors: anchors.clone(),
            body: body.clone(),
        };
        note::render(&probe).map_err(invalid)?;
        let new = NewProposal {
            repo: key,
            task_id: Some(task.id.clone()),
            origin: if agent_task.is_some() { "agent" } else { "cli" }.into(),
            title,
            kind: kind.as_str().into(),
            body,
            anchors,
            redactions,
        };
        let mut st = self.lock();
        // The caps are counted inside the insert's transaction (with its `dna.proposed`).
        let proposal = st.store.dna_insert_proposal(&new, CAPS).map_err(|e| match e {
            CoreError::Limit(m) => HubError::Conflict(m),
            e => e.into(),
        })?;
        st.publish();
        Ok(proposal)
    }

    /// The review queue: proposals, oldest first, a page at a time (pending by default; see
    /// [`DnaProposals`]).
    pub fn dna_proposals(&self, q: DnaProposals) -> Result<Vec<ProposalView>> {
        self.dna_proposals_in(View::Person, q)
    }

    pub fn dna_proposals_in(&self, view: View, q: DnaProposals) -> Result<Vec<ProposalView>> {
        let status = match q.status.as_deref().unwrap_or("pending") {
            "all" => None,
            s @ ("pending" | "accepted" | "rejected") => Some(s.to_string()),
            s => return Err(HubError::Invalid(format!("status {s:?}: use pending, accepted, rejected or all"))),
        };
        let repo = match &q.repo {
            Some(r) => Some(key_of(&self.dna_repo_of(&DnaRef { repo: Some(r.clone()), task: None })?)),
            None => None,
        };
        let st = self.lock();
        let task_id = match &q.task {
            Some(t) => Some(st.resolve(t)?.id),
            None => None,
        };
        let query = ProposalQuery {
            repo,
            task_id,
            status,
            id: q.id,
            after: q.after,
            limit: q.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX),
        };
        let page = st.store.dna_proposals(&query).map_err(|e| match e {
            CoreError::NotFound { id, .. } => HubError::NotFound(format!("no proposal {id}")),
            e => e.into(),
        })?;
        Ok(page.into_iter().map(|p| proposal_view(p, view)).collect())
    }

    /// A person's decision. Accept writes `.mirante/memory/<slug>.md` (never overwriting;
    /// left uncommitted for the user) and records its exact content as reviewed; reject
    /// writes nothing. A failed write leaves the proposal pending.
    pub fn dna_review(&self, caller: &Caller, r: DnaReview) -> Result<ProposalView> {
        self.require_person(caller)?;
        let p = self.lock().store.dna_proposal(&r.id).map_err(|e| match e {
            CoreError::NotFound { .. } => HubError::NotFound(format!("no proposal {}", r.id)),
            e => e.into(),
        })?;
        if p.status != "pending" {
            return Err(HubError::Conflict(format!("proposal {} was already {}", p.id, p.status)));
        }
        let why = r.note.unwrap_or_default();
        let as_conflict = |e: CoreError| match e {
            CoreError::InvalidTransition { .. } => HubError::Conflict(format!("proposal {} was already decided", r.id)),
            e => e.into(),
        };
        if !r.accept {
            // Rejecting writes nothing, so it stays possible in a disabled repo.
            let mut st = self.lock();
            let d = st.store.dna_decide(&p.id, false, None, &why).map_err(as_conflict)?;
            st.publish();
            return Ok(d);
        }
        self.require_enabled(&p.repo)?;
        let repo = PathBuf::from(&p.repo);
        let (title, body, _) =
            clean_text(r.title.as_deref().unwrap_or(&p.title), r.body.as_deref().unwrap_or(&p.body))?;
        let kind = Kind::parse(&p.kind).ok_or_else(|| HubError::Invalid(format!("kind {:?}", p.kind)))?;
        let n = Note {
            id: new_id(),
            title,
            kind,
            status: Status::Active,
            created: today(),
            source: p.task_id.as_ref().map_or_else(|| "cli".to_string(), |t| format!("task:{t}")),
            anchors: recite_at_checkout(&repo, &p.anchors),
            body,
        };
        let text = note::render(&n).map_err(|e| HubError::Invalid(e.to_string()))?;
        fsio::init(&repo).map_err(dna_err)?;
        let slug = create_unique(&repo, &note::slugify(&n.title), &text).map_err(dna_err)?;
        let hash = sha256_hex(text.as_bytes());
        // The decision, the ledger entry and `dna.decided` commit together or not at all.
        let accepted = {
            let mut st = self.lock();
            let d = st.store.dna_accept(&p.id, &slug, &why, &hash).map_err(as_conflict);
            st.publish();
            d
        };
        let d = match accepted {
            Ok(d) => d,
            Err(e) => {
                // Not accepted (decided by someone else meanwhile, or the store failed): take
                // back the file this call wrote, through the directory descriptor, and only
                // if it is still our text.
                let _ = fsio::remove_note(&repo, &slug, &hash);
                return Err(e);
            }
        };
        if let Err(e) = self.dna_refresh(&repo) {
            self.lock().log_error(d.task_id.as_deref(), "dna reindex after accept", &e);
        }
        Ok(d)
    }
}

impl Hub {
    /// The task's memory: notes citing the files its branch changed (most cited first),
    /// then full-text hits on its title and goal (deduped, at most 10); the notes whose
    /// cited lines the branch changed (the per-task "Memory CI"); and its proposals (oldest
    /// first, at most 50). git runs without the hub lock.
    ///
    /// At risk means: an anchor that is fresh or relocated at the checkout's HEAD, in a file
    /// the branch changed, and stale at the branch tip. Only committed work counts:
    /// uncommitted edits in the worktree are not checked (an agent commits before it cites,
    /// and the tip is what would merge). An anchor already stale at the checkout is not the
    /// task's doing and is not listed.
    pub fn dna_task_context(&self, task_ref: &str) -> Result<DnaTaskContext> {
        self.dna_task_context_in(View::Person, task_ref)
    }

    /// [`Hub::dna_task_context`] in `view`: what is handed to an agent asks for
    /// [`View::Agent`].
    pub fn dna_task_context_in(&self, view: View, task_ref: &str) -> Result<DnaTaskContext> {
        let task = self.lock().resolve(task_ref)?;
        let repo = self.checkout(&task.repo_path)?;
        let key = key_of(&repo);
        let enabled = self.lock().store.dna_repo(&key)?.is_some_and(|r| r.enabled);
        if !enabled {
            return Ok(DnaTaskContext { enabled, relevant: Vec::new(), at_risk: Vec::new(), proposals: Vec::new() });
        }
        self.dna_ensure_fresh(&repo)?;
        let live = task.lifecycle != Lifecycle::Archived;
        let worktree = Path::new(&task.worktree_path);
        let files: Vec<String> = if live {
            let d = crate::diff::task_diff(&repo, worktree, &task.branch).map_err(HubError::Conflict)?;
            d.files.into_iter().map(|f| f.path).chain(d.untracked).collect()
        } else {
            Vec::new()
        };
        let (citing, hits, proposals) = {
            let st = self.lock();
            let slugs = st.store.dna_notes_citing(&key, &files)?;
            let mut citing = Vec::with_capacity(slugs.len());
            for slug in &slugs {
                citing.extend(st.store.dna_note(&key, slug)?);
            }
            let hits = match index::fts_query(&format!("{} {}", task.title, task.goal)) {
                Some(q) => st.store.dna_search(&key, &q, RELEVANT_MAX)?,
                None => Vec::new(),
            };
            let query = ProposalQuery {
                repo: Some(key.clone()),
                task_id: Some(task.id.clone()),
                limit: PAGE_DEFAULT,
                ..Default::default()
            };
            let proposals: Vec<ProposalView> =
                st.store.dna_proposals(&query)?.into_iter().map(|p| proposal_view(p, view)).collect();
            (citing, hits, proposals)
        };
        let mut relevant: Vec<NoteRow> = Vec::new();
        for n in citing.iter().chain(&hits) {
            if relevant.len() < RELEVANT_MAX && !relevant.iter().any(|r| r.slug == n.slug) {
                relevant.push(n.clone());
            }
        }
        // At risk: the anchors in changed files that hold at the checkout, checked at the
        // branch tip.
        let mut at_risk = Vec::new();
        let holds = |a: &IndexedAnchor| matches!(a.state, AnchorState::Fresh | AnchorState::Relocated { .. });
        let cited: Vec<(&NoteRow, &IndexedAnchor)> = citing
            .iter()
            .flat_map(|n| n.anchors.iter().filter(|a| files.contains(&a.anchor.path) && holds(a)).map(move |a| (n, a)))
            .collect();
        if live && !cited.is_empty() {
            let tip = rev_parse(worktree, "HEAD").map_err(dna_err)?;
            let anchors: Vec<_> = cited.iter().map(|(_, a)| a.anchor.clone()).collect();
            let checked = check_all(&repo, &tip, &anchors).map_err(dna_err)?;
            let mut reasons: Vec<(&NoteRow, Vec<String>)> = Vec::new();
            for ((n, a), c) in cited.iter().zip(checked) {
                if let AnchorState::Stale { reason } = c.state {
                    let line = format!("{}: {reason}", a.anchor.path);
                    match reasons.iter_mut().find(|(r, _)| r.slug == n.slug) {
                        Some((_, v)) => v.push(line),
                        None => reasons.push((n, vec![line])),
                    }
                }
            }
            let rows: Vec<NoteRow> = reasons.iter().map(|(n, _)| (*n).clone()).collect();
            let summaries = self.dna_summaries(&key, &rows, view)?;
            at_risk = summaries
                .into_iter()
                .zip(reasons)
                .map(|(note, (_, why))| AtRisk { note, reason: view.line(&why.join("; "), MAX_FIELD * 4) })
                .collect();
        }
        Ok(DnaTaskContext { enabled, relevant: self.dna_summaries(&key, &relevant, view)?, at_risk, proposals })
    }

    /// A person's action on a note in the checkout. Confirm re-cites every anchor at the
    /// checkout's HEAD (each must be fresh or relocated); retire sets `status: retired`.
    /// Both rewrite only the frontmatter lines they own, need the hash the person saw, never
    /// delete, and record the new content as reviewed.
    pub fn dna_note_action(&self, caller: &Caller, a: DnaNoteAction) -> Result<NoteView> {
        self.require_person(caller)?;
        let repo = self.dna_repo_of(&DnaRef { repo: Some(a.repo.clone()), task: None })?;
        self.require_enabled(&key_of(&repo))?;
        let text = fsio::read_note(&repo, &a.slug).map_err(dna_err)?;
        if sha256_hex(text.as_bytes()) != a.expect_hash {
            return Err(dna_err(DnaError::Changed));
        }
        let n = note::parse(&text).map_err(|e| HubError::Conflict(format!("note {} does not parse: {e}", a.slug)))?;
        let updated = match a.action {
            NoteAction::Confirm => {
                let head = rev_parse(&repo, "HEAD").map_err(dna_err)?;
                let checked = check_all(&repo, &head, &n.anchors).map_err(dna_err)?;
                let mut anchors = Vec::with_capacity(checked.len());
                for (i, c) in checked.iter().enumerate() {
                    let (start, end) = match &c.state {
                        AnchorState::Fresh => (c.anchor.start, c.anchor.end),
                        AnchorState::Relocated { start, end } => (*start, *end),
                        other => {
                            return Err(HubError::Conflict(format!(
                                "anchor {} is {}: re-cite it or retire the note",
                                i + 1,
                                state_name(other)
                            )))
                        }
                    };
                    let symbol = c.anchor.symbol.as_deref();
                    anchors.push(cite(&repo, &head, &c.anchor.path, symbol, start, end).map_err(dna_err)?);
                }
                note::update(&text, None, Some(&anchors))
            }
            NoteAction::Retire => note::update(&text, Some(Status::Retired), None),
        }
        .map_err(dna_err)?;
        fsio::update_note(&repo, &a.slug, &a.expect_hash, &updated).map_err(dna_err)?;
        let key = key_of(&repo);
        {
            // The ledger entry and its `dna.changed` event commit together.
            let mut st = self.lock();
            st.store.dna_record_review(&key, &a.slug, &sha256_hex(updated.as_bytes()), "confirmed")?;
            st.publish();
        }
        self.dna_refresh(&repo)?;
        self.dna_view(&repo, &a.slug, View::Person)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::process::Command;

    use mirante_core::dna::check::{cite, rev_parse};
    use mirante_core::dna::note::{render, Kind, Note, Status};
    use mirante_core::dna::sha256_hex;
    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::types::{
        DnaGet, DnaNoteAction, DnaProposals, DnaPropose, DnaRef, DnaReview, DnaSearch, NoteAction, ProposeAnchor,
    };
    use serde_json::Value;

    use super::*;
    use crate::driver::proctree::tests::sleeper;
    use crate::driver::proctree::{proc_of, Proc};
    use crate::hub::testkit::Kit;
    use crate::hub::HubError;
    use mirante_core::claude_launch::{AGENT_ENV, TASK_ENV};

    impl Kit {
        pub fn repo_ref(&self) -> DnaRef {
            DnaRef { repo: Some(self.repo.to_string_lossy().into_owned()), task: None }
        }

        /// Every event (repo-level ones too) of this kind, payloads in order.
        pub fn payloads(&self, kind: &str) -> Vec<Value> {
            let st = self.hub.lock();
            st.store
                .events_since(0, 10_000)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.payload)
                .collect()
        }
    }

    pub fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    pub fn commit_in(dir: impl AsRef<Path>, path: &str, text: &str) {
        let dir = dir.as_ref();
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        git(dir, &["add", "-A", "--", path]);
        git(dir, &["commit", "-q", "-m", path]);
    }

    pub fn head_of(dir: impl AsRef<Path>) -> String {
        rev_parse(dir.as_ref(), "HEAD").unwrap()
    }

    pub const CODE: &str = "fn keep() {}\nfn spool() {\n  write();\n}\nfn other() {}\n";

    /// A note's text citing `path` lines `start..=end` at the repo's HEAD.
    pub fn note_text(repo: &Path, title: &str, body: &str, path: &str, start: u32, end: u32) -> String {
        let a = cite(repo, &head_of(repo), path, Some("spool"), start, end).unwrap();
        render(&Note {
            id: "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55".into(),
            title: title.into(),
            kind: Kind::Gotcha,
            status: Status::Active,
            created: "2026-11-07".into(),
            source: "manual".into(),
            anchors: vec![a],
            body: format!("{body}\n"),
        })
        .unwrap()
    }

    pub fn write_note(repo: &Path, slug: &str, text: &str) {
        std::fs::write(repo.join(format!(".mirante/memory/{slug}.md")), text).unwrap();
    }

    /// Enabled memory with `src/a.rs` committed and two notes on disk: `hook-spool` (links
    /// `[[other]]` and `[[gone]]`) citing a.rs L2-4 and `other` citing a.rs L5.
    pub fn with_two_notes(k: &Kit) {
        commit_in(&k.repo, "src/a.rs", CODE);
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let spool =
            note_text(&k.repo, "Hook spool fallback", "Falls back. See [[other]] and [[gone]].", "src/a.rs", 2, 4);
        write_note(&k.repo, "hook-spool", &spool);
        write_note(&k.repo, "other", &note_text(&k.repo, "Other thing", "Unrelated.", "src/a.rs", 5, 5));
    }

    pub fn propose(task: &str, title: &str, body: &str, path: &str, start: u32, end: u32) -> DnaPropose {
        DnaPropose {
            task: task.into(),
            title: title.into(),
            kind: "gotcha".into(),
            body: body.into(),
            anchors: vec![ProposeAnchor { path: path.into(), symbol: Some("spool".into()), start, end }],
        }
    }

    fn review_accept(id: &str) -> DnaReview {
        DnaReview { id: id.into(), accept: true, ..Default::default() }
    }

    /// A started session whose fake driver reports `root` as its process-tree root.
    pub fn started(k: &Kit, branch: &str, root: Proc) -> mirante_core::model::Task {
        *k.factory.driver.root.lock().unwrap() = Some(root);
        k.started(AgentKind::Claude, branch).task
    }

    /// A live process that is nobody's agent: no agent env marker, outside every worktree.
    /// Killed when dropped.
    pub struct Peer(pub std::process::Child);

    impl Peer {
        pub fn spawn(set: &[(&str, &str)], cwd: &Path) -> Peer {
            Peer(sleeper(set, &[AGENT_ENV, TASK_ENV], cwd))
        }

        pub fn proc(&self) -> Proc {
            proc_of(self.0.id() as i32).expect("the peer is alive")
        }

        pub fn caller(&self) -> Caller {
            Caller::Peer(self.proc())
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn forbidden_with(r: Result<impl std::fmt::Debug>, needle: &str) {
        match r {
            Err(HubError::Forbidden(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// Enabled memory and a task whose worktree has `src/a.rs` committed.
    fn proposing(k: &Kit, branch: &str) -> mirante_core::model::Task {
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let t = k.create(AgentKind::Claude, branch).task;
        commit_in(&t.worktree_path, "src/a.rs", CODE);
        t
    }

    #[test]
    fn propose_validates_cites_commits_and_redacts() {
        let k = Kit::new();
        let t = proposing(&k, "mem");
        let p = k
            .hub
            .dna_propose(
                &Caller::Person,
                propose(&t.id, "Spool", "uses TOKEN=sk-ant-api03-abcdefghijklmnopqrstu", "src/a.rs", 2, 4),
            )
            .unwrap();
        assert_eq!((p.status.as_str(), p.redactions, p.origin.as_str()), ("pending", 1, "cli"));
        assert!(!p.body.contains("sk-ant"), "{}", p.body);
        assert_eq!(p.anchors[0].commit, head_of(&t.worktree_path));
        assert_eq!((p.anchors[0].start, p.anchors[0].end, p.anchors[0].symbol.as_deref()), (2, 4, Some("spool")));
        assert_eq!(p.task_id.as_deref(), Some(t.id.as_str()));
        assert!(k.kinds(&t.id).contains(&"dna.proposed".to_string()));
        let queue = k
            .hub
            .dna_proposals(DnaProposals { repo: Some(k.repo.to_string_lossy().into()), ..Default::default() })
            .unwrap();
        assert_eq!(queue.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), [p.id.as_str()]);
        assert_eq!(k.hub.dna_status(&k.repo_ref()).unwrap().pending, 1);
    }

    #[test]
    fn propose_refuses_uncited_dirty_sensitive_out_of_range_and_floods() {
        let k = Kit::new();
        let t0 = k.create(AgentKind::Claude, "off").task;
        let off = k.hub.dna_propose(&Caller::Person, propose(&t0.id, "T", "b", "README", 1, 1));
        assert!(
            matches!(&off, Err(HubError::Conflict(m)) if m.contains("memory is not enabled for this repo")),
            "{off:?}"
        );

        let t = proposing(&k, "mem");
        let invalid = |p: DnaPropose| match k.hub.dna_propose(&Caller::Person, p) {
            Err(HubError::Invalid(m)) => m,
            other => panic!("{other:?}"),
        };
        let mut none = propose(&t.id, "T", "b", "src/a.rs", 1, 1);
        none.anchors.clear();
        invalid(none);
        std::fs::write(Path::new(&t.worktree_path).join("src/a.rs"), "changed\n").unwrap();
        assert!(invalid(propose(&t.id, "T", "b", "src/a.rs", 1, 1)).contains("commit"));
        git(Path::new(&t.worktree_path), &["checkout", "--", "src/a.rs"]);
        commit_in(&t.worktree_path, ".env", "SECRET=1\n");
        invalid(propose(&t.id, "T", "b", ".env", 1, 1));
        invalid(propose(&t.id, "T", "b", "src/a.rs", 9, 10));
        invalid(propose(&t.id, "T", &"x".repeat(16 * 1024 + 1), "src/a.rs", 1, 1));
        invalid(propose(&t.id, &"t".repeat(121), "b", "src/a.rs", 1, 1));
        let mut kind = propose(&t.id, "T", "b", "src/a.rs", 1, 1);
        kind.kind = "rumor".into();
        invalid(kind);
        for _ in 0..20 {
            k.hub.dna_propose(&Caller::Person, propose(&t.id, "T", "b", "src/a.rs", 1, 1)).unwrap();
        }
        let flood = k.hub.dna_propose(&Caller::Person, propose(&t.id, "T", "b", "src/a.rs", 1, 1));
        assert!(matches!(flood, Err(HubError::Conflict(_))), "{flood:?}");
    }

    #[test]
    fn accept_writes_the_note_records_review_and_indexes_it() {
        let k = Kit::new();
        let t = proposing(&k, "mem");
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "Spool", "Falls back.", "src/a.rs", 2, 4)).unwrap();
        let edited =
            DnaReview { title: Some("Hook spool fallback".into()), note: Some("ok".into()), ..review_accept(&p.id) };
        let d = k.hub.dna_review(&Caller::Person, edited).unwrap();
        assert_eq!(
            (d.status.as_str(), d.slug.as_deref(), d.decision_note.as_str()),
            ("accepted", Some("hook-spool-fallback"), "ok")
        );
        let file = k.repo.join(".mirante/memory/hook-spool-fallback.md");
        let text = std::fs::read_to_string(&file).unwrap();
        let n = mirante_core::dna::note::parse(&text).unwrap();
        assert_eq!(
            (n.title.as_str(), n.anchors.clone(), n.source.clone()),
            ("Hook spool fallback", p.anchors.clone(), format!("task:{}", t.id))
        );
        assert_eq!(n.body, "Falls back.\n");
        let key = k.repo.canonicalize().unwrap().to_string_lossy().into_owned();
        assert!(k.hub.lock().store.dna_is_reviewed(&key, "hook-spool-fallback", &sha256_hex(text.as_bytes())).unwrap());
        assert_eq!(k.kinds(&t.id).iter().filter(|e| *e == "dna.decided").count(), 1);
        let v = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "hook-spool-fallback".into() }).unwrap();
        assert!(v.summary.reviewed);
        let again = k.hub.dna_review(&Caller::Person, review_accept(&p.id));
        assert!(matches!(again, Err(HubError::Conflict(_))), "{again:?}");
        // Never committed for the user.
        let log = Command::new("git").arg("-C").arg(&k.repo).args(["status", "--porcelain"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains(".mirante/"));
    }

    #[test]
    fn accept_recites_anchors_whose_code_is_already_in_the_checkout() {
        let k = Kit::new();
        commit_in(&k.repo, "src/a.rs", CODE);
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let t = k.create(AgentKind::Claude, "recite").task;
        commit_in(&t.worktree_path, "src/z.rs", "z\n");
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "Spool", "b", "src/a.rs", 2, 4)).unwrap();
        assert_eq!(p.anchors[0].commit, head_of(&t.worktree_path));
        let d = k.hub.dna_review(&Caller::Person, review_accept(&p.id)).unwrap();
        let text = std::fs::read_to_string(k.repo.join(format!(".mirante/memory/{}.md", d.slug.unwrap()))).unwrap();
        let n = mirante_core::dna::note::parse(&text).unwrap();
        assert_eq!((n.anchors[0].commit.clone(), n.anchors[0].start), (head_of(&k.repo), 2));
        assert_eq!(n.anchors[0].fingerprint, p.anchors[0].fingerprint);
    }

    #[test]
    fn accept_and_note_actions_need_memory_enabled() {
        let k = Kit::new();
        with_a_and_b(&k);
        let t = k.create(AgentKind::Claude, "mem").task;
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "Late", "b", "src/a.rs", 1, 1)).unwrap();
        let b = view(&k, "b");
        k.hub.dna_disable(&k.repo_ref(), &Caller::Person).unwrap();
        let acc = k.hub.dna_review(&Caller::Person, review_accept(&p.id));
        assert!(matches!(&acc, Err(HubError::Conflict(m)) if m.contains("not enabled")), "{acc:?}");
        assert_eq!(k.hub.lock().store.dna_proposal(&p.id).unwrap().status, "pending");
        assert!(!k.repo.join(".mirante/memory/late.md").exists());
        let act = k.hub.dna_note_action(&Caller::Person, action(&k, "b", NoteAction::Retire, &b.content_hash));
        assert!(matches!(&act, Err(HubError::Conflict(m)) if m.contains("not enabled")), "{act:?}");
        // Rejecting writes nothing, so it stays possible.
        let r = DnaReview { id: p.id.clone(), accept: false, ..Default::default() };
        assert_eq!(k.hub.dna_review(&Caller::Person, r).unwrap().status, "rejected");
    }

    #[test]
    fn accept_slug_collision_gets_a_suffix_and_never_overwrites() {
        let k = Kit::new();
        let t = proposing(&k, "mem");
        write_note(&k.repo, "a", "mine");
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "A", "b", "src/a.rs", 1, 1)).unwrap();
        let d = k.hub.dna_review(&Caller::Person, review_accept(&p.id)).unwrap();
        assert_eq!(d.slug.as_deref(), Some("a-2"));
        assert_eq!(std::fs::read_to_string(k.repo.join(".mirante/memory/a.md")).unwrap(), "mine");
        assert!(k.repo.join(".mirante/memory/a-2.md").exists());
    }

    #[test]
    fn reject_writes_nothing() {
        let k = Kit::new();
        let t = proposing(&k, "mem");
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "Nope", "b", "src/a.rs", 1, 1)).unwrap();
        let before: Vec<_> = std::fs::read_dir(k.repo.join(".mirante/memory")).unwrap().collect();
        let d = k
            .hub
            .dna_review(&Caller::Person, DnaReview { id: p.id.clone(), accept: false, ..Default::default() })
            .unwrap();
        assert_eq!((d.status.as_str(), d.slug), ("rejected", None));
        let after: Vec<_> = std::fs::read_dir(k.repo.join(".mirante/memory")).unwrap().collect();
        assert_eq!(before.len(), after.len());
        assert!(k.kinds(&t.id).contains(&"dna.decided".to_string()));
    }

    #[test]
    fn review_and_note_actions_are_refused_from_an_agent_tree() {
        let k = Kit::new();
        let root = Peer::spawn(&[], k.dir.path());
        let t = started(&k, "g", root.proc());
        let caller = root.caller();
        assert_eq!(k.hub.identify(&caller), Identity::Agent { task: Some(t.id.clone()) });
        forbidden_with(k.hub.dna_review(&caller, review_accept("x")), "Mirante-launched agents are refused");
        assert!(matches!(k.hub.dna_enable(&k.repo_ref(), &caller), Err(HubError::Forbidden(_))));
        assert!(matches!(k.hub.dna_disable(&k.repo_ref(), &caller), Err(HubError::Forbidden(_))));
        assert!(matches!(k.hub.dna_review(&Caller::Unknown, review_accept("x")), Err(HubError::Forbidden(_))));
        let act = DnaNoteAction {
            repo: k.repo.to_string_lossy().into(),
            slug: "a".into(),
            action: NoteAction::Retire,
            expect_hash: "h".into(),
        };
        assert!(matches!(k.hub.dna_note_action(&caller, act), Err(HubError::Forbidden(_))));
        // A process that is no Mirante-launched agent is a person (the review then fails on the id).
        let person = Peer::spawn(&[], k.dir.path());
        assert_eq!(k.hub.identify(&person.caller()), Identity::Person);
        let r = k.hub.dna_review(&person.caller(), review_accept("x"));
        assert!(matches!(r, Err(HubError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn a_process_working_in_a_worktree_counts_as_an_agent() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "orphan").task;
        let orphan = Peer::spawn(&[], Path::new(&t.worktree_path));
        assert_eq!(k.hub.identify(&orphan.caller()), Identity::Agent { task: None });
        assert!(matches!(k.hub.dna_enable(&k.repo_ref(), &orphan.caller()), Err(HubError::Forbidden(_))));
    }

    #[test]
    fn an_agent_that_left_its_process_tree_is_known_by_its_env_marker() {
        let k = Kit::new();
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let mine = k.create(AgentKind::Claude, "mine").task;
        commit_in(&mine.worktree_path, "src/a.rs", CODE);
        let other = k.create(AgentKind::Claude, "other").task;
        commit_in(&other.worktree_path, "src/a.rs", CODE);
        // Double-forked, `cd /`, after a daemon restart: no live tree, no worktree cwd.
        let escaped = Peer::spawn(&[(AGENT_ENV, &mine.id)], k.dir.path());
        let caller = escaped.caller();
        assert_eq!(k.hub.identify(&caller), Identity::Agent { task: Some(mine.id.clone()) });
        forbidden_with(k.hub.dna_review(&caller, review_accept("x")), "Mirante-launched agents are refused");
        forbidden_with(k.hub.dna_enable(&k.repo_ref(), &caller), "Mirante-launched agents are refused");
        let claimed = k.hub.dna_propose(&caller, propose(&other.id, "T", "b", "src/a.rs", 1, 1));
        forbidden_with(claimed, "its own task");
        let p = k.hub.dna_propose(&caller, propose(&mine.id, "T", "b", "src/a.rs", 1, 1)).unwrap();
        assert_eq!(p.origin, "agent");
        // Claude's older marker counts too; markers naming different tasks name none.
        let claude = Peer::spawn(&[(TASK_ENV, &mine.id)], k.dir.path());
        assert_eq!(k.hub.identify(&claude.caller()), Identity::Agent { task: Some(mine.id.clone()) });
        let mixed = Peer::spawn(&[(TASK_ENV, &mine.id), (AGENT_ENV, &other.id)], k.dir.path());
        assert_eq!(k.hub.identify(&mixed.caller()), Identity::Agent { task: None });
        forbidden_with(k.hub.dna_propose(&mixed.caller(), propose(&mine.id, "T", "b", "src/a.rs", 1, 1)), "task");
    }

    /// The test binary sleeping on a fresh PTY (its controlling terminal), with no agent
    /// env marker, outside every worktree.
    fn on_a_pty(cwd: &Path) -> Arc<crate::driver::pty::PtyProcess> {
        let spec = crate::driver::pty::PtySpec {
            program: std::env::current_exe().unwrap(),
            args: ["--exact", "driver::proctree::tests::sleeper_child", "--ignored", "--test-threads=1", "-q"]
                .map(String::from)
                .to_vec(),
            cwd: cwd.to_path_buf(),
            env_remove: vec![AGENT_ENV.into(), TASK_ENV.into()],
            env_set: vec![("MIRANTE_SLEEPER".into(), "1".into())],
            rows: 24,
            cols: 80,
        };
        crate::driver::pty::PtyProcess::spawn(&spec, |_| {}).unwrap()
    }

    #[test]
    fn a_process_on_a_live_agents_terminal_counts_as_that_agent() {
        let k = Kit::new();
        let pty = on_a_pty(k.dir.path());
        let caller = Caller::Peer(pty.root().unwrap());
        // No live session has that terminal yet: a person.
        assert_eq!(k.hub.identify(&caller), Identity::Person);
        assert!(k.hub.require_person(&caller).is_ok());
        // An agent session on it (a scrubbed env, outside its tree and its worktree: e.g. a
        // tool call that ran `env -i setsid …` but kept the terminal).
        *k.factory.driver.tty.lock().unwrap() = pty.tty();
        let t = k.started(AgentKind::Claude, "tty").task;
        assert_eq!(k.hub.identify(&caller), Identity::Agent { task: Some(t.id.clone()) });
        forbidden_with(k.hub.dna_review(&caller, review_accept("x")), "Mirante-launched agents are refused");
        forbidden_with(k.hub.require_person(&caller), "Mirante-launched agents are refused");
    }

    #[test]
    fn an_agent_reads_only_its_own_tasks_memory_and_only_while_it_is_enabled() {
        let k = Kit::new();
        with_two_notes(&k);
        let mine = k.create(AgentKind::Claude, "mine").task;
        let other = k.create(AgentKind::Claude, "other").task;
        let agent = Peer::spawn(&[(AGENT_ENV, &mine.id)], k.dir.path());
        let caller = agent.caller();
        let at = |task: Option<&str>, repo: bool| DnaRef {
            task: task.map(str::to_string),
            repo: repo.then(|| k.repo.to_string_lossy().into_owned()),
        };
        let search =
            |c: &Caller, at: DnaRef| k.hub.dna_search_from(c, DnaSearch { at, query: "spool".into(), limit: None });
        let get = |c: &Caller, at: DnaRef| k.hub.dna_get_from(c, DnaGet { at, slug: "hook-spool".into() });
        // Its own task (by id or by branch): the agent view.
        let hits = search(&caller, at(Some(&mine.id), false)).unwrap();
        assert_eq!(hits[0].slug, "hook-spool");
        assert_eq!(get(&caller, at(Some("mine"), false)).unwrap().summary.slug, "hook-spool");
        // Another task, a repo path, or both: refused.
        forbidden_with(search(&caller, at(Some(&other.id), false)), "its own task");
        forbidden_with(get(&caller, at(Some(&other.id), false)), "its own task");
        forbidden_with(search(&caller, at(None, true)), "its own task");
        forbidden_with(get(&caller, at(Some(&mine.id), true)), "its own task");
        // An agent whose task is unknown, or a caller that could not be checked: refused.
        let lost = Peer::spawn(&[(TASK_ENV, &mine.id), (AGENT_ENV, &other.id)], k.dir.path());
        forbidden_with(search(&lost.caller(), at(Some(&mine.id), false)), "task is unknown");
        forbidden_with(get(&Caller::Unknown, at(Some(&mine.id), false)), "could not read");
        // A person reads any repo or task (the cockpit, the CLI).
        assert_eq!(search(&Caller::Person, at(None, true)).unwrap().len(), 1);
        assert_eq!(get(&Caller::Person, at(Some(&other.id), false)).unwrap().summary.slug, "hook-spool");
        // Disabling the repo stops a running session's reads; a person still reads.
        k.hub.dna_disable(&k.repo_ref(), &Caller::Person).unwrap();
        let off = search(&caller, at(Some(&mine.id), false));
        assert!(matches!(&off, Err(HubError::Conflict(m)) if m.contains("not enabled")), "{off:?}");
        assert!(matches!(get(&caller, at(Some(&mine.id), false)), Err(HubError::Conflict(_))));
        assert_eq!(search(&Caller::Person, at(None, true)).unwrap().len(), 1);
    }

    #[test]
    fn a_caller_that_exited_or_whose_pid_was_reused_is_refused() {
        let k = Kit::new();
        let gone = {
            let p = Peer::spawn(&[], k.dir.path());
            p.proc()
        }; // killed and reaped
        forbidden_with(k.hub.dna_review(&Caller::Peer(gone), review_accept("x")), "exited");
        let me = proc_of(std::process::id() as i32).unwrap();
        let reused = Caller::Peer(Proc { pid: me.pid, start: me.start + 1 });
        forbidden_with(k.hub.dna_review(&reused, review_accept("x")), "exited");
        forbidden_with(k.hub.dna_review(&Caller::Unknown, review_accept("x")), "terminal you opened yourself");
    }

    #[test]
    fn a_caller_whose_environment_cannot_be_read_is_refused_with_what_to_do() {
        let k = Kit::new();
        // Another user's process (init on Linux; macOS does not even show launchd's start
        // time to us): its environment is not readable.
        if let Some(init) = proc_of(1).filter(|_| unsafe { libc::getuid() } != 0) {
            forbidden_with(k.hub.dna_review(&Caller::Peer(init), review_accept("x")), "terminal you opened yourself");
        }
        // macOS hides a platform binary's environment (`nc -U` on the socket).
        #[cfg(target_os = "macos")]
        {
            let mut nc = Command::new("/bin/sleep").arg("30").current_dir(k.dir.path()).spawn().unwrap();
            let caller = Caller::Peer(proc_of(nc.id() as i32).unwrap());
            assert!(matches!(k.hub.identify(&caller), Identity::Unverified(_)));
            forbidden_with(k.hub.dna_review(&caller, review_accept("x")), "environment");
            let _ = nc.kill();
            let _ = nc.wait();
        }
    }

    #[test]
    fn propose_from_an_agent_tree_must_name_its_own_task() {
        let k = Kit::new();
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let root = Peer::spawn(&[], k.dir.path());
        let mine = started(&k, "mine", root.proc());
        commit_in(&mine.worktree_path, "src/a.rs", CODE);
        let other = k.create(AgentKind::Claude, "other").task;
        commit_in(&other.worktree_path, "src/a.rs", CODE);
        let caller = root.caller();
        let claimed = k.hub.dna_propose(&caller, propose(&other.id, "T", "b", "src/a.rs", 1, 1));
        assert!(matches!(claimed, Err(HubError::Forbidden(_))), "{claimed:?}");
        let p = k.hub.dna_propose(&caller, propose(&mine.id, "T", "b", "src/a.rs", 1, 1)).unwrap();
        assert_eq!(p.origin, "agent");
        let unknown = k.hub.dna_propose(&Caller::Unknown, propose(&mine.id, "T", "b", "src/a.rs", 1, 1));
        assert!(matches!(unknown, Err(HubError::Forbidden(_))), "{unknown:?}");
    }

    #[test]
    fn a_failed_write_leaves_the_proposal_pending() {
        let k = Kit::with_store(|_| Store::open_in_memory().unwrap());
        let t = proposing(&k, "mem");
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "T", "b", "src/a.rs", 1, 1)).unwrap();
        let mem = k.repo.join(".mirante/memory");
        std::fs::rename(&mem, k.repo.join(".mirante/elsewhere")).unwrap();
        std::os::unix::fs::symlink(k.repo.join(".mirante/elsewhere"), &mem).unwrap();
        assert!(k.hub.dna_review(&Caller::Person, review_accept(&p.id)).is_err());
        assert_eq!(k.hub.lock().store.dna_proposal(&p.id).unwrap().status, "pending");
    }

    fn key(k: &Kit) -> String {
        k.repo.canonicalize().unwrap().to_string_lossy().into_owned()
    }

    fn action(k: &Kit, slug: &str, action: NoteAction, expect_hash: &str) -> DnaNoteAction {
        DnaNoteAction {
            repo: k.repo.to_string_lossy().into(),
            slug: slug.into(),
            action,
            expect_hash: expect_hash.into(),
        }
    }

    fn view(k: &Kit, slug: &str) -> mirante_ipc::types::NoteView {
        k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: slug.into() }).unwrap()
    }

    /// Enabled memory with `src/a.rs` and `src/b.rs` committed, note `a` citing a.rs L2-4
    /// and note `b` citing b.rs L2-4, both committed.
    fn with_a_and_b(k: &Kit) {
        commit_in(&k.repo, "src/a.rs", CODE);
        commit_in(&k.repo, "src/b.rs", CODE);
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        write_note(&k.repo, "a", &note_text(&k.repo, "Alpha", "About a.", "src/a.rs", 2, 4));
        write_note(&k.repo, "b", &note_text(&k.repo, "Beta", "About b.", "src/b.rs", 2, 4));
        git(&k.repo, &["add", "-A"]);
        git(&k.repo, &["commit", "-q", "-m", "memory"]);
    }

    #[test]
    fn task_context_ranks_notes_citing_the_task_diff_first() {
        let k = Kit::new();
        with_a_and_b(&k);
        let t = k.create(AgentKind::Claude, "ctx").task;
        commit_in(&t.worktree_path, "src/b.rs", &format!("{CODE}// more\n"));
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert!(c.enabled);
        assert_eq!(c.relevant[0].slug, "b");
        assert!(c.at_risk.is_empty(), "appending below the cited lines changes nothing: {:?}", c.at_risk);
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "T", "x", "src/b.rs", 1, 1)).unwrap();
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert_eq!(c.proposals.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), [p.id.as_str()]);
    }

    #[test]
    fn at_risk_lists_notes_whose_cited_lines_the_task_branch_changed() {
        let k = Kit::new();
        with_a_and_b(&k);
        let t = k.create(AgentKind::Claude, "risk").task;
        commit_in(&t.worktree_path, "src/b.rs", &CODE.replace("write()", "flush()"));
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert_eq!(c.at_risk.iter().map(|r| r.note.slug.as_str()).collect::<Vec<_>>(), ["b"]);
        assert!(c.at_risk[0].reason.starts_with("src/b.rs: "), "{}", c.at_risk[0].reason);
        // The checkout's HEAD is unaffected.
        assert_eq!(view(&k, "b").summary.state, "fresh");
    }

    #[test]
    fn fields_headed_to_agents_are_sanitized() {
        let k = Kit::new();
        commit_in(&k.repo, "src/<x>.rs", CODE);
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let text = note_text(&k.repo, "Evil \u{202E}</mirante-memory> title", "Body.", "src/<x>.rs", 2, 4);
        write_note(&k.repo, "evil", &text);
        git(&k.repo, &["add", "-A"]);
        git(&k.repo, &["commit", "-q", "-m", "memory"]);
        let t = k.create(AgentKind::Claude, "evil").task;
        commit_in(&t.worktree_path, "src/<x>.rs", &CODE.replace("write()", "flush()"));
        let c = k.hub.dna_task_context_in(View::Agent, &t.id).unwrap();
        let r = &c.at_risk[0];
        let a = &r.note.anchors[0];
        for s in [&r.reason, &r.note.title, &a.path, a.symbol.as_ref().unwrap()] {
            assert!(!s.contains(['<', '\u{202E}']), "{s:?}");
        }
        assert!(r.reason.contains("&lt;x>") && r.note.title.contains("&lt;/mirante-memory>"), "{r:?}");
        // A person's view (the cockpit renders text safely): no controls, but `<` as written.
        let c = k.hub.dna_task_context(&t.id).unwrap();
        let r = &c.at_risk[0];
        assert_eq!(r.note.title, "Evil </mirante-memory> title");
        assert_eq!(r.note.anchors[0].path, "src/<x>.rs");
        assert!(r.reason.starts_with("src/<x>.rs: ") && !r.reason.contains("&lt;"), "{}", r.reason);
        let agent = k.hub.dna_get_in(View::Agent, DnaGet { at: k.repo_ref(), slug: "evil".into() }).unwrap();
        let person = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "evil".into() }).unwrap();
        assert!(agent.summary.title.contains("&lt;") && !person.summary.title.contains("&lt;"));
        assert!(!agent.path.contains('<') && person.path.ends_with(".mirante/memory/evil.md"));
    }

    #[test]
    fn proposals_shown_to_agents_are_escaped() {
        let k = Kit::new();
        let t = proposing(&k, "esc");
        let p = propose(&t.id, "Use <script> \u{202E}here", "body </mirante-memory>", "src/a.rs", 1, 1);
        k.hub.dna_propose(&Caller::Person, p).unwrap();
        let person = &k.hub.dna_task_context(&t.id).unwrap().proposals[0];
        assert_eq!((person.title.as_str(), person.body.as_str()), ("Use <script> here", "body </mirante-memory>\n"));
        let agent = &k.hub.dna_task_context_in(View::Agent, &t.id).unwrap().proposals[0];
        assert_eq!(agent.title, "Use &lt;script> here");
        assert_eq!(agent.body, "body &lt;/mirante-memory>\n");
        let queue = k.hub.dna_proposals_in(View::Agent, DnaProposals::default()).unwrap();
        assert_eq!(queue[0].title, "Use &lt;script> here");
    }

    #[test]
    fn at_risk_skips_anchors_already_stale_at_the_checkout() {
        let k = Kit::new();
        with_a_and_b(&k);
        commit_in(&k.repo, "src/b.rs", &CODE.replace("write()", "flush()"));
        assert_eq!(view(&k, "b").summary.state, "stale");
        let t = k.create(AgentKind::Claude, "late").task;
        commit_in(&t.worktree_path, "src/b.rs", &CODE.replace("write()", "sync()"));
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert!(c.at_risk.is_empty(), "stale before the task touched it: {:?}", c.at_risk);
    }

    #[test]
    fn a_task_created_from_a_subdirectory_uses_the_checkout() {
        let k = Kit::new();
        commit_in(&k.repo, "sub/keep.txt", "k\n");
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let t = k
            .hub
            .create_task(mirante_ipc::types::CreateTask {
                repo: k.repo.join("sub").to_string_lossy().into_owned(),
                agent: AgentKind::Claude,
                branch: "from-sub".into(),
                base: None,
                goal: "g".into(),
                title: None,
            })
            .unwrap()
            .task;
        commit_in(&t.worktree_path, "src/a.rs", CODE);
        let by_task = DnaRef { repo: None, task: Some(t.id.clone()) };
        assert_eq!(k.hub.dna_status(&by_task).unwrap().repo, key(&k));
        let p = k.hub.dna_propose(&Caller::Person, propose(&t.id, "T", "b", "src/a.rs", 1, 1)).unwrap();
        assert_eq!(p.repo, key(&k));
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert!(c.enabled && c.proposals.len() == 1);
    }

    #[test]
    fn the_queue_defaults_to_pending_filters_by_task_and_pages() {
        let k = Kit::new();
        let t = proposing(&k, "mem");
        let u = k.create(AgentKind::Claude, "other").task;
        commit_in(&u.worktree_path, "src/a.rs", CODE);
        let a = k.hub.dna_propose(&Caller::Person, propose(&t.id, "A", "b", "src/a.rs", 1, 1)).unwrap().id;
        let b = k.hub.dna_propose(&Caller::Person, propose(&u.id, "B", "b", "src/a.rs", 1, 1)).unwrap().id;
        let c = k.hub.dna_propose(&Caller::Person, propose(&t.id, "C", "b", "src/a.rs", 1, 1)).unwrap().id;
        k.hub.dna_review(&Caller::Person, DnaReview { id: a.clone(), accept: false, ..Default::default() }).unwrap();
        let ids = |q: DnaProposals| k.hub.dna_proposals(q).unwrap().into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(DnaProposals::default()), [b.clone(), c.clone()]);
        assert_eq!(
            ids(DnaProposals { status: Some("all".into()), ..Default::default() }),
            [a.clone(), b.clone(), c.clone()]
        );
        assert_eq!(ids(DnaProposals { task: Some("mem".into()), ..Default::default() }), [c.as_str()]);
        assert_eq!(ids(DnaProposals { limit: Some(1), ..Default::default() }), [b.as_str()]);
        assert_eq!(ids(DnaProposals { after: Some(b.clone()), ..Default::default() }), [c.as_str()]);
        assert_eq!(ids(DnaProposals { id: Some(a.clone()), status: Some("all".into()), ..Default::default() }), [a]);
        let bad = k.hub.dna_proposals(DnaProposals { status: Some("bogus".into()), ..Default::default() });
        assert!(matches!(bad, Err(HubError::Invalid(_))), "{bad:?}");
        let gone = k.hub.dna_proposals(DnaProposals { after: Some("nope".into()), ..Default::default() });
        assert!(matches!(gone, Err(HubError::NotFound(_))), "{gone:?}");
    }

    #[test]
    fn context_of_a_repo_without_memory_says_disabled() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "off").task;
        let c = k.hub.dna_task_context(&t.id).unwrap();
        assert!(!c.enabled && c.relevant.is_empty() && c.at_risk.is_empty() && c.proposals.is_empty());
    }

    #[test]
    fn confirm_reanchors_to_head_and_records_review() {
        let k = Kit::new();
        with_a_and_b(&k);
        let path = k.repo.join(".mirante/memory/a.md");
        let text = std::fs::read_to_string(&path).unwrap().replace("source: manual\n", "source: manual\ntags: [x]\n");
        std::fs::write(&path, &text).unwrap();
        commit_in(&k.repo, "src/a.rs", &format!("// moved\n{CODE}"));
        let before = view(&k, "a");
        assert_eq!((before.summary.anchors[0].state.as_str(), before.summary.reviewed), ("relocated", false));
        let after =
            k.hub.dna_note_action(&Caller::Person, action(&k, "a", NoteAction::Confirm, &before.content_hash)).unwrap();
        let text2 = std::fs::read_to_string(&path).unwrap();
        let n = mirante_core::dna::note::parse(&text2).unwrap();
        assert_eq!((n.anchors[0].start, n.anchors[0].end, n.anchors[0].commit.clone()), (3, 5, head_of(&k.repo)));
        assert_eq!(n.anchors[0].fingerprint, mirante_core::dna::note::parse(&text).unwrap().anchors[0].fingerprint);
        assert!(text2.contains("tags: [x]\n") && text2.contains("id: 0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55\n"));
        assert_eq!((after.summary.state.as_str(), after.summary.reviewed), ("fresh", true));
        assert!(k.hub.lock().store.dna_is_reviewed(&key(&k), "a", &sha256_hex(text2.as_bytes())).unwrap());
        assert!(!k.payloads("dna.changed").is_empty());
    }

    #[test]
    fn confirm_refuses_a_stale_anchor_and_a_changed_file() {
        let k = Kit::new();
        with_a_and_b(&k);
        commit_in(&k.repo, "src/b.rs", &CODE.replace("write()", "flush()"));
        let b = view(&k, "b");
        let stale = k.hub.dna_note_action(&Caller::Person, action(&k, "b", NoteAction::Confirm, &b.content_hash));
        assert!(
            matches!(&stale, Err(HubError::Conflict(m)) if m == "anchor 1 is stale: re-cite it or retire the note"),
            "{stale:?}"
        );
        let a = view(&k, "a");
        let changed = k.hub.dna_note_action(&Caller::Person, action(&k, "a", NoteAction::Confirm, "0000"));
        assert!(
            matches!(&changed, Err(HubError::Conflict(m)) if *m == mirante_core::dna::DnaError::Changed.to_string()),
            "{changed:?}"
        );
        assert_eq!(
            std::fs::read_to_string(k.repo.join(".mirante/memory/a.md")).map(|t| sha256_hex(t.as_bytes())).unwrap(),
            a.content_hash
        );
    }

    #[test]
    fn an_action_on_a_missing_note_is_not_found() {
        let k = Kit::new();
        with_a_and_b(&k);
        let r = k.hub.dna_note_action(&Caller::Person, action(&k, "nope", NoteAction::Retire, "h"));
        assert!(matches!(r, Err(HubError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn retire_keeps_the_file_with_status_retired() {
        let k = Kit::new();
        with_a_and_b(&k);
        let b = view(&k, "b");
        let v = k.hub.dna_note_action(&Caller::Person, action(&k, "b", NoteAction::Retire, &b.content_hash)).unwrap();
        assert_eq!((v.summary.status.as_str(), v.summary.reviewed), ("retired", true));
        let text = std::fs::read_to_string(k.repo.join(".mirante/memory/b.md")).unwrap();
        assert!(text.contains("status: retired\n") && text.contains("About b."));
        let hits = k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: "beta".into(), limit: None }).unwrap();
        assert!(hits.is_empty(), "retired notes are not handed to agents");
    }

    #[test]
    fn enable_creates_readme_and_is_explicit() {
        let k = Kit::new();
        let st = k.hub.dna_status(&k.repo_ref()).unwrap();
        assert!(!st.enabled && !st.dir && !k.repo.join(".mirante").exists());
        let st = k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        assert!(st.enabled && st.dir && k.repo.join(".mirante/memory/README.md").exists());
        let st = k.hub.dna_disable(&k.repo_ref(), &Caller::Person).unwrap();
        assert!(!st.enabled && k.repo.join(".mirante/memory/README.md").exists());
    }

    #[test]
    fn search_get_and_graph_reflect_files_on_disk() {
        let k = Kit::new();
        with_two_notes(&k);
        git(&k.repo, &["add", "-A"]);
        git(&k.repo, &["commit", "-q", "-m", "memory"]);
        k.hub.dna_check(&k.repo_ref()).unwrap();
        let hits = k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: "spool".into(), limit: None }).unwrap();
        assert_eq!(hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(), ["hook-spool"]);
        assert_eq!((hits[0].state.as_str(), hits[0].reviewed), ("fresh", false));
        assert_eq!(hits[0].anchors[0].symbol.as_deref(), Some("spool"));
        let none = k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: " ** ".into(), limit: None }).unwrap();
        assert!(none.is_empty());
        let n = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "hook-spool".into() }).unwrap();
        assert!(n.body.contains("Falls back"));
        assert_eq!(n.links, ["other", "gone"]);
        assert!(n.path.ends_with(".mirante/memory/hook-spool.md"));
        let other = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "other".into() }).unwrap();
        assert_eq!(other.backlinks, ["hook-spool"]);
        let missing = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "nope".into() });
        assert!(matches!(missing, Err(HubError::NotFound(_))), "{missing:?}");
        let g = k.hub.dna_graph(&k.repo_ref()).unwrap();
        assert_eq!(g.nodes.len(), 2);
        let links: Vec<(&str, &str, bool)> =
            g.links.iter().map(|l| (l.src.as_str(), l.dst.as_str(), l.dangling)).collect();
        assert_eq!(links, [("hook-spool", "gone", true), ("hook-spool", "other", false)]);
    }

    #[test]
    fn get_by_task_resolves_its_repo() {
        let k = Kit::new();
        with_two_notes(&k);
        let t = k.create(AgentKind::Claude, "mem").task;
        let by_task = DnaRef { repo: None, task: Some(t.id.clone()) };
        let n = k.hub.dna_get(DnaGet { at: by_task.clone(), slug: "other".into() }).unwrap();
        assert_eq!(n.summary.title, "Other thing");
        assert!(k.hub.dna_status(&by_task).unwrap().enabled);
    }

    #[test]
    fn check_emits_dna_indexed_with_counts() {
        let k = Kit::new();
        with_two_notes(&k);
        write_note(&k.repo, "broken", "no frontmatter");
        let rep = k.hub.dna_check(&k.repo_ref()).unwrap();
        assert_eq!((rep.notes, rep.invalid.len(), rep.unreviewed), (2, 1, 2));
        let last = k.payloads("dna.indexed").pop().expect("dna.indexed");
        assert_eq!(
            (last["notes"].as_u64(), last["unreviewed"].as_u64(), last["stale"].as_u64()),
            (Some(2), Some(2), Some(0))
        );
        assert_eq!(last["repo"].as_str(), Some(k.repo.canonicalize().unwrap().to_str().unwrap()));
        let st = k.hub.dna_status(&k.repo_ref()).unwrap();
        assert_eq!((st.notes, st.invalid, st.unreviewed, st.pending), (2, 1, 2, 0));
    }

    #[test]
    fn dna_indexed_is_emitted_only_when_something_changed() {
        let k = Kit::new();
        with_two_notes(&k);
        k.hub.dna_check(&k.repo_ref()).unwrap();
        let n = k.payloads("dna.indexed").len();
        k.hub.dna_check(&k.repo_ref()).unwrap();
        k.hub.dna_status(&k.repo_ref()).unwrap();
        k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: "spool".into(), limit: None }).unwrap();
        assert_eq!(k.payloads("dna.indexed").len(), n);
        write_note(&k.repo, "late", &note_text(&k.repo, "Late", "x", "src/a.rs", 1, 1));
        k.hub.dna_status(&k.repo_ref()).unwrap();
        assert_eq!(k.payloads("dna.indexed").len(), n + 1);
    }

    #[test]
    fn search_refreshes_lazily_when_head_moves() {
        let k = Kit::new();
        with_two_notes(&k);
        let q = || DnaSearch { at: k.repo_ref(), query: "spool".into(), limit: None };
        assert_eq!(k.hub.dna_search(q()).unwrap()[0].state, "fresh");
        commit_in(&k.repo, "src/a.rs", &CODE.replace("write()", "flush()"));
        let hit = &k.hub.dna_search(q()).unwrap()[0];
        assert_eq!(hit.state, "stale");
        assert_eq!(hit.anchors[0].state, "stale");
        // A note written later shows up without an explicit check.
        write_note(&k.repo, "late", &note_text(&k.repo, "Late spool note", "x", "src/a.rs", 1, 1));
        assert_eq!(k.hub.dna_search(q()).unwrap().len(), 2);
    }

    #[test]
    fn worktrees_and_non_repos_are_refused() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "wt").task;
        let at = |p: &str| DnaRef { repo: Some(p.into()), task: None };
        assert!(matches!(k.hub.dna_status(&at(&t.worktree_path)), Err(HubError::Invalid(_))));
        let plain = k.dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(matches!(k.hub.dna_status(&at(plain.to_str().unwrap())), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.dna_status(&at("/definitely/not/here")), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.dna_status(&DnaRef::default()), Err(HubError::Invalid(_))));
        // A subdirectory names its repo.
        std::fs::create_dir_all(k.repo.join("sub")).unwrap();
        let st = k.hub.dna_status(&at(k.repo.join("sub").to_str().unwrap())).unwrap();
        assert_eq!(st.repo, k.repo.canonicalize().unwrap().to_string_lossy());
    }
}
