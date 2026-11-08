//! The fake daemon's repo memory: a 6-note demo graph (1 stale, 1 unreviewed, 1 dangling
//! link), 2 pending proposals and a task context for demo task `t-claude-1`. `dna.review`
//! and `dna.note_action` change this state the way the daemon would, with its refusals
//! (already decided, changed since read, stale on confirm) as `CONFLICT`.
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::method;
use serde_json::{json, Value};

/// The repo whose memory is enabled at start (demo task `t-claude-1` lives there).
pub const DEMO_REPO: &str = "/repos/mirante";
const HEAD: &str = "50ea8cc255941e739b13cdbe2c937abca0319834";

struct DemoNote {
    slug: String,
    title: String,
    kind: &'static str,
    status: &'static str,
    state: &'static str,
    reviewed: bool,
    body: String,
    anchor: Value,
    version: u32,
}

pub struct DnaDemo {
    enabled: Vec<String>,
    notes: Vec<DemoNote>,
    proposals: Vec<Value>,
    next_slug: u32,
}

fn anchor(path: &str, symbol: &str, start: u32, end: u32, state: &str, reason: Option<&str>) -> Value {
    json!({"path": path, "symbol": symbol, "start": start, "end": end, "commit": HEAD,
           "state": state, "reason": reason, "now_start": null, "now_end": null})
}

fn note(
    slug: &str,
    title: &str,
    kind: &'static str,
    state: &'static str,
    reviewed: bool,
    body: &str,
    a: Value,
) -> DemoNote {
    DemoNote {
        slug: slug.into(),
        title: title.into(),
        kind,
        status: "active",
        state,
        reviewed,
        body: body.into(),
        anchor: a,
        version: 1,
    }
}

fn links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("[[") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("]]") else { break };
        let slug = rest[..j].split(['|', '#']).next().unwrap_or("").trim().to_string();
        if !slug.is_empty() && !out.contains(&slug) {
            out.push(slug);
        }
        rest = &rest[j + 2..];
    }
    out
}

fn conflict(m: &str) -> RpcError {
    RpcError::new(code::CONFLICT, m)
}

impl Default for DnaDemo {
    fn default() -> Self {
        let stale_reason = "the code cited at crates/mirante-daemon/src/hub/session.rs:88-120 changed (start_session is now at line 97)";
        let notes = vec![
            note(
                "hook-spool-fallback",
                "Hook events fall back to the spool when the daemon socket is down",
                "gotcha",
                "fresh",
                true,
                "`mirante-hook` never blocks Claude: when the socket refuses, the event is written to the\nspool and the daemon replays it ([[hub-dedupes-hook-events]]).",
                anchor("crates/mirante-hook/src/lib.rs", "deliver_session", 120, 148, "fresh", None),
            ),
            note(
                "hub-dedupes-hook-events",
                "The hub dedupes replayed hook events",
                "fact",
                "fresh",
                true,
                "A replayed spool event carries its original id, so the hub ignores a second copy.\nSee [[session-boot-order]] for when sessions accept events.",
                anchor("crates/mirante-daemon/src/hub/mod.rs", "Hub::hook_event", 40, 72, "fresh", None),
            ),
            note(
                "session-boot-order",
                "Sessions boot through trust before ready",
                "decision",
                "stale",
                true,
                "A session is `booting`, then `trust_prompt` when the folder is new, then `ready`.\nMessages queue until `ready`.",
                anchor("crates/mirante-daemon/src/hub/session.rs", "start_session", 88, 120, "stale", Some(stale_reason)),
            ),
            note(
                "worktree-per-task",
                "Every task gets its own git worktree",
                "decision",
                "fresh",
                false,
                "Agents never share a checkout: each task works in its own worktree and branch\n([[hook-spool-fallback]] relies on the worktree path). Release steps: [[release-checklist]].",
                anchor("crates/mirante-core/src/git.rs", "Git::worktree_add", 200, 231, "fresh", None),
            ),
            note(
                "memory-guard-levels",
                "The memory guard has three levels",
                "fact",
                "fresh",
                true,
                "normal, elevated and critical; hibernation starts at critical. Each task runs in a\n[[worktree-per-task]].",
                anchor("crates/mirante-daemon/src/memguard.rs", "Level", 10, 30, "fresh", None),
            ),
            note(
                "codex-app-server",
                "Codex runs as app-server over stdio",
                "pattern",
                "fresh",
                true,
                "The Codex driver speaks JSON-RPC to `codex app-server`; hook events do not apply\n(compare [[hook-spool-fallback]]).",
                anchor("crates/mirante-daemon/src/driver/codex.rs", "CodexDriver::spawn", 940, 990, "fresh", None),
            ),
        ];
        let proposals = vec![
            json!({
                "id": "p-1", "repo": DEMO_REPO, "task_id": "t-claude-1", "origin": "agent",
                "title": "Login tokens are refreshed by the session layer",
                "kind": "fact",
                "body": "The login flow never stores TOKEN=«redacted»; the session layer refreshes it.\nRelated: [[hub-dedupes-hook-events]].",
                "anchors": [{"path": "src/login.rs", "symbol": "refresh", "start": 12, "end": 30, "commit": HEAD, "fingerprint": "9f2c4b1a7d3e5f60"}],
                "redactions": 1, "status": "pending", "slug": null, "decision_note": "",
                "created_at": 1_700_000_000_000u64, "decided_at": null
            }),
            json!({
                "id": "p-2", "repo": DEMO_REPO, "task_id": null, "origin": "cli",
                "title": "Release builds are signed in CI only",
                "kind": "decision",
                "body": "Local builds are unsigned; the release workflow signs and notarizes.",
                "anchors": [{"path": ".github/workflows/release.yml", "start": 1, "end": 20, "commit": HEAD, "fingerprint": "0a1b2c3d4e5f6071"}],
                "redactions": 0, "status": "pending", "slug": null, "decision_note": "",
                "created_at": 1_700_000_100_000u64, "decided_at": null
            }),
        ];
        DnaDemo { enabled: vec![DEMO_REPO.into()], notes, proposals, next_slug: 1 }
    }
}

impl DnaDemo {
    fn summary(n: &DemoNote) -> Value {
        json!({"slug": n.slug, "title": n.title, "kind": n.kind, "status": n.status,
               "state": n.state, "reviewed": n.reviewed, "anchors": [n.anchor]})
    }

    fn hash(n: &DemoNote) -> String {
        format!("hash-{}-{}", n.slug, n.version)
    }

    fn view(&self, n: &DemoNote) -> Value {
        let mut v = Self::summary(n);
        let backlinks: Vec<&str> =
            self.notes.iter().filter(|o| links(&o.body).contains(&n.slug)).map(|o| o.slug.as_str()).collect();
        v["body"] = json!(n.body);
        v["links"] = json!(links(&n.body));
        v["backlinks"] = json!(backlinks);
        v["content_hash"] = json!(Self::hash(n));
        v["source"] = json!("manual");
        v["path"] = json!(format!("{DEMO_REPO}/.mirante/memory/{}.md", n.slug));
        v
    }

    fn repo_of(p: &Value, tasks: &[Value]) -> Option<String> {
        if let Some(r) = p["repo"].as_str() {
            return Some(r.to_string());
        }
        let t = p["task"].as_str()?;
        tasks.iter().find(|x| x["id"] == t).and_then(|x| x["repo_path"].as_str()).map(String::from)
    }

    fn on(&self, repo: &Option<String>) -> bool {
        repo.as_ref().is_some_and(|r| self.enabled.contains(r))
    }

    fn pending(&self) -> usize {
        self.proposals.iter().filter(|p| p["status"] == "pending").count()
    }

    fn status(&self, repo: &str) -> Value {
        let on = self.enabled.iter().any(|r| r == repo);
        let (notes, stale, unreviewed) = if on {
            let active = self.notes.iter().filter(|n| n.status == "active");
            (
                self.notes.len(),
                active.clone().filter(|n| n.state == "stale").count(),
                active.filter(|n| !n.reviewed).count(),
            )
        } else {
            (0, 0, 0)
        };
        json!({"repo": repo, "enabled": on, "dir": on, "notes": notes, "stale": stale,
               "unreviewed": unreviewed, "pending": if on { self.pending() } else { 0 },
               "invalid": 0, "head": HEAD})
    }

    fn find(&mut self, slug: &str) -> Result<&mut DemoNote, RpcError> {
        self.notes
            .iter_mut()
            .find(|n| n.slug == slug)
            .ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no note {slug}")))
    }

    /// The answer to a `dna.*` call, or `None` when `m` is not one.
    pub fn answer(&mut self, m: &str, p: &Value, tasks: &[Value]) -> Option<Result<Value, RpcError>> {
        let repo = Self::repo_of(p, tasks);
        let invalid = || RpcError::new(code::INVALID_PARAMS, "repo or task required");
        Some(match m {
            method::DNA_STATUS => repo.map(|r| self.status(&r)).ok_or_else(invalid),
            method::DNA_ENABLE => repo.ok_or_else(invalid).map(|r| {
                if !self.enabled.contains(&r) {
                    self.enabled.push(r.clone());
                }
                self.status(&r)
            }),
            method::DNA_DISABLE => repo.ok_or_else(invalid).map(|r| {
                self.enabled.retain(|x| *x != r);
                self.status(&r)
            }),
            method::DNA_GRAPH => Ok(if self.on(&repo) {
                let slugs: Vec<&str> = self.notes.iter().map(|n| n.slug.as_str()).collect();
                let mut ls = Vec::new();
                for n in &self.notes {
                    for d in links(&n.body) {
                        ls.push(json!({"src": n.slug, "dangling": !slugs.contains(&d.as_str()), "dst": d}));
                    }
                }
                json!({"nodes": self.notes.iter().map(Self::summary).collect::<Vec<_>>(), "links": ls})
            } else {
                json!({"nodes": [], "links": []})
            }),
            method::DNA_SEARCH => {
                let q = p["query"].as_str().unwrap_or("").to_lowercase();
                let hits: Vec<Value> = if self.on(&repo) && !q.trim().is_empty() {
                    self.notes
                        .iter()
                        .filter(|n| {
                            q.split_whitespace().all(|w| {
                                n.title.to_lowercase().contains(w)
                                    || n.body.to_lowercase().contains(w)
                                    || n.slug.contains(w)
                            })
                        })
                        .map(Self::summary)
                        .collect()
                } else {
                    vec![]
                };
                Ok(json!(hits))
            }
            method::DNA_GET => {
                let slug = p["slug"].as_str().unwrap_or("");
                match self.notes.iter().find(|n| n.slug == slug) {
                    Some(n) => Ok(self.view(n)),
                    None => Err(RpcError::new(code::NOT_FOUND, format!("no note {slug}"))),
                }
            }
            method::DNA_CHECK => repo.ok_or_else(invalid).map(|r| {
                let st = self.status(&r);
                json!({"repo": r, "head": HEAD, "notes": st["notes"], "invalid": [], "stale": st["stale"],
                       "relocated": 0, "unverifiable": 0, "unreviewed": st["unreviewed"]})
            }),
            method::DNA_TASK_CONTEXT => {
                let task = p["task"].as_str().unwrap_or("");
                let mine = |x: &&Value| x["task_id"] == task;
                Ok(if !self.on(&repo) {
                    json!({"enabled": false, "relevant": [], "at_risk": [], "proposals": []})
                } else if task == "t-claude-1" {
                    let pick = |s: &str| self.notes.iter().find(|n| n.slug == s).map(Self::summary);
                    let at_risk: Vec<Value> = ["hook-spool-fallback"]
                        .iter()
                        .filter_map(|s| pick(s))
                        .map(|mut v| {
                            v["reason"] = json!("crates/mirante-hook/src/lib.rs: the code cited at crates/mirante-hook/src/lib.rs:120-148 changed (deliver_session is now at line 131)");
                            v
                        })
                        .collect();
                    let relevant: Vec<Value> =
                        ["worktree-per-task", "hub-dedupes-hook-events"].iter().filter_map(|s| pick(s)).collect();
                    let props: Vec<&Value> = self.proposals.iter().filter(mine).collect();
                    json!({"enabled": true, "relevant": relevant, "at_risk": at_risk, "proposals": props})
                } else {
                    let props: Vec<&Value> = self.proposals.iter().filter(mine).collect();
                    json!({"enabled": true, "relevant": [], "at_risk": [], "proposals": props})
                })
            }
            method::DNA_PROPOSALS => {
                let status = p["status"].as_str().unwrap_or("pending");
                let limit = p["limit"].as_u64().unwrap_or(50).min(200) as usize;
                let after = p["after"].as_str();
                let mut skipping = after.is_some();
                let out: Vec<Value> = self
                    .proposals
                    .iter()
                    .filter(|x| status == "all" || x["status"] == status)
                    .filter(|x| p["task"].as_str().is_none_or(|t| x["task_id"] == t))
                    .filter(|x| p["id"].as_str().is_none_or(|i| x["id"] == i))
                    .filter(|x| {
                        if skipping {
                            skipping = after != x["id"].as_str();
                            return false;
                        }
                        true
                    })
                    .take(limit)
                    .cloned()
                    .collect();
                Ok(json!(out))
            }
            method::DNA_REVIEW => self.review(p),
            method::DNA_NOTE_ACTION => self.note_action(p),
            _ => return None,
        })
    }

    fn review(&mut self, p: &Value) -> Result<Value, RpcError> {
        let id = p["id"].as_str().unwrap_or("");
        let accept = p["accept"].as_bool().unwrap_or(false);
        let i = self
            .proposals
            .iter()
            .position(|x| x["id"] == id)
            .ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no proposal {id}")))?;
        if self.proposals[i]["status"] != "pending" {
            return Err(conflict("this proposal was already decided"));
        }
        let mut slug = Value::Null;
        if accept {
            let prop = &self.proposals[i];
            let title = p["title"].as_str().or(prop["title"].as_str()).unwrap_or("").to_string();
            let body = p["body"].as_str().or(prop["body"].as_str()).unwrap_or("").to_string();
            let s = format!("accepted-note-{}", self.next_slug);
            self.next_slug += 1;
            let a = &prop["anchors"][0];
            let av = anchor(
                a["path"].as_str().unwrap_or(""),
                a["symbol"].as_str().unwrap_or(""),
                a["start"].as_u64().unwrap_or(1) as u32,
                a["end"].as_u64().unwrap_or(1) as u32,
                "fresh",
                None,
            );
            let mut n = note(&s, &title, "fact", "fresh", true, &body, av);
            n.kind = match prop["kind"].as_str() {
                Some("decision") => "decision",
                Some("pattern") => "pattern",
                Some("gotcha") => "gotcha",
                _ => "fact",
            };
            self.notes.push(n);
            slug = json!(s);
        }
        let x = &mut self.proposals[i];
        x["status"] = json!(if accept { "accepted" } else { "rejected" });
        x["slug"] = slug;
        x["decision_note"] = json!(p["note"].as_str().unwrap_or(""));
        x["decided_at"] = json!(1_700_000_200_000u64);
        // `dna.review` answers the proposal plus how many secrets the edits had (none here).
        let mut out = x.clone();
        out["redacted"] = json!(0);
        Ok(out)
    }

    fn note_action(&mut self, p: &Value) -> Result<Value, RpcError> {
        let slug = p["slug"].as_str().unwrap_or("").to_string();
        let action = p["action"].as_str().unwrap_or("");
        let expect = p["expect_hash"].as_str().unwrap_or("").to_string();
        let n = self.find(&slug)?;
        if DnaDemo::hash(n) != expect {
            return Err(conflict("the note changed on disk since it was read; reload and try again"));
        }
        match action {
            "confirm" if n.state == "stale" => {
                return Err(conflict("anchor 1 is stale: re-cite it or retire the note"))
            }
            "confirm" => n.reviewed = true,
            "retire" => {
                n.status = "retired";
                n.reviewed = true;
            }
            _ => return Err(RpcError::new(code::INVALID_PARAMS, "action must be confirm or retire")),
        }
        n.version += 1;
        let n = self.notes.iter().find(|n| n.slug == slug).expect("just found");
        Ok(self.view(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_graph_has_one_stale_one_unreviewed_and_one_dangling_link() {
        let mut d = DnaDemo::default();
        let g = d.answer(method::DNA_GRAPH, &json!({"repo": DEMO_REPO}), &[]).unwrap().unwrap();
        let nodes = g["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 6);
        assert_eq!(nodes.iter().filter(|n| n["state"] == "stale").count(), 1);
        assert_eq!(nodes.iter().filter(|n| n["reviewed"] == false).count(), 1);
        let dangling: Vec<&Value> = g["links"].as_array().unwrap().iter().filter(|l| l["dangling"] == true).collect();
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0]["dst"], "release-checklist");
        assert!(d.answer("task.list", &json!({}), &[]).is_none());
    }

    #[test]
    fn review_flips_the_proposal_once_and_adds_the_note() {
        let mut d = DnaDemo::default();
        let pending = |d: &mut DnaDemo| d.answer(method::DNA_PROPOSALS, &json!({}), &[]).unwrap().unwrap();
        assert_eq!(pending(&mut d).as_array().unwrap().len(), 2);
        let r = d
            .answer(method::DNA_REVIEW, &json!({"id": "p-1", "accept": true, "title": "Edited"}), &[])
            .unwrap()
            .unwrap();
        assert_eq!((r["status"].as_str(), r["slug"].as_str()), (Some("accepted"), Some("accepted-note-1")));
        assert_eq!(r["redacted"], 0);
        let again = d.answer(method::DNA_REVIEW, &json!({"id": "p-1", "accept": false}), &[]).unwrap();
        assert_eq!(again.unwrap_err().code, code::CONFLICT);
        assert_eq!(pending(&mut d).as_array().unwrap().len(), 1);
        let all = d.answer(method::DNA_PROPOSALS, &json!({"status": "all"}), &[]).unwrap().unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        let n =
            d.answer(method::DNA_GET, &json!({"repo": DEMO_REPO, "slug": "accepted-note-1"}), &[]).unwrap().unwrap();
        assert_eq!(n["title"], "Edited");
    }

    #[test]
    fn note_actions_need_the_shown_hash_and_refuse_confirming_stale_notes() {
        let mut d = DnaDemo::default();
        let get = |d: &mut DnaDemo, s: &str| d.answer(method::DNA_GET, &json!({"slug": s}), &[]).unwrap().unwrap();
        let act = |d: &mut DnaDemo, s: &str, a: &str, h: &str| {
            d.answer(
                method::DNA_NOTE_ACTION,
                &json!({"repo": DEMO_REPO, "slug": s, "action": a, "expect_hash": h}),
                &[],
            )
            .unwrap()
        };
        let w = get(&mut d, "worktree-per-task");
        assert_eq!(act(&mut d, "worktree-per-task", "confirm", "nope").unwrap_err().code, code::CONFLICT);
        let v = act(&mut d, "worktree-per-task", "confirm", w["content_hash"].as_str().unwrap()).unwrap();
        assert_eq!(v["reviewed"], true);
        let s = get(&mut d, "session-boot-order");
        let h = s["content_hash"].as_str().unwrap().to_string();
        assert_eq!(act(&mut d, "session-boot-order", "confirm", &h).unwrap_err().code, code::CONFLICT);
        assert_eq!(act(&mut d, "session-boot-order", "retire", &h).unwrap()["status"], "retired");
    }

    #[test]
    fn task_context_follows_the_task_repo() {
        let mut d = DnaDemo::default();
        let tasks =
            [json!({"id": "t-claude-1", "repo_path": DEMO_REPO}), json!({"id": "t-x", "repo_path": "/repos/shop"})];
        let c = d.answer(method::DNA_TASK_CONTEXT, &json!({"task": "t-claude-1"}), &tasks).unwrap().unwrap();
        assert_eq!((c["enabled"].as_bool(), c["at_risk"].as_array().unwrap().len()), (Some(true), 1));
        assert_eq!(c["proposals"][0]["id"], "p-1");
        let off = d.answer(method::DNA_TASK_CONTEXT, &json!({"task": "t-x"}), &tasks).unwrap().unwrap();
        assert_eq!(off["enabled"], false);
        d.answer(method::DNA_ENABLE, &json!({"repo": "/repos/shop"}), &tasks).unwrap().unwrap();
        let on = d.answer(method::DNA_TASK_CONTEXT, &json!({"task": "t-x"}), &tasks).unwrap().unwrap();
        assert_eq!(on["enabled"], true);
    }
}
