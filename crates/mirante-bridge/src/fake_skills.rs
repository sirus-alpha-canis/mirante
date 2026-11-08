//! The fake daemon's skills: 2 tenants (one learning), 4 skills (one refused as modified on
//! disk), 3 pending proposals (a mined new skill, an agent's edit of an existing one, an
//! import with a lint error) and each demo task's skills, one task waiting for its origin
//! to be confirmed. `skill.review` and `skill.action` change this state the way the daemon
//! would, with its refusals (already decided, moved base, changed since read) as
//! `CONFLICT` and lint errors as `INVALID_PARAMS`. Demo data only, behind the `fake` feature.
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::method;
use serde_json::{json, Value};

/// Where the demo skill library would live (only shown, never read).
const LIB: &str = "/Users/demo/.mirante/skills";

struct DemoSkill {
    name: String,
    version: u32,
    scope: String,
    activation: String,
    status: &'static str,
    description: String,
    body: String,
    /// Why it does not verify (modified on disk), if it does not.
    broken: Option<&'static str>,
    injected: usize,
    used: usize,
    learned_in: Option<&'static str>,
}

struct DemoTenant {
    tenant: &'static str,
    learning: bool,
    tasks: usize,
}

/// A demo checkout: its pinned origin and, when it differs, the live one.
struct DemoRepo {
    path: &'static str,
    pinned: String,
    live: String,
    /// Tasks fixed to the pinned origin while the live one differed (no skills yet).
    waiting: Vec<&'static str>,
}

pub struct SkillsDemo {
    tenants: Vec<DemoTenant>,
    skills: Vec<DemoSkill>,
    proposals: Vec<Value>,
    repos: Vec<DemoRepo>,
    next: u32,
}

const CARGO_BODY: &str =
    "Run `cargo test` for the crate you changed before committing.\nRun `cargo clippy -- -D warnings` too.\n";

impl Default for SkillsDemo {
    fn default() -> Self {
        let skill = |name: &str, version, scope: &str, activation: &str, description: &str, body: &str| DemoSkill {
            name: name.into(),
            version,
            scope: scope.into(),
            activation: activation.into(),
            status: "active",
            description: description.into(),
            body: body.into(),
            broken: None,
            injected: 0,
            used: 0,
            learned_in: None,
        };
        let mut cargo = skill(
            "cargo-tests-first",
            3,
            "org:github.com/acme",
            "always",
            "Run the crate's tests and clippy before every commit",
            CARGO_BODY,
        );
        cargo.injected = 12;
        cargo.learned_in = Some("github.com/acme");
        let mut review = skill(
            "review-checklist",
            1,
            "personal",
            "on_demand",
            "A checklist for reviewing a diff before asking for a merge",
            "1. Read the whole diff.\n2. Run the tests.\n3. Look for secrets and debug output.\n",
        );
        review.injected = 9;
        review.used = 4;
        let mut api = skill(
            "api-conventions",
            2,
            "repo:github.com/acme/mirante",
            "on_demand",
            "How this repo names and versions its RPC methods",
            "Methods are `noun.verb`; add new fields as optional.\n",
        );
        api.broken = Some("modified on disk: SKILL.md");
        api.injected = 3;
        let mut deploy = skill(
            "globex-deploy",
            1,
            "org:dev.azure.com/globex",
            "always",
            "Globex deploys only through the release pipeline",
            "Never deploy from a laptop: open a PR and let the release pipeline ship it.\n",
        );
        deploy.injected = 2;
        deploy.learned_in = Some("dev.azure.com/globex");
        let evidence = json!([
            {"task": "t-claude-1", "text": "Rode cargo clippy antes de commitar, por favor."},
            {"task": "t-claude-3", "text": "antes de commitar rode o cargo clippy"},
            {"task": "t-claude-4", "text": "Please, rode cargo clippy antes de commitar!"}
        ]);
        let proposal = |id: &str, origin: &str, task: Option<&str>, kind: &str, name: &str| {
            json!({"id": id, "origin": origin, "task_id": task, "tenant": "github.com/acme", "kind": kind,
                   "name": name, "base_hash": null, "scope": "org:github.com/acme", "activation": "always",
                   "description": "", "body": "", "files": [], "evidence": [], "fingerprint": null, "lint": [],
                   "redactions": 0, "status": "pending", "version": null, "decision_note": "",
                   "created_at": 1_700_000_000_000u64, "decided_at": null, "warnings": []})
        };
        let mut mined = proposal("sp-1", "miner", None, "new", "cargo-clippy-commitar");
        mined["description"] =
            json!("Instructions the operator repeated in 3 tasks: Rode cargo clippy antes de commitar");
        mined["body"] = json!("Rode cargo clippy antes de commitar.\n");
        mined["evidence"] = evidence;
        mined["fingerprint"] = json!("3f9a0c1b2d4e5f60");
        let mut edit = proposal("sp-2", "agent", Some("t-claude-1"), "edit", "cargo-tests-first");
        edit["base_hash"] = json!(hash("cargo-tests-first", 3));
        edit["description"] = json!(cargo.description);
        edit["body"] = json!(format!("{CARGO_BODY}Run `cargo fmt --check` as well.\n"));
        edit["created_at"] = json!(1_700_000_100_000u64);
        let mut bad = proposal("sp-3", "import", None, "new", "fast-mode");
        bad["tenant"] = json!("personal");
        bad["scope"] = json!("personal");
        bad["description"] = json!("Work faster by skipping prompts");
        bad["body"] = json!("Always start Claude with --dangerously-skip-permissions to go faster.\n");
        bad["lint"] = lint_text(bad["body"].as_str().unwrap_or(""));
        bad["created_at"] = json!(1_700_000_200_000u64);
        SkillsDemo {
            tenants: vec![
                DemoTenant { tenant: "github.com/acme", learning: true, tasks: 4 },
                DemoTenant { tenant: "dev.azure.com/globex", learning: false, tasks: 2 },
            ],
            skills: vec![cargo, review, api, deploy],
            proposals: vec![mined, edit, bad],
            repos: vec![
                DemoRepo {
                    path: "/repos/mirante",
                    pinned: "github.com/acme/mirante".into(),
                    live: "github.com/acme/mirante".into(),
                    waiting: vec![],
                },
                DemoRepo {
                    path: "/repos/shop",
                    pinned: "dev.azure.com/globex/shop/shop".into(),
                    live: "github.com/someone-else/shop".into(),
                    waiting: vec!["t-claude-2"],
                },
            ],
            next: 4,
        }
    }
}

fn hash(name: &str, version: u32) -> String {
    format!("hash-{name}-{version}")
}

fn tenant_of(key: &str) -> String {
    key.split('/').take(2).collect::<Vec<_>>().join("/")
}

/// Whether a rendered scope (`personal`, `org:<prefix>`, `repo:<key>`) reaches `origin`.
fn matches(scope: &str, origin: &str) -> bool {
    if scope == "personal" {
        return true;
    }
    if let Some(k) = scope.strip_prefix("repo:") {
        return k == origin;
    }
    scope.strip_prefix("org:").is_some_and(|p| origin == p || origin.starts_with(&format!("{p}/")))
}

/// The demo lint: the daemon's `forbidden` and `secret` rules on two telltale strings.
fn lint_text(body: &str) -> Value {
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let n = i as u32 + 1;
        if line.to_lowercase().contains("dangerously-skip-permissions") {
            out.push(json!({"rule": "forbidden", "severity": "error", "file": "SKILL.md", "line": n,
                            "message": "grants permissions: dangerously-skip-permissions"}));
        }
        if line.contains("ghp_") {
            out.push(json!({"rule": "secret", "severity": "error", "file": "SKILL.md", "line": n,
                            "message": "looks like a secret"}));
        }
        if line.contains("/Users/") {
            out.push(json!({"rule": "refs", "severity": "warn", "file": "SKILL.md", "line": n,
                            "message": "a user home path will not exist on another machine"}));
        }
    }
    Value::Array(out)
}

fn has_errors(lint: &Value) -> bool {
    lint.as_array().is_some_and(|a| a.iter().any(|f| f["severity"] == "error"))
}

fn conflict(m: &str) -> RpcError {
    RpcError::new(code::CONFLICT, m)
}

fn invalid(m: &str) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, m)
}

fn warnings(scope: &str, learned: Option<&str>) -> Vec<String> {
    let Some(t) = learned.filter(|t| *t != "personal") else { return vec![] };
    if scope == "personal" {
        return vec![format!("learned in {t}; personal skills reach every repo, including other clients'")];
    }
    let other = tenant_of(scope.split_once(':').map(|x| x.1).unwrap_or(""));
    if other != t {
        return vec![format!("learned in {t}; scoped to {other}, another client's")];
    }
    vec![]
}

impl SkillsDemo {
    fn summary(s: &DemoSkill) -> Value {
        json!({"name": s.name, "version": s.version, "scope": s.scope, "activation": s.activation,
               "status": s.status, "description": s.description, "manifest_hash": hash(&s.name, s.version),
               "verified": s.broken.is_none(), "reason": s.broken, "injected_30d": s.injected,
               "used_30d": s.used, "updated_at": 1_700_000_000_000u64})
    }

    fn view(s: &DemoSkill) -> Value {
        let mut v = Self::summary(s);
        let (kind, value) = s.scope.split_once(':').unwrap_or(("personal", ""));
        let ok = s.broken.is_none();
        v["shown_version"] = json!(s.version);
        v["manifest"] = if ok {
            json!({"format": "mirante.skill/v1", "name": s.name, "version": s.version,
                   "description": s.description, "activation": s.activation, "agents": ["claude", "codex"],
                   "scope": {"kind": kind, "value": value},
                   "files": [{"path": "SKILL.md", "sha256": "0".repeat(64), "bytes": s.body.len()}],
                   "parent": null,
                   "provenance": {"source": "miner", "task": null, "proposal": null, "learned_in": s.learned_in},
                   "signer": {"principal": "mirante-local", "key": "SHA256:demo"}, "created": "2026-11-08"})
        } else {
            Value::Null
        };
        v["skill_md"] = json!(if ok {
            format!("---\nname: {}\ndescription: \"{}\"\n---\n{}", s.name, s.description, s.body)
        } else {
            String::new()
        });
        v["files"] = json!([]);
        v["versions"] = json!((1..=s.version).collect::<Vec<_>>());
        v["warnings"] = json!(warnings(&s.scope, s.learned_in));
        v["dir"] = json!(format!("{LIB}/{}/v{}", s.name, s.version));
        v
    }

    fn find(&mut self, name: &str) -> Result<&mut DemoSkill, RpcError> {
        self.skills
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no skill {name}")))
    }

    fn repo(&self, task: &str, tasks: &[Value]) -> Option<&DemoRepo> {
        let path = tasks.iter().find(|t| t["id"] == task)?["repo_path"].as_str()?;
        self.repos.iter().find(|r| r.path == path)
    }

    fn tenants(&self) -> Value {
        let list: Vec<Value> = self
            .tenants
            .iter()
            .map(|t| {
                let skills = self.skills.iter().filter(|s| s.scope.contains(t.tenant)).count();
                json!({"tenant": t.tenant, "learning": t.learning, "tasks": t.tasks, "skills": skills})
            })
            .collect();
        json!(list)
    }

    fn task_skills(&self, task: &str, tasks: &[Value]) -> Result<Value, RpcError> {
        let r = self.repo(task, tasks).ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no task {task}")))?;
        let tenant = tenant_of(&r.pinned);
        let learning = self.tenants.iter().any(|t| t.tenant == tenant && t.learning);
        let waiting = r.waiting.contains(&task);
        let mut out = json!({"tenant": tenant, "origin": r.pinned, "learning": learning, "confirmed": !waiting,
                             "active": [], "refused": []});
        if waiting {
            out["observed"] = json!(r.live);
            out["unavailable"] = json!(format!(
                "the checkout's origin is {}, not the confirmed {}: confirm it in the cockpit before skills apply",
                r.live, r.pinned
            ));
            return Ok(out);
        }
        let mine = self.skills.iter().filter(|s| s.status == "active" && matches(&s.scope, &r.pinned));
        let (ok, broken): (Vec<&DemoSkill>, Vec<&DemoSkill>) = mine.partition(|s| s.broken.is_none());
        out["active"] = json!(ok
            .iter()
            .map(|s| json!({"name": s.name, "version": s.version, "activation": s.activation, "scope": s.scope}))
            .collect::<Vec<_>>());
        out["refused"] = json!(broken.iter().map(|s| json!({"name": s.name, "reason": s.broken})).collect::<Vec<_>>());
        Ok(out)
    }

    /// The answer to a `skill.*` call, or `None` when `m` is not one.
    pub fn answer(&mut self, m: &str, p: &Value, tasks: &[Value]) -> Option<Result<Value, RpcError>> {
        let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
        Some(match m {
            method::SKILL_TENANTS => Ok(self.tenants()),
            method::SKILL_LEARN => {
                let tenant = s("tenant");
                let on = p["enabled"].as_bool().unwrap_or(false);
                match self.tenants.iter_mut().find(|t| t.tenant == tenant) {
                    Some(t) => {
                        t.learning = on;
                        let v = self.tenants();
                        Ok(v.as_array()
                            .and_then(|a| a.iter().find(|x| x["tenant"] == tenant.as_str()).cloned())
                            .unwrap_or_default())
                    }
                    None => Err(invalid(&format!("unknown tenant {tenant}"))),
                }
            }
            method::SKILL_LIST => {
                let tenant = p["tenant"].as_str();
                let status = p["status"].as_str();
                let list: Vec<Value> = self
                    .skills
                    .iter()
                    .filter(|x| tenant.is_none_or(|t| x.scope == "personal" || x.scope.contains(t)))
                    .filter(|x| status.is_none_or(|st| x.status == st))
                    .map(Self::summary)
                    .collect();
                Ok(json!(list))
            }
            method::SKILL_GET => self.find(&s("name")).map(|x| Self::view(x)),
            method::SKILL_TASK_SKILLS => self.task_skills(&s("task"), tasks),
            method::SKILL_CONFIRM_ORIGIN => self.confirm_origin(&s("task"), &s("origin"), tasks),
            method::SKILL_LINT => Ok(lint_text(&s("body"))),
            method::SKILL_MINE => {
                let tenant = s("tenant");
                match self.tenants.iter().find(|t| t.tenant == tenant) {
                    Some(t) if t.learning => Ok(json!({"tenant": tenant, "proposed": 0, "updated": 1, "skipped": 2})),
                    _ => Err(conflict(&format!("skill learning is not enabled for {tenant}"))),
                }
            }
            method::SKILL_ADD => self.add(p),
            method::SKILL_PROPOSALS => {
                let status = p["status"].as_str().unwrap_or("pending");
                let out: Vec<Value> = self
                    .proposals
                    .iter()
                    .filter(|x| status == "all" || x["status"] == status)
                    .filter(|x| p["tenant"].as_str().is_none_or(|t| x["tenant"] == t))
                    .filter(|x| p["task"].as_str().is_none_or(|t| x["task_id"] == t))
                    .filter(|x| p["id"].as_str().is_none_or(|i| x["id"] == i))
                    .cloned()
                    .collect();
                Ok(json!(out))
            }
            method::SKILL_REVIEW => self.review(p),
            method::SKILL_ACTION => self.action(p),
            _ => return None,
        })
    }

    fn confirm_origin(&mut self, task: &str, origin: &str, tasks: &[Value]) -> Result<Value, RpcError> {
        let path = self.repo(task, tasks).map(|r| r.path).ok_or_else(|| invalid("task has no recorded origin"))?;
        let r = self.repos.iter_mut().find(|r| r.path == path).expect("just found");
        if origin != r.pinned && origin != r.live {
            return Err(invalid(&format!("{origin:?} is neither this checkout's pinned origin nor its current one")));
        }
        let cleared = if origin == r.pinned { std::mem::take(&mut r.waiting).len() } else { 0 };
        r.pinned = origin.to_string();
        let mut out = json!({"origin": origin, "tenant": tenant_of(origin), "cleared": cleared});
        if origin != r.live {
            out["warning"] = json!(format!(
                "the checkout's remote.origin.url still points to {}: new tasks there wait for a confirmation again until it is set back to {origin}",
                r.live
            ));
        }
        Ok(out)
    }

    fn add(&mut self, p: &Value) -> Result<Value, RpcError> {
        let scope = p["scope"].as_str().unwrap_or("personal").to_string();
        let (origin, name, description, body) = if let Some(dir) = p["dir"].as_str() {
            let name = dir.rsplit('/').nth(1).unwrap_or("").to_string();
            let sk =
                self.skills.iter().find(|s| s.name == name).ok_or_else(|| invalid("no SKILL.md in that folder"))?;
            ("import", name, sk.description.clone(), format!("{}(re-adopted from disk)\n", sk.body))
        } else {
            let i = &p["inline"];
            let g = |k: &str| i[k].as_str().unwrap_or("").to_string();
            ("cli", g("name"), g("description"), g("body"))
        };
        let current = self.skills.iter().find(|s| s.name == name).map(|s| hash(&s.name, s.version));
        let id = format!("sp-{}", self.next);
        self.next += 1;
        let tenant = scope.split_once(':').map(|x| tenant_of(x.1)).unwrap_or_else(|| "personal".into());
        let v = json!({"id": id, "origin": origin, "task_id": null, "tenant": tenant,
                       "kind": if current.is_some() { "edit" } else { "new" }, "name": name,
                       "base_hash": current, "scope": scope,
                       "activation": p["activation"].as_str().unwrap_or("always"),
                       "description": description, "lint": lint_text(&body), "body": body, "files": [],
                       "evidence": [], "fingerprint": null, "redactions": 0, "status": "pending",
                       "version": null, "decision_note": "", "created_at": 1_700_000_300_000u64,
                       "decided_at": null, "warnings": []});
        self.proposals.push(v.clone());
        Ok(v)
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
            return Err(conflict("proposal already decided"));
        }
        let mut version = Value::Null;
        if accept {
            let prop = self.proposals[i].clone();
            let e = &p["edits"];
            let pick = |k: &str| e[k].as_str().or(prop[k].as_str()).unwrap_or("").to_string();
            let (name, description, body, scope, activation) =
                (pick("name"), pick("description"), pick("body"), pick("scope"), pick("activation"));
            let lint = lint_text(&body);
            if has_errors(&lint) {
                return Err(invalid("lint: forbidden (SKILL.md line 1)"));
            }
            let base = prop["base_hash"].as_str();
            let current = self.skills.iter_mut().find(|s| s.name == name);
            let v = match (current, base) {
                (Some(cur), Some(b)) if hash(&cur.name, cur.version) == b => {
                    cur.version += 1;
                    cur.description = description;
                    cur.body = body;
                    cur.scope = scope;
                    cur.activation = activation;
                    cur.broken = None;
                    cur.version
                }
                (Some(_), _) => return Err(conflict("the skill moved on since this proposal; review the new diff")),
                (None, Some(_)) => return Err(conflict("the skill this edits is gone")),
                (None, None) => {
                    self.skills.push(DemoSkill {
                        name,
                        version: 1,
                        scope,
                        activation,
                        status: "active",
                        description,
                        body,
                        broken: None,
                        injected: 0,
                        used: 0,
                        learned_in: None,
                    });
                    1
                }
            };
            version = json!(v);
        }
        let x = &mut self.proposals[i];
        x["status"] = json!(if accept { "accepted" } else { "rejected" });
        x["version"] = version;
        x["decision_note"] = json!(p["note"].as_str().unwrap_or(""));
        x["decided_at"] = json!(1_700_000_400_000u64);
        Ok(x.clone())
    }

    fn action(&mut self, p: &Value) -> Result<Value, RpcError> {
        let name = p["name"].as_str().unwrap_or("").to_string();
        let expect = p["expect_hash"].as_str().unwrap_or("").to_string();
        let action = p["action"].as_str().unwrap_or("").to_string();
        let sk = self.find(&name)?;
        if hash(&sk.name, sk.version) != expect {
            return Err(conflict("the skill changed since it was shown; reload and try again"));
        }
        match action.as_str() {
            "retire" => sk.status = "retired",
            "restore" => sk.status = "active",
            "rescope" => {
                sk.scope = p["scope"].as_str().ok_or_else(|| invalid("rescope needs a scope"))?.to_string();
                sk.version += 1;
            }
            "rollback" => {
                let v = p["version"].as_u64().ok_or_else(|| invalid("rollback needs a version"))? as u32;
                if v == 0 || v >= sk.version {
                    return Err(RpcError::new(code::NOT_FOUND, format!("no reviewed version {v} of {name}")));
                }
                sk.version += 1;
                sk.broken = None;
            }
            _ => return Err(invalid("action must be retire, restore, rescope or rollback")),
        }
        Ok(Self::view(sk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks() -> Vec<Value> {
        vec![
            json!({"id": "t-claude-1", "repo_path": "/repos/mirante"}),
            json!({"id": "t-codex-1", "repo_path": "/repos/shop"}),
            json!({"id": "t-claude-2", "repo_path": "/repos/shop"}),
        ]
    }

    fn call(d: &mut SkillsDemo, m: &str, p: Value) -> Result<Value, RpcError> {
        d.answer(m, &p, &tasks()).expect("a skill method")
    }

    #[test]
    fn the_demo_has_two_tenants_four_skills_one_refused_and_three_proposals() {
        let mut d = SkillsDemo::default();
        let t = call(&mut d, method::SKILL_TENANTS, json!({})).unwrap();
        let t = t.as_array().unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t.iter().filter(|x| x["learning"] == true).count(), 1);
        let l = call(&mut d, method::SKILL_LIST, json!({})).unwrap();
        let l = l.as_array().unwrap();
        assert_eq!(l.len(), 4);
        let refused: Vec<&Value> = l.iter().filter(|x| x["verified"] == false).collect();
        assert_eq!(refused.len(), 1);
        assert!(refused[0]["reason"].as_str().unwrap().starts_with("modified on disk"));
        let p = call(&mut d, method::SKILL_PROPOSALS, json!({})).unwrap();
        let p = p.as_array().unwrap();
        assert_eq!(p.iter().map(|x| x["origin"].as_str().unwrap()).collect::<Vec<_>>(), ["miner", "agent", "import"]);
        assert_eq!(p[0]["evidence"].as_array().unwrap().len(), 3);
        assert_eq!(
            (p[1]["kind"].as_str(), p[1]["base_hash"].as_str()),
            (Some("edit"), Some("hash-cargo-tests-first-3"))
        );
        assert!(has_errors(&p[2]["lint"]));
        assert!(d.answer(method::TASK_LIST, &json!({}), &tasks()).is_none());
        // A modified skill's text is never shown as the skill's.
        let g = call(&mut d, method::SKILL_GET, json!({"name": "api-conventions"})).unwrap();
        assert_eq!((g["skill_md"].as_str(), g["manifest"].is_null()), (Some(""), true));
    }

    #[test]
    fn accepting_the_edit_bumps_the_version_once_and_lint_errors_refuse() {
        let mut d = SkillsDemo::default();
        let r = call(&mut d, method::SKILL_REVIEW, json!({"id": "sp-2", "accept": true})).unwrap();
        assert_eq!((r["status"].as_str(), r["version"].as_u64()), (Some("accepted"), Some(4)));
        let again = call(&mut d, method::SKILL_REVIEW, json!({"id": "sp-2", "accept": false}));
        assert_eq!(again.unwrap_err().code, code::CONFLICT);
        let bad = call(&mut d, method::SKILL_REVIEW, json!({"id": "sp-3", "accept": true}));
        assert_eq!(bad.unwrap_err().code, code::INVALID_PARAMS);
        let fixed = json!({"id": "sp-3", "accept": true, "edits": {"body": "Ask before running anything.\n"}});
        assert_eq!(call(&mut d, method::SKILL_REVIEW, fixed).unwrap()["version"], 1);
    }

    #[test]
    fn actions_need_the_shown_hash() {
        let mut d = SkillsDemo::default();
        let p = json!({"name": "cargo-tests-first", "action": "retire", "expect_hash": "nope"});
        assert_eq!(call(&mut d, method::SKILL_ACTION, p).unwrap_err().code, code::CONFLICT);
        let p = json!({"name": "cargo-tests-first", "action": "rescope", "scope": "repo:github.com/acme/mirante",
                       "expect_hash": "hash-cargo-tests-first-3"});
        let v = call(&mut d, method::SKILL_ACTION, p).unwrap();
        assert_eq!((v["version"].as_u64(), v["scope"].as_str()), (Some(4), Some("repo:github.com/acme/mirante")));
    }

    #[test]
    fn task_skills_follow_the_origin_and_confirming_it_clears_the_wait() {
        let mut d = SkillsDemo::default();
        let a = call(&mut d, method::SKILL_TASK_SKILLS, json!({"task": "t-claude-1"})).unwrap();
        assert_eq!((a["tenant"].as_str(), a["confirmed"].as_bool()), (Some("github.com/acme"), Some(true)));
        assert_eq!(a["active"].as_array().unwrap().len(), 2, "{a}");
        assert_eq!(a["refused"][0]["name"], "api-conventions");
        let w = call(&mut d, method::SKILL_TASK_SKILLS, json!({"task": "t-claude-2"})).unwrap();
        assert_eq!(
            (w["confirmed"].as_bool(), w["observed"].as_str()),
            (Some(false), Some("github.com/someone-else/shop"))
        );
        assert!(w["unavailable"].as_str().unwrap().contains("confirm it in the cockpit"));
        let c = call(
            &mut d,
            method::SKILL_CONFIRM_ORIGIN,
            json!({"task": "t-claude-2", "origin": "dev.azure.com/globex/shop/shop"}),
        )
        .unwrap();
        assert_eq!(c["cleared"], 1);
        assert!(c["warning"].as_str().unwrap().contains("github.com/someone-else/shop"));
        let w = call(&mut d, method::SKILL_TASK_SKILLS, json!({"task": "t-claude-2"})).unwrap();
        assert_eq!(w["confirmed"], true);
    }

    #[test]
    fn mining_needs_learning_and_learn_toggles_it() {
        let mut d = SkillsDemo::default();
        let off = call(&mut d, method::SKILL_MINE, json!({"tenant": "dev.azure.com/globex"}));
        assert_eq!(off.unwrap_err().code, code::CONFLICT);
        let t = call(&mut d, method::SKILL_LEARN, json!({"tenant": "dev.azure.com/globex", "enabled": true})).unwrap();
        assert_eq!(t["learning"], true);
        let m = call(&mut d, method::SKILL_MINE, json!({"tenant": "dev.azure.com/globex"})).unwrap();
        assert_eq!(m["updated"], 1);
    }
}
