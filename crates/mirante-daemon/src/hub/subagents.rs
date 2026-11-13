//! Claude's own agents (subagents), as read-only lanes of a task's live Claude session
//! (docs/spikes/FINDINGS-threads.md S1, Claude 2.1.285).
//!
//! - A lane opens on `SubagentStart` (or on the first tool event naming its `agent_id`, for
//!   a Claude without that hook) and closes on `SubagentStop`, whose `last_assistant_message`
//!   is its final text. The async `PostToolUse(Agent)` names its description at once.
//! - Its side transcript `<transcript dir>/<session>/subagents/agent-<id>.jsonl` (and the
//!   `.meta.json` beside it) is read only by descriptor walk under the main transcript's
//!   directory, with a checked id: never through a symlink (the `outputFile` Claude hands
//!   the main agent is one), never a path a hook names (`agent_transcript_path` is only
//!   compared with the expected one). Only assistant `text` blocks are read; `attachment`
//!   lines (environment, credentials) never are.
//! - Lanes live with the session (at most [`MAX_LANES`]) and go when it ends; their
//!   `subagent.*` events stay in the task's log.
//! - A `<task-notification>` prompt is Claude resuming on its own when an agent finished:
//!   it is recorded as the agent's (`subagent.notified`), never as the person's message.
//! - Read-only: nothing here steers, stops or answers an agent. A subagent is never a
//!   Mirante child task.
use std::path::{Path, PathBuf};

use mirante_core::hook::HookEvent;
use mirante_core::model::now_ms;
use mirante_core::redact::redact;
use mirante_core::safedir::SafeDir;
use mirante_ipc::types::{SubagentStatus, SubagentView, TaskSubagents};
use serde_json::{json, Value};

use super::{Hub, Result, State};

/// Most lanes one session keeps (the oldest finished one makes room).
pub(crate) const MAX_LANES: usize = 32;
/// Longest description kept (characters).
pub(crate) const DESCRIPTION_CAP: usize = 200;
/// Longest text kept (characters).
pub(crate) const TEXT_CAP: usize = 2000;
/// How much of a side transcript's end is read for its latest text.
pub(crate) const TAIL_BYTES: usize = 256 * 1024;
/// Largest `.meta.json` read.
const META_MAX: usize = 16 * 1024;
/// Most permission ids remembered per lane.
const PERMISSIONS_CAP: usize = 64;

/// One of Claude's own agents in a live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) id: String,
    pub(crate) agent_type: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) started_ms: i64,
    pub(crate) ended_ms: Option<i64>,
    pub(crate) tools: u32,
    /// Its final text (from `SubagentStop`), redacted and capped.
    pub(crate) final_text: Option<String>,
    pub(crate) permissions: Vec<String>,
    /// `SubagentStop` named a side file other than the expected one: nothing is read.
    pub(crate) path_mismatch: bool,
    /// Opening order (lanes opened in the same millisecond keep it).
    pub(crate) order: u64,
}

static LANE_ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Lane {
    fn new(id: &str, agent_type: Option<String>) -> Lane {
        Lane {
            id: id.to_string(),
            agent_type,
            description: None,
            started_ms: now_ms(),
            ended_ms: None,
            tools: 0,
            final_text: None,
            permissions: Vec::new(),
            path_mismatch: false,
            order: LANE_ORDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// A plain name of at most `max` bytes: `[A-Za-z0-9_-]`.
fn plain_name(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Claude's `agent_id` shape (`a` + 16 hex seen; anything in `[A-Za-z0-9_-]{1,64}`).
pub(crate) fn valid_agent_id(id: &str) -> bool {
    plain_name(id, 64)
}

fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A text from an agent, as Mirante keeps it: redacted, trimmed, capped; `None` when empty.
fn clean(s: &str, n: usize) -> Option<String> {
    let s = cap(redact(s.trim()).trim(), n);
    (!s.is_empty()).then_some(s)
}

/// Where one agent's side files are: the directory of the main transcript, the session
/// directory (the transcript's file stem) and the checked agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SideFile {
    pub(crate) root: PathBuf,
    pub(crate) session: String,
    pub(crate) id: String,
}

impl SideFile {
    /// From the main transcript `<dir>/<session>.jsonl` and an agent id; `None` when the id
    /// or the session name is not a plain name.
    pub(crate) fn of(transcript: &Path, id: &str) -> Option<SideFile> {
        if !valid_agent_id(id) || transcript.extension().is_none_or(|e| e != "jsonl") {
            return None;
        }
        let session = transcript.file_stem()?.to_str()?;
        if !plain_name(session, 128) {
            return None;
        }
        Some(SideFile { root: transcript.parent()?.to_path_buf(), session: session.to_string(), id: id.to_string() })
    }

    pub(crate) fn jsonl(&self) -> String {
        format!("agent-{}.jsonl", self.id)
    }

    fn meta(&self) -> String {
        format!("agent-{}.meta.json", self.id)
    }

    /// The path Claude names in `SubagentStop.agent_transcript_path`.
    pub(crate) fn expected(&self) -> PathBuf {
        self.root.join(&self.session).join("subagents").join(self.jsonl())
    }

    fn dir(&self) -> Option<SafeDir> {
        SafeDir::open(&self.root, &[&self.session, "subagents"]).ok().flatten()
    }

    /// The description in `.meta.json` (redacted, capped).
    pub(crate) fn description(&self) -> Option<String> {
        let text = self.dir()?.read_file(&self.meta(), META_MAX).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        clean(v["description"].as_str()?, DESCRIPTION_CAP)
    }

    /// The newest assistant text of the side transcript (its last [`TAIL_BYTES`]).
    pub(crate) fn last_text(&self) -> Option<String> {
        let (bytes, cut) = self.dir()?.read_tail(&self.jsonl(), TAIL_BYTES).ok()?;
        last_sidechain_text(&bytes, cut, &self.id)
    }
}

/// The newest assistant `text` of agent `id` in a side transcript's tail (`cut`: the first
/// line may be partial). Only `assistant` lines of that sidechain; never an `attachment`.
pub(crate) fn last_sidechain_text(bytes: &[u8], cut: bool, id: &str) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    lines.iter().rev().find_map(|l| {
        if !l.contains("\"assistant\"") {
            return None;
        }
        let v: Value = serde_json::from_str(l).ok()?;
        if v["type"] != "assistant" || v["isSidechain"] != true || v["agentId"].as_str().is_some_and(|a| a != id) {
            return None;
        }
        let parts: Vec<&str> = v["message"]["content"]
            .as_array()?
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !t.trim().is_empty())
            .collect();
        clean(&parts.join("\n\n"), TEXT_CAP)
    })
}

/// `(agent id, status)` of a `<task-notification>` prompt.
pub(crate) fn notification_of(prompt: &str) -> Option<(String, Option<String>)> {
    if !mirante_core::status::is_task_notification(prompt) {
        return None;
    }
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let from = prompt.find(&open)? + open.len();
        let to = prompt[from..].find(&close)? + from;
        Some(prompt[from..to].trim().to_string())
    };
    let id = tag("task-id").filter(|i| valid_agent_id(i))?;
    let status = tag("status").map(|s| cap(&s, 32));
    Some((id, status))
}

impl State {
    /// The lane of `id` in `task_id`'s live session, opened (with a `subagent.started`
    /// event) if it was not there. `None` when there is no live session, the id is not one,
    /// or the session already holds [`MAX_LANES`] running lanes.
    fn lane(&mut self, task_id: &str, id: &str, agent_type: Option<&str>) -> Result<Option<&mut Lane>> {
        if !valid_agent_id(id) {
            return Ok(None);
        }
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(None);
        };
        let agent_type = agent_type.map(|a| cap(a, 64)).filter(|a| !a.is_empty());
        if !l.subagents.contains_key(id) {
            if l.subagents.len() >= MAX_LANES {
                let oldest = l.subagents.values().filter(|x| x.ended_ms.is_some()).min_by_key(|x| x.started_ms);
                let Some(old) = oldest.map(|x| x.id.clone()) else {
                    return Ok(None);
                };
                l.subagents.remove(&old);
            }
            l.subagents.insert(id.to_string(), Lane::new(id, agent_type.clone()));
            let payload = json!({ "agent_id": id, "agent_type": agent_type });
            self.event(Some(task_id), "subagent.started", payload)?;
        }
        let mut lane = self.live.get_mut(task_id).and_then(|l| l.subagents.get_mut(id));
        if let Some(l) = lane.as_mut().filter(|l| l.agent_type.is_none()) {
            l.agent_type = agent_type;
        }
        Ok(lane)
    }

    /// Follows one Claude hook for the subagent lanes. `raw` is the payload as delivered
    /// (the async `PostToolUse(Agent)`'s `tool_response` is read from it); `card` is the
    /// permission card this hook just opened, if any.
    pub(crate) fn observe_subagent(
        &mut self,
        task_id: &str,
        ev: &HookEvent,
        raw: &str,
        card: Option<&str>,
    ) -> Result<()> {
        let name = ev.hook_event_name.as_str();
        if name == "UserPromptSubmit" && ev.agent_id.is_none() {
            let Some((id, status)) = ev.prompt.as_deref().and_then(notification_of) else {
                return Ok(());
            };
            // Claude resumed on its own: the agent's doing, not the person's.
            return self.event(Some(task_id), "subagent.notified", json!({ "agent_id": id, "status": status }));
        }
        if name == "PostToolUse" && ev.agent_id.is_none() && ev.tool_name.as_deref() == Some("Agent") {
            let Ok(v) = serde_json::from_str::<Value>(raw) else {
                return Ok(());
            };
            let r = &v["tool_response"];
            let Some(id) = r["agentId"].as_str() else {
                return Ok(());
            };
            let input = ev.tool_input.as_ref();
            let agent_type = r["agentType"].as_str().or(input.and_then(|i| i["subagent_type"].as_str()));
            let description = r["description"]
                .as_str()
                .or(input.and_then(|i| i["description"].as_str()))
                .and_then(|d| clean(d, DESCRIPTION_CAP));
            if let Some(lane) = self.lane(task_id, id, agent_type)? {
                if lane.description.is_none() {
                    lane.description = description;
                }
            }
            return Ok(());
        }
        let Some(id) = ev.agent_id.as_deref() else {
            return Ok(());
        };
        let agent_type = ev.agent_type.as_deref();
        match name {
            "SubagentStart" | "PreToolUse" => {
                self.lane(task_id, id, agent_type)?;
            }
            "PostToolUse" | "PostToolUseFailure" => {
                if let Some(lane) = self.lane(task_id, id, agent_type)? {
                    lane.tools = lane.tools.saturating_add(1);
                }
            }
            "PermissionRequest" => {
                let Some(lane) = self.lane(task_id, id, agent_type)? else {
                    return Ok(());
                };
                let Some(card) = card else {
                    return Ok(());
                };
                if lane.permissions.len() < PERMISSIONS_CAP {
                    lane.permissions.push(card.to_string());
                }
                let agent_type = lane.agent_type.clone();
                let payload = json!({ "permission_id": card, "agent_id": id, "agent_type": agent_type });
                self.event(Some(task_id), "subagent.permission", payload)?;
            }
            "SubagentStop" => {
                let transcript = self.live.get(task_id).and_then(|l| l.transcript_path.clone());
                let expected = transcript.as_deref().and_then(|t| SideFile::of(Path::new(t), id)).map(|s| s.expected());
                let mismatch =
                    ev.agent_transcript_path.as_deref().is_some_and(|p| expected.as_deref() != Some(Path::new(p)));
                let text = ev.last_assistant_message.as_deref().and_then(|t| clean(t, TEXT_CAP));
                let Some(lane) = self.lane(task_id, id, agent_type)? else {
                    return Ok(());
                };
                if lane.ended_ms.is_none() {
                    lane.ended_ms = Some(now_ms());
                }
                lane.path_mismatch |= mismatch;
                if text.is_some() {
                    lane.final_text = text.clone();
                }
                let agent_type = lane.agent_type.clone();
                let payload =
                    json!({ "agent_id": id, "agent_type": agent_type, "text": text, "path_mismatch": mismatch });
                self.event(Some(task_id), "subagent.stopped", payload)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The type of the subagent that raised permission card `card` of `task_id`, if one did.
    pub(crate) fn subagent_of_card(&self, task_id: &str, card: &str) -> Option<String> {
        let l = self.live.get(task_id)?;
        let agent = l.dialogs.get(card)?.agent.as_deref()?;
        let lane = l.subagents.get(agent);
        Some(lane.and_then(|x| x.agent_type.clone()).unwrap_or_else(|| "agent".into()))
    }
}

impl Hub {
    /// `task.subagents`: Claude's own agents in the task's live session, oldest first.
    /// Side files are read without the hub lock. Empty when no Claude session is live.
    pub fn task_subagents(&self, p: &TaskSubagents) -> Result<Vec<SubagentView>> {
        let (lanes, transcript, pending) = {
            let st = self.lock();
            let t = st.resolve(&p.task)?;
            let Some(l) = st.live.get(&t.id) else {
                return Ok(Vec::new());
            };
            let pending: Vec<String> = st.store.pending_permissions(&t.id)?.into_iter().map(|x| x.id).collect();
            (l.subagents.values().cloned().collect::<Vec<_>>(), l.transcript_path.clone(), pending)
        };
        let mut lanes = lanes;
        lanes.sort_by_key(|l| (l.started_ms, l.order));
        let out: Vec<SubagentView> = lanes
            .into_iter()
            .map(|lane| {
                let side = transcript
                    .as_deref()
                    .filter(|_| !lane.path_mismatch)
                    .and_then(|t| SideFile::of(Path::new(t), &lane.id));
                let description = lane.description.clone().or_else(|| side.as_ref().and_then(SideFile::description));
                let done = lane.ended_ms.is_some();
                let last_text = match (&lane.final_text, done) {
                    (Some(t), true) => Some(t.clone()),
                    _ => side.as_ref().and_then(SideFile::last_text).or(lane.final_text.clone()),
                };
                SubagentView {
                    pending: lane.permissions.iter().filter(|p| pending.contains(p)).count(),
                    id: lane.id,
                    agent_type: lane.agent_type,
                    description,
                    status: if done { SubagentStatus::Done } else { SubagentStatus::Running },
                    started_ms: lane.started_ms,
                    ended_ms: lane.ended_ms,
                    tools: lane.tools,
                    last_text,
                    permissions: lane.permissions,
                }
            })
            .collect();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DriverEvent;
    use crate::hub::testkit::Kit;
    use mirante_core::model::{AgentKind, Status};

    /// The S1 run (Claude 2.1.285, 3 background agents, the third one asking to `touch`):
    /// hooks and main-transcript lines in arrival order, redacted (`{TRANSCRIPT}`,
    /// `{SIDE_DIR}` stand for the paths), plus each agent's side file with its attachments
    /// replaced by synthetic ones.
    const RUN1: &str = include_str!("testdata/subagents-run1.jsonl");
    const SIDE: [(&str, &str, &str); 3] = [
        (
            "a7386d060a40b63d6",
            include_str!("testdata/agent-a7386d060a40b63d6.jsonl"),
            include_str!("testdata/agent-a7386d060a40b63d6.meta.json"),
        ),
        (
            "acf4d4c4535b0eda5",
            include_str!("testdata/agent-acf4d4c4535b0eda5.jsonl"),
            include_str!("testdata/agent-acf4d4c4535b0eda5.meta.json"),
        ),
        (
            "a9ef71ae0212bb123",
            include_str!("testdata/agent-a9ef71ae0212bb123.jsonl"),
            include_str!("testdata/agent-a9ef71ae0212bb123.meta.json"),
        ),
    ];
    const SESSION: &str = "be649dfc-66c5-42f9-9c06-62ca60c7bf2a";

    /// A fake `~/.claude/projects/<slug>` with the run's side files; the main transcript path.
    fn projects(k: &Kit) -> PathBuf {
        let dir = k.dir.path().join("projects").join("-private-tmp-repo");
        let side = dir.join(SESSION).join("subagents");
        std::fs::create_dir_all(&side).unwrap();
        for (id, jsonl, meta) in SIDE {
            std::fs::write(side.join(format!("agent-{id}.jsonl")), jsonl).unwrap();
            std::fs::write(side.join(format!("agent-{id}.meta.json")), meta).unwrap();
        }
        let main = dir.join(format!("{SESSION}.jsonl"));
        std::fs::write(&main, "").unwrap();
        main
    }

    /// Replays the recorded records with `after < at <= until` (ms from the start).
    fn replay(k: &Kit, task: &str, main: &Path, after: i64, until: i64) {
        let side_dir = main.with_extension("").join("subagents");
        for l in RUN1.lines() {
            let r: Value = serde_json::from_str(l).unwrap();
            let at = r["at"].as_i64().unwrap();
            if at <= after || at > until {
                continue;
            }
            if let Some(h) = r.get("hook") {
                let s = h
                    .to_string()
                    .replace("{TRANSCRIPT}", &main.to_string_lossy())
                    .replace("{SIDE_DIR}", &side_dir.to_string_lossy());
                k.hook(task, serde_json::from_str(&s).unwrap());
            } else {
                k.emit(DriverEvent::Transcript(r["line"].clone()));
            }
        }
    }

    fn run(k: &Kit) -> (String, PathBuf) {
        let t = k.started(AgentKind::Claude, "a").task;
        let main = projects(k);
        (t.id, main)
    }

    fn lanes(k: &Kit, task: &str) -> Vec<SubagentView> {
        k.hub.task_subagents(&TaskSubagents { task: task.into() }).unwrap()
    }

    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
    }

    fn events(k: &Kit, task: &str, kind: &str) -> Vec<Value> {
        let st = k.hub.lock();
        st.store.task_events(task).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect()
    }

    #[test]
    fn tool_events_with_agent_id_open_a_lane_and_subagent_stop_closes_it() {
        let k = Kit::new();
        let (t, main) = run(&k);
        // Mid-run: all three started, b/ (acf4…) just stopped.
        replay(&k, &t, &main, -1, 7370);
        let l = lanes(&k, &t);
        let ids: Vec<&str> = l.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["a7386d060a40b63d6", "acf4d4c4535b0eda5", "a9ef71ae0212bb123"]);
        let by = |id: &str| l.iter().find(|x| x.id == id).unwrap().clone();
        assert_eq!(by("acf4d4c4535b0eda5").status, SubagentStatus::Done);
        assert_eq!(
            by("acf4d4c4535b0eda5").last_text.as_deref(),
            Some("The b/ directory contains one file:\n\n- **b1.txt**")
        );
        assert_eq!(by("a7386d060a40b63d6").status, SubagentStatus::Running);
        assert_eq!(by("a7386d060a40b63d6").tools, 1);
        assert_eq!(by("a7386d060a40b63d6").agent_type.as_deref(), Some("general-purpose"));
        // Running: its latest text comes from its side file (the fixture holds the whole run).
        assert!(by("a9ef71ae0212bb123").last_text.as_deref().unwrap().starts_with("Here are the results:"));
        replay(&k, &t, &main, 7370, i64::MAX);
        let l = lanes(&k, &t);
        assert!(l.iter().all(|x| x.status == SubagentStatus::Done && x.ended_ms.is_some()), "{l:?}");
        assert_eq!(events(&k, &t, "subagent.started").len(), 3);
        assert_eq!(events(&k, &t, "subagent.stopped").len(), 3);
        // Their tool steps say which agent ran them.
        let steps = events(&k, &t, "step");
        let bash: Vec<&Value> = steps.iter().filter(|s| s["tool"] == "Bash").collect();
        assert_eq!(bash.len(), 3);
        assert!(bash.iter().all(|s| s["agent_id"].as_str().is_some_and(valid_agent_id)), "{bash:?}");
    }

    /// Without `SubagentStart` (an older Claude), the first tool event opens the lane.
    #[test]
    fn a_tool_event_alone_opens_a_lane() {
        let k = Kit::new();
        let (t, _) = run(&k);
        k.hook(
            &t,
            json!({"hook_event_name": "PostToolUse", "agent_id": "a-1", "agent_type": "Explore", "tool_name": "Read"}),
        );
        let l = lanes(&k, &t);
        assert_eq!((l.len(), l[0].tools, l[0].agent_type.as_deref()), (1, 1, Some("Explore")));
        // The main agent's own tools open none.
        k.hook(&t, json!({"hook_event_name": "PostToolUse", "agent_type": "reviewer", "tool_name": "Read"}));
        assert_eq!(lanes(&k, &t).len(), 1);
    }

    #[test]
    fn the_task_tool_input_description_names_the_lane_redacted_and_capped() {
        let k = Kit::new();
        let (t, main) = run(&k);
        replay(&k, &t, &main, -1, 5104);
        let l = lanes(&k, &t);
        assert_eq!(l[0].description.as_deref(), Some("List files in a/ directory"));
        assert_eq!(l[2].description.as_deref(), Some("List c/ and create new file"));
        // From the async PostToolUse(Agent): redacted, capped; its outputFile is never used.
        let secret = "ghp_".to_string() + &"a".repeat(36);
        let long = format!("deploy with {secret} {}", "x".repeat(400));
        k.hook(&t, json!({"hook_event_name": "SubagentStart", "agent_id": "a-new", "agent_type": "Explore"}));
        k.hook(
            &t,
            json!({"hook_event_name": "PostToolUse", "tool_name": "Agent",
                "tool_input": {"description": "ignored", "subagent_type": "Explore"},
                "tool_response": {"isAsync": true, "status": "async_launched", "agentId": "a-new",
                    "description": long, "outputFile": "/somewhere/else.output"}}),
        );
        let l = lanes(&k, &t);
        let d = l.iter().find(|x| x.id == "a-new").unwrap().description.clone().unwrap();
        assert!(!d.contains(&secret), "{d}");
        assert_eq!(d.chars().count(), DESCRIPTION_CAP);
        // A lane without one reads it from .meta.json (only under the transcript's dir).
        let side = main.with_extension("").join("subagents");
        std::fs::write(side.join("agent-a-meta.meta.json"), r#"{"agentType":"Plan","description":"Read the plan"}"#)
            .unwrap();
        k.hook(&t, json!({"hook_event_name": "SubagentStart", "agent_id": "a-meta", "agent_type": "Plan"}));
        let l = lanes(&k, &t);
        assert_eq!(l.iter().find(|x| x.id == "a-meta").unwrap().description.as_deref(), Some("Read the plan"));
    }

    #[test]
    fn side_file_path_is_built_from_the_transcript_dir_and_a_checked_id() {
        let t = Path::new("/h/.claude/projects/-p/s-1.jsonl");
        let f = SideFile::of(t, "a7386d060a40b63d6").unwrap();
        assert_eq!(f.expected(), Path::new("/h/.claude/projects/-p/s-1/subagents/agent-a7386d060a40b63d6.jsonl"));
        assert!(SideFile::of(t, "../x").is_none());
        assert!(SideFile::of(t, "a/b").is_none());
        assert!(SideFile::of(t, "").is_none());
        assert!(SideFile::of(t, &"a".repeat(65)).is_none());
        assert!(SideFile::of(Path::new("/h/x.txt"), "a1").is_none());
        assert!(SideFile::of(Path::new("/h/..jsonl"), "a1").is_none());
        // A side file planted as a symlink is never read.
        let k = Kit::new();
        let (task, main) = run(&k);
        let side = main.with_extension("").join("subagents");
        let outside = k.dir.path().join("secret.jsonl");
        let secret =
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"SECRET"}]}}"#;
        std::fs::write(&outside, secret).unwrap();
        std::os::unix::fs::symlink(&outside, side.join("agent-a-link.jsonl")).unwrap();
        assert_eq!(SideFile::of(&main, "a-link").unwrap().last_text(), None);
        // `SubagentStop` naming another file: nothing is read for that lane.
        k.hook(
            &task,
            json!({"hook_event_name": "SubagentStart", "agent_id": "a9ef71ae0212bb123", "transcript_path": main}),
        );
        k.hook(
            &task,
            json!({"hook_event_name": "SubagentStop", "agent_id": "a9ef71ae0212bb123",
                "agent_transcript_path": outside, "transcript_path": main}),
        );
        let l = lanes(&k, &task);
        assert_eq!((l[0].last_text.clone(), l[0].description.clone()), (None, None));
        assert_eq!(events(&k, &task, "subagent.stopped")[0]["path_mismatch"], true);
    }

    #[test]
    fn last_text_is_the_newest_sidechain_assistant_text_tail_read_256k() {
        let (id, jsonl, _) = SIDE[2];
        assert!(jsonl.contains("SYNTHETIC-ATTACHMENT"), "the fixture has attachments");
        let t = last_sidechain_text(jsonl.as_bytes(), false, id).unwrap();
        assert!(t.starts_with("Here are the results:"), "{t}");
        // Attachments never surface, even as the newest line.
        let att = r#"{"type":"attachment","isSidechain":true,"attachment":{"type":"credential_org","text":"ORG"}}"#;
        let with = format!("{jsonl}{att}\n");
        assert_eq!(last_sidechain_text(with.as_bytes(), false, id), Some(t.clone()));
        // Another agent's line, a main-chain line and a partial first line are skipped.
        let other = r#"{"type":"assistant","isSidechain":true,"agentId":"zz","message":{"content":[{"type":"text","text":"other"}]}}"#;
        let main = r#"{"type":"assistant","isSidechain":false,"message":{"content":[{"type":"text","text":"main"}]}}"#;
        let text = format!("{jsonl}{other}\n{main}\n");
        assert_eq!(last_sidechain_text(text.as_bytes(), false, id), Some(t));
        let partial = br#"tant","isSidechain":true,"message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert_eq!(last_sidechain_text(partial, true, id), None);
        // A big file: only its tail is read, and the latest text is still found.
        let k = Kit::new();
        let (_, main_t) = run(&k);
        let side = main_t.with_extension("").join("subagents");
        let pad = "p".repeat(1000);
        let filler = format!("{{\"type\":\"attachment\",\"isSidechain\":true,\"attachment\":{{\"pad\":\"{pad}\"}}}}\n");
        let mut big = String::from(
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"too old"}]}}"#,
        );
        big.push('\n');
        for _ in 0..400 {
            big.push_str(&filler);
        }
        big.push_str(r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"newest"},{"type":"tool_use","name":"Bash"}]}}"#);
        big.push('\n');
        assert!(big.len() > TAIL_BYTES);
        std::fs::write(side.join("agent-a-big.jsonl"), &big).unwrap();
        assert_eq!(SideFile::of(&main_t, "a-big").unwrap().last_text().as_deref(), Some("newest"));
        // Redacted and capped.
        let secret = "ghp_".to_string() + &"b".repeat(36);
        let y = "y".repeat(5000);
        let line = format!(
            "{{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{secret} {y}\"}}]}}}}\n"
        );
        let got = last_sidechain_text(line.as_bytes(), false, "a").unwrap();
        assert!(!got.contains(&secret));
        assert_eq!(got.chars().count(), TEXT_CAP);
    }

    #[test]
    fn lanes_are_dropped_when_the_session_ends_but_events_stay() {
        let k = Kit::new();
        let (t, main) = run(&k);
        replay(&k, &t, &main, -1, 12530);
        assert_eq!(lanes(&k, &t).len(), 3);
        k.emit(DriverEvent::Exited { code: Some(0) });
        assert!(lanes(&k, &t).is_empty());
        assert_eq!(events(&k, &t, "subagent.started").len(), 3);
        assert_eq!(events(&k, &t, "subagent.stopped").len(), 3);
    }

    #[test]
    fn subagents_never_count_as_children() {
        let k = Kit::new();
        let (t, main) = run(&k);
        replay(&k, &t, &main, -1, i64::MAX);
        let st = k.hub.lock();
        let tasks = st.store.list_tasks(true).unwrap();
        assert_eq!(tasks.len(), 1, "a subagent is no task");
        assert!(tasks[0].group_id.is_none());
        assert_eq!(tasks[0].id, t);
    }

    /// S1: the three `<task-notification>` prompts are Claude resuming on its own: recorded
    /// as the agents', never as a message of the person (nothing the miner reads).
    #[test]
    fn task_notifications_are_the_agents_not_the_person_s() {
        let k = Kit::new();
        let (t, main) = run(&k);
        replay(&k, &t, &main, -1, i64::MAX);
        let n = events(&k, &t, "subagent.notified");
        let ids: Vec<&str> = n.iter().map(|e| e["agent_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["acf4d4c4535b0eda5", "a7386d060a40b63d6", "a9ef71ae0212bb123"]);
        assert_eq!(n[0]["status"], "completed");
        let kinds = k.kinds(&t);
        for kind in ["message_sent", "message_queued", "message_delivery"] {
            assert!(!kinds.iter().any(|x| x == kind), "{kind} in {kinds:?}");
        }
        let st = k.hub.lock();
        let all = st.store.task_events(&t).unwrap();
        assert!(all.iter().all(|e| e.payload["by"] != "person"));
        assert!(!all.iter().any(|e| e.payload.to_string().contains("<task-notification>")));
    }

    /// A subagent's permission request makes the task `NeedsYou` (its dialog is real), the
    /// card names the subagent, and a background agent's dialog outlives the main turns.
    #[test]
    fn a_subagent_s_permission_is_attributed_and_outlives_the_main_turns() {
        let k = Kit::new();
        let (t, main) = run(&k);
        replay(&k, &t, &main, -1, 7467);
        assert_eq!(status(&k, &t), Status::NeedsYou);
        let perms = k.hub.permissions().unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].subagent.as_deref(), Some("general-purpose"));
        let p = events(&k, &t, "subagent.permission");
        assert_eq!(p[0]["agent_id"], "a9ef71ae0212bb123");
        assert_eq!(p[0]["permission_id"], perms[0].request.id.as_str());
        let lane = lanes(&k, &t).into_iter().find(|x| x.id == "a9ef71ae0212bb123").unwrap();
        assert_eq!((lane.permissions.len(), lane.pending), (1, 1));
        // Up to the main agent's next Stop (a7386…'s notification turn): still asking.
        replay(&k, &t, &main, 7467, 9163);
        assert_eq!(status(&k, &t), Status::NeedsYou);
        assert_eq!(k.hub.permissions().unwrap().len(), 1, "the main Stop settled no card");
        // Its SubagentStop (the spike's hook denied it): the dialog went with the agent.
        replay(&k, &t, &main, 9163, 9771);
        assert_eq!(k.hub.permissions().unwrap().len(), 0);
        assert_eq!(status(&k, &t), Status::Working, "a9ef…'s notification turn runs");
        replay(&k, &t, &main, 9771, 12530);
        assert_eq!(status(&k, &t), Status::Idle);
    }

    #[test]
    fn notification_prompts_parse_their_agent() {
        let p = "<task-notification>\n<task-id>acf4d4c4535b0eda5</task-id>\n<status>completed</status>\n</task-notification>";
        assert_eq!(notification_of(p), Some(("acf4d4c4535b0eda5".into(), Some("completed".into()))));
        assert_eq!(notification_of("<task-notification><task-id>../x</task-id>"), None);
        assert_eq!(notification_of("<task-notification>"), None);
        assert_eq!(notification_of("my words <task-id>a1</task-id>"), None);
    }

    #[test]
    fn at_most_32_lanes_the_oldest_finished_makes_room() {
        let k = Kit::new();
        let (t, _) = run(&k);
        for i in 0..MAX_LANES {
            k.hook(&t, json!({"hook_event_name": "SubagentStart", "agent_id": format!("a{i:02}")}));
        }
        k.hook(&t, json!({"hook_event_name": "SubagentStart", "agent_id": "over"}));
        assert_eq!(lanes(&k, &t).len(), MAX_LANES);
        assert!(!lanes(&k, &t).iter().any(|x| x.id == "over"));
        k.hook(&t, json!({"hook_event_name": "SubagentStop", "agent_id": "a05"}));
        k.hook(&t, json!({"hook_event_name": "SubagentStart", "agent_id": "over"}));
        let l = lanes(&k, &t);
        assert_eq!(l.len(), MAX_LANES);
        assert!(l.iter().any(|x| x.id == "over") && !l.iter().any(|x| x.id == "a05"));
    }
}
