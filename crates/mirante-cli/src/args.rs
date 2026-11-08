//! Command-line parsing (no dependencies). Flags may come before or after positionals;
//! `--` ends flags; `--flag=value` is accepted.
use std::collections::HashMap;

use mirante_core::model::AgentKind;

pub const USAGE: &str = "\
usage: mirante <command>

  new <repo> --agent claude|codex --branch <name> [--base <ref>] [--title <t>] [--trust] <goal...>
                         create a worktree + task and start the agent with <goal>
  ls [--all]             tasks with status and what needs you (--all: include archived)
  show <task>            one task in detail
  watch                  live status board (rings the bell when a task needs you)
  attach <task>          live terminal (Claude) or event stream (Codex); detach with Ctrl-]
  say <task> [--queue] <message...>
                         steer now (default) or queue for the end of the running turn
  queue ls <task>        the messages queued for the turn end, numbered (1 = next)
  queue edit <task> <n> <message...> | queue rm <task> <n> | queue mv <task> <n> <to>
                         change, drop or move queued message <n> (not once it is delivered)
  perms                  pending permission requests
  allow <perm> | deny <perm>
                         answer one request, once (never \"always\"); a Claude dialog is
                         answered in the terminal (offers to attach) unless the daemon runs
                         with MIRANTE_CLAUDE_INLINE_ANSWERS=1 and has the evidence
  interrupt <task>       stop the current step (Esc / turn/interrupt)
  stop <task>            end the agent process (resumable)
  resume <task> [--trust] [--fresh]
                         restart the agent on its native session (context kept);
                         --fresh starts a new conversation instead (your explicit choice)
  hibernate <task>       end an idle agent to free memory (resumable; never one waiting on you)
  wake <task> [--trust]  resume a hibernated task on its native conversation
  doctor [--kill <pid>]  orphan processes / checkouts and memory; --kill asks, then kills one orphan
  archive <task>         remove the worktree (branch kept) and hide the task
  memory enable|disable [<repo>]
                         repo memory for new sessions of the repo's tasks (explicit, per repo)
  memory ls [<repo>] | memory show <slug> [--repo <r>] | memory search <query...> [--repo <r>]
                         notes with their state (fresh, stale, unverifiable) and review
  memory proposals [--repo <r>] [--all]
                         the review queue (--all: decided ones too)
  memory accept <proposal> [--title <t>] [--note <why>] | memory reject <proposal> [--note <why>]
  memory confirm <slug> [--repo <r>] | memory retire <slug> [--repo <r>]
                         a person's decision: you type the verb on this terminal to confirm
                         (an intent check, not a security boundary: the daemon refuses
                         Mirante-launched agents, also on an agent's terminal). Confirm
                         re-anchors at HEAD; retire keeps the file with status retired
  memory check [<repo>] [--offline [--rev <rev>]]
                         recheck every anchor; exits 1 when one is stale or unverifiable or
                         a note does not parse, 3 when it could not check (not a checkout,
                         a bad --rev, no daemon, a git failure, a shallow clone). --offline
                         reads the files and git directly (no daemon; for CI: check out full
                         history, e.g. actions/checkout with fetch-depth: 0); with --rev <rev>
                         it checks the notes as committed at <rev>, against <rev> (not the
                         working tree)
  memory propose --task <task> --title <t> --kind fact|decision|pattern|gotcha
                 --cite <path:start-end[#symbol]>... <body...>
                         propose a note yourself, citing committed code in the task worktree
  skills tenant [<repo>]  the repo's normalized origin and client (tenant), and its learning state
  skills learn on|off [<repo>]
                         let Mirante read this client's goals, messages and denied permissions
                         (on this machine only) to find repeated instructions; typed on the terminal
  skills ls | skills show <name> [--version <n>]
                         the library: scope, activation, verification, 30-day usage
  skills proposals [--all]
                         the review queue (--all: decided ones too)
  skills accept <proposal> [--scope <s>] [--activation always|on_demand] [--note <why>]
  skills reject <proposal> [--note <why>]
  skills retire|restore <name> | skills rescope <name> <scope> | skills rollback <name> <version>
                         a person's decision, typed on this terminal like memory's (accept writes
                         and signs a new version; rescope and rollback make new versions)
  skills mine [<repo>]   find instructions repeated in 3+ tasks of the repo's client (learning on)
  skills add <dir> --scope <s> [--activation always|on_demand]
                         propose a skill folder (SKILL.md + text files) for review
  skills lint <SKILL.md> offline lint; exits 1 on an error finding, 3 when it could not run
  daemon start|stop|restart|status
                         restart: stop (ends live sessions) then start, e.g. after an upgrade;
                         the new daemon resumes them unless auto-resume is off
                         (MIRANTE_AUTO_RESUME=off, or the cockpit's setting)

<task> is a task id, a unique id suffix (4+ chars) or the branch name.
<repo> defaults to the git checkout of the current directory.
<scope> is personal, org:<host>/<owner>[/…] or repo:<host>/<path>.
<perm> is a permission id or a unique suffix.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueCmd {
    Ls { task: String },
    Edit { task: String, n: usize, text: String },
    Rm { task: String, n: usize },
    Mv { task: String, n: usize, to: usize },
}

/// Code a hand-written proposal cites: `path:start-end[#symbol]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cite {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub symbol: Option<String>,
}

/// `mirante memory …`. A `repo` of `None` means the current directory's checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCmd {
    /// Accept (writes the note, optionally retitled) or reject a proposal.
    Review {
        id: String,
        accept: bool,
        title: Option<String>,
        note: Option<String>,
    },
    /// `enable` (`on`) or `disable` memory for new sessions of the repo's tasks.
    Enable {
        repo: Option<String>,
        on: bool,
    },
    Ls {
        repo: Option<String>,
    },
    Show {
        slug: String,
        repo: Option<String>,
    },
    Search {
        query: String,
        repo: Option<String>,
    },
    /// The review queue (`all`: decided ones too).
    Proposals {
        repo: Option<String>,
        all: bool,
    },
    /// `confirm` (re-anchor at HEAD, mark reviewed) or `retire` a note.
    NoteAction {
        slug: String,
        repo: Option<String>,
        confirm: bool,
    },
    /// Recheck every anchor; `offline` reads the files and git directly, no daemon.
    Check {
        repo: Option<String>,
        rev: Option<String>,
        offline: bool,
    },
    /// A person's own proposal, cited at the task worktree's HEAD.
    Propose {
        task: String,
        title: String,
        kind: String,
        cites: Vec<Cite>,
        body: String,
    },
}

/// What `mirante skills retire|restore|rescope|rollback` does to a skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOp {
    Retire,
    Restore,
    Rescope(String),
    Rollback(u32),
}

/// `mirante skills …`. A `repo` of `None` means the current directory's checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillsCmd {
    Tenant {
        repo: Option<String>,
    },
    Learn {
        on: bool,
        repo: Option<String>,
    },
    Ls,
    Show {
        name: String,
        version: Option<u32>,
    },
    Proposals {
        all: bool,
    },
    /// Accept (optionally rescoped / with another activation) or reject a proposal.
    Review {
        id: String,
        accept: bool,
        scope: Option<String>,
        activation: Option<String>,
        note: Option<String>,
    },
    Action {
        name: String,
        op: SkillOp,
    },
    Mine {
        repo: Option<String>,
    },
    Add {
        dir: String,
        scope: String,
        activation: Option<String>,
    },
    /// Offline lint of a `SKILL.md` (and the files beside it).
    Lint {
        file: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonCmd {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    New {
        repo: String,
        agent: AgentKind,
        branch: String,
        base: Option<String>,
        title: Option<String>,
        goal: String,
        trust: bool,
    },
    Ls {
        all: bool,
    },
    Show {
        task: String,
    },
    Watch,
    Attach {
        task: String,
    },
    Say {
        task: String,
        text: String,
        queue: bool,
    },
    Queue(QueueCmd),
    Perms,
    Answer {
        id: String,
        allow: bool,
    },
    Interrupt {
        task: String,
    },
    Stop {
        task: String,
    },
    Resume {
        task: String,
        trust: bool,
        fresh: bool,
    },
    Archive {
        task: String,
    },
    Hibernate {
        task: String,
    },
    Wake {
        task: String,
        trust: bool,
    },
    Doctor {
        kill: Option<i32>,
    },
    Daemon(DaemonCmd),
    Memory(MemoryCmd),
    Skills(SkillsCmd),
    Help,
}

struct Parsed {
    positional: Vec<String>,
    /// The last value of each flag.
    values: HashMap<String, String>,
    /// Every flag value, in order (for repeatable flags).
    all: Vec<(String, String)>,
    bools: Vec<String>,
}

fn split(args: &[String], with_value: &[&str], bools: &[&str]) -> Result<Parsed, String> {
    let mut p = Parsed { positional: Vec::new(), values: HashMap::new(), all: Vec::new(), bools: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            p.positional.extend(it.by_ref().cloned());
            break;
        }
        if !a.starts_with("--") {
            p.positional.push(a.clone());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        if with_value.contains(&name) {
            let v = match inline {
                Some(v) => v,
                None => it.next().cloned().ok_or_else(|| format!("{name} needs a value"))?,
            };
            p.all.push((name.to_string(), v.clone()));
            p.values.insert(name.to_string(), v);
        } else if bools.contains(&name) && inline.is_none() {
            p.bools.push(name.to_string());
        } else {
            return Err(format!("unknown option {a}"));
        }
    }
    Ok(p)
}

fn one(p: &Parsed, what: &str) -> Result<String, String> {
    match p.positional.as_slice() {
        [x] => Ok(x.clone()),
        [] => Err(format!("missing <{what}>")),
        _ => Err(format!("expected one <{what}>, got {}", p.positional.len())),
    }
}

/// The task followed by a free text of one or more words.
fn task_and_text(p: &Parsed, what: &str) -> Result<(String, String), String> {
    let (task, words) = p.positional.split_first().ok_or("missing <task>")?;
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err(format!("missing <{what}>"));
    }
    Ok((task.clone(), text))
}

/// A 1-based queue position.
fn position(v: &str) -> Result<usize, String> {
    v.parse::<usize>().ok().filter(|n| *n >= 1).ok_or_else(|| format!("{v} is not a queue position (1 = next)"))
}

const MEMORY_VERBS: &str =
    "enable, disable, ls, show, search, proposals, accept, reject, confirm, retire, check, propose";

/// At most one positional: the repo (default: the current directory's checkout).
fn repo_arg(p: &Parsed) -> Result<Option<String>, String> {
    match p.positional.as_slice() {
        [] => Ok(None),
        [r] => Ok(Some(r.clone())),
        more => Err(format!("expected at most one <repo>, got {}", more.len())),
    }
}

/// `path:start-end[#symbol]`, 1-based inclusive lines.
fn cite(v: &str) -> Result<Cite, String> {
    let bad = || format!("--cite {v}: expected path:start-end[#symbol]");
    let (path, rest) = v.rsplit_once(':').ok_or_else(bad)?;
    let (range, symbol) = match rest.split_once('#') {
        Some((r, s)) if !s.is_empty() => (r, Some(s.to_string())),
        Some(_) => return Err(bad()),
        None => (rest, None),
    };
    let (a, b) = range.split_once('-').ok_or_else(bad)?;
    let line = |x: &str| x.parse::<u32>().ok().filter(|n| *n >= 1).ok_or_else(bad);
    let (start, end) = (line(a)?, line(b)?);
    if path.is_empty() || end < start {
        return Err(format!("--cite {v}: lines must be start-end with start <= end"));
    }
    Ok(Cite { path: path.to_string(), start, end, symbol })
}

fn memory(args: &[String]) -> Result<MemoryCmd, String> {
    let (op, rest) = args.split_first().ok_or(format!("memory <command> ({MEMORY_VERBS})"))?;
    let none: &[&str] = &[];
    let repo_flag = |p: &Parsed| p.values.get("--repo").cloned();
    match op.as_str() {
        "accept" | "reject" => {
            let accept = op == "accept";
            let flags: &[&str] = if accept { &["--title", "--note"] } else { &["--note"] };
            let p = split(rest, flags, none)?;
            let id = one(&p, "proposal")?;
            Ok(MemoryCmd::Review {
                id,
                accept,
                title: p.values.get("--title").cloned(),
                note: p.values.get("--note").cloned(),
            })
        }
        "enable" | "disable" => {
            Ok(MemoryCmd::Enable { repo: repo_arg(&split(rest, none, none)?)?, on: op == "enable" })
        }
        "ls" | "list" => Ok(MemoryCmd::Ls { repo: repo_arg(&split(rest, none, none)?)? }),
        "show" => {
            let p = split(rest, &["--repo"], none)?;
            Ok(MemoryCmd::Show { slug: one(&p, "slug")?, repo: repo_flag(&p) })
        }
        "search" => {
            let p = split(rest, &["--repo"], none)?;
            let query = p.positional.join(" ");
            if query.trim().is_empty() {
                return Err("missing <query>".into());
            }
            Ok(MemoryCmd::Search { query, repo: repo_flag(&p) })
        }
        "proposals" => {
            let p = split(rest, &["--repo"], &["--all"])?;
            if !p.positional.is_empty() {
                return Err("proposals takes no arguments (use --repo)".into());
            }
            Ok(MemoryCmd::Proposals { repo: repo_flag(&p), all: !p.bools.is_empty() })
        }
        "confirm" | "retire" => {
            let p = split(rest, &["--repo"], none)?;
            Ok(MemoryCmd::NoteAction { slug: one(&p, "slug")?, repo: repo_flag(&p), confirm: op == "confirm" })
        }
        "check" => {
            let p = split(rest, &["--rev"], &["--offline"])?;
            let offline = !p.bools.is_empty();
            let rev = p.values.get("--rev").cloned();
            if rev.is_some() && !offline {
                return Err("--rev needs --offline (the daemon checks the checkout's HEAD)".into());
            }
            Ok(MemoryCmd::Check { repo: repo_arg(&p)?, rev, offline })
        }
        "propose" => {
            let p = split(rest, &["--task", "--title", "--kind", "--cite"], none)?;
            let task = p.values.get("--task").cloned().ok_or("missing --task <task>")?;
            let title = p.values.get("--title").cloned().ok_or("missing --title <title>")?;
            let kind = p.values.get("--kind").cloned().ok_or("missing --kind fact|decision|pattern|gotcha")?;
            if !["fact", "decision", "pattern", "gotcha"].contains(&kind.as_str()) {
                return Err(format!("unknown kind {kind} (fact, decision, pattern, gotcha)"));
            }
            let cites: Vec<Cite> =
                p.all.iter().filter(|(k, _)| k == "--cite").map(|(_, v)| cite(v)).collect::<Result<_, _>>()?;
            if cites.is_empty() {
                return Err("missing --cite path:start-end[#symbol]".into());
            }
            let body = p.positional.join(" ");
            if body.trim().is_empty() {
                return Err("missing <body>".into());
            }
            Ok(MemoryCmd::Propose { task, title, kind, cites, body })
        }
        other => Err(format!("unknown memory command {other} ({MEMORY_VERBS})")),
    }
}

const SKILLS_VERBS: &str =
    "tenant, learn, ls, show, proposals, accept, reject, retire, restore, rescope, rollback, mine, add, lint";

/// `always` or `on_demand`.
fn activation(v: Option<&String>) -> Result<Option<String>, String> {
    match v.map(String::as_str) {
        None => Ok(None),
        Some(a @ ("always" | "on_demand")) => Ok(Some(a.to_string())),
        Some(other) => Err(format!("unknown activation {other} (always or on_demand)")),
    }
}

fn skills(args: &[String]) -> Result<SkillsCmd, String> {
    let (op, rest) = args.split_first().ok_or(format!("skills <command> ({SKILLS_VERBS})"))?;
    let none: &[&str] = &[];
    let two = |p: &Parsed, second: &str| -> Result<(String, String), String> {
        match p.positional.as_slice() {
            [a, b] => Ok((a.clone(), b.clone())),
            [] => Err("missing <name>".into()),
            [_] => Err(format!("missing <{second}>")),
            more => Err(format!("expected <name> <{second}>, got {} arguments", more.len())),
        }
    };
    match op.as_str() {
        "tenant" => Ok(SkillsCmd::Tenant { repo: repo_arg(&split(rest, none, none)?)? }),
        "learn" => {
            let usage = || "skills learn on|off [<repo>]".to_string();
            let p = split(rest, none, none)?;
            let (on, repo) = match p.positional.as_slice() {
                [w, more @ ..] if w == "on" || w == "off" => (w == "on", more),
                _ => return Err(usage()),
            };
            match repo {
                [] => Ok(SkillsCmd::Learn { on, repo: None }),
                [r] => Ok(SkillsCmd::Learn { on, repo: Some(r.clone()) }),
                _ => Err(usage()),
            }
        }
        "ls" | "list" => {
            if !split(rest, none, none)?.positional.is_empty() {
                return Err("ls takes no arguments".into());
            }
            Ok(SkillsCmd::Ls)
        }
        "show" => {
            let p = split(rest, &["--version"], none)?;
            let version = match p.values.get("--version") {
                Some(v) => Some(v.parse::<u32>().map_err(|_| format!("--version needs a version number, got {v}"))?),
                None => None,
            };
            Ok(SkillsCmd::Show { name: one(&p, "name")?, version })
        }
        "proposals" => {
            let p = split(rest, none, &["--all"])?;
            if !p.positional.is_empty() {
                return Err("proposals takes no arguments".into());
            }
            Ok(SkillsCmd::Proposals { all: !p.bools.is_empty() })
        }
        "accept" | "reject" => {
            let accept = op == "accept";
            let flags: &[&str] = if accept { &["--scope", "--activation", "--note"] } else { &["--note"] };
            let p = split(rest, flags, none)?;
            Ok(SkillsCmd::Review {
                id: one(&p, "proposal")?,
                accept,
                scope: p.values.get("--scope").cloned(),
                activation: activation(p.values.get("--activation"))?,
                note: p.values.get("--note").cloned(),
            })
        }
        "retire" | "restore" => {
            let name = one(&split(rest, none, none)?, "name")?;
            Ok(SkillsCmd::Action { name, op: if op == "retire" { SkillOp::Retire } else { SkillOp::Restore } })
        }
        "rescope" => {
            let (name, scope) = two(&split(rest, none, none)?, "scope")?;
            Ok(SkillsCmd::Action { name, op: SkillOp::Rescope(scope) })
        }
        "rollback" => {
            let (name, v) = two(&split(rest, none, none)?, "version")?;
            let n = v.parse::<u32>().map_err(|_| format!("rollback needs a version number, got {v}"))?;
            Ok(SkillsCmd::Action { name, op: SkillOp::Rollback(n) })
        }
        "mine" => Ok(SkillsCmd::Mine { repo: repo_arg(&split(rest, none, none)?)? }),
        "add" => {
            let p = split(rest, &["--scope", "--activation"], none)?;
            let dir = one(&p, "dir")?;
            let scope = p.values.get("--scope").cloned().ok_or("missing --scope personal|org:<prefix>|repo:<key>")?;
            Ok(SkillsCmd::Add { dir, scope, activation: activation(p.values.get("--activation"))? })
        }
        "lint" => Ok(SkillsCmd::Lint { file: one(&split(rest, none, none)?, "SKILL.md")? }),
        other => Err(format!("unknown skills command {other} ({SKILLS_VERBS})")),
    }
}

fn queue(args: &[String]) -> Result<QueueCmd, String> {
    let none: &[&str] = &[];
    let (op, rest) = args.split_first().ok_or("queue ls|edit|rm|mv <task> ...")?;
    let p = split(rest, none, none)?;
    let at = |i: usize, what: &str| p.positional.get(i).cloned().ok_or(format!("missing <{what}>"));
    let exactly = |n: usize| {
        if p.positional.len() > n {
            Err(format!("queue {op} takes {n} arguments"))
        } else {
            Ok(())
        }
    };
    match op.as_str() {
        "ls" => {
            exactly(1)?;
            Ok(QueueCmd::Ls { task: at(0, "task")? })
        }
        "edit" => {
            let (task, n) = (at(0, "task")?, position(&at(1, "n")?)?);
            let text = p.positional[2..].join(" ");
            if text.trim().is_empty() {
                return Err("missing <message>".into());
            }
            Ok(QueueCmd::Edit { task, n, text })
        }
        "rm" => {
            exactly(2)?;
            Ok(QueueCmd::Rm { task: at(0, "task")?, n: position(&at(1, "n")?)? })
        }
        "mv" => {
            exactly(3)?;
            Ok(QueueCmd::Mv { task: at(0, "task")?, n: position(&at(1, "n")?)?, to: position(&at(2, "to")?)? })
        }
        other => Err(format!("unknown queue command {other} (ls, edit, rm, mv)")),
    }
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Ok(Cmd::Help);
    };
    let none: &[&str] = &[];
    match sub.as_str() {
        "new" => {
            let p = split(rest, &["--agent", "--branch", "--base", "--title"], &["--trust"])?;
            let (repo, goal) = task_and_text(&p, "goal").map_err(|e| e.replace("<task>", "<repo>"))?;
            let agent = match p.values.get("--agent").map(String::as_str) {
                Some("claude") => AgentKind::Claude,
                Some("codex") => AgentKind::Codex,
                Some(other) => return Err(format!("unknown agent {other} (claude or codex)")),
                None => return Err("missing --agent claude|codex".into()),
            };
            let branch = p.values.get("--branch").cloned().ok_or("missing --branch <name>")?;
            Ok(Cmd::New {
                repo,
                agent,
                branch,
                base: p.values.get("--base").cloned(),
                title: p.values.get("--title").cloned(),
                goal,
                trust: p.bools.iter().any(|b| b == "--trust"),
            })
        }
        "ls" | "list" => {
            let p = split(rest, none, &["--all"])?;
            if !p.positional.is_empty() {
                return Err("ls takes no arguments".into());
            }
            Ok(Cmd::Ls { all: !p.bools.is_empty() })
        }
        "show" => Ok(Cmd::Show { task: one(&split(rest, none, none)?, "task")? }),
        "watch" => split(rest, none, none)?
            .positional
            .is_empty()
            .then_some(Cmd::Watch)
            .ok_or("watch takes no arguments".into()),
        "attach" => Ok(Cmd::Attach { task: one(&split(rest, none, none)?, "task")? }),
        "say" => {
            let p = split(rest, none, &["--queue"])?;
            let (task, text) = task_and_text(&p, "message")?;
            Ok(Cmd::Say { task, text, queue: !p.bools.is_empty() })
        }
        "queue" => queue(rest).map(Cmd::Queue),
        "memory" => memory(rest).map(Cmd::Memory),
        "skills" => skills(rest).map(Cmd::Skills),
        "perms" => Ok(Cmd::Perms),
        "allow" | "deny" => Ok(Cmd::Answer { id: one(&split(rest, none, none)?, "perm")?, allow: sub == "allow" }),
        "interrupt" => Ok(Cmd::Interrupt { task: one(&split(rest, none, none)?, "task")? }),
        "stop" => Ok(Cmd::Stop { task: one(&split(rest, none, none)?, "task")? }),
        "resume" => {
            let p = split(rest, none, &["--trust", "--fresh"])?;
            let has = |f: &str| p.bools.iter().any(|b| b == f);
            Ok(Cmd::Resume { task: one(&p, "task")?, trust: has("--trust"), fresh: has("--fresh") })
        }
        "archive" => Ok(Cmd::Archive { task: one(&split(rest, none, none)?, "task")? }),
        "hibernate" => Ok(Cmd::Hibernate { task: one(&split(rest, none, none)?, "task")? }),
        "wake" => {
            let p = split(rest, none, &["--trust"])?;
            Ok(Cmd::Wake { task: one(&p, "task")?, trust: !p.bools.is_empty() })
        }
        "doctor" => {
            let p = split(rest, &["--kill"], none)?;
            if !p.positional.is_empty() {
                return Err("doctor takes no arguments".into());
            }
            let kill = match p.values.get("--kill") {
                Some(v) => Some(v.parse::<i32>().map_err(|_| format!("--kill needs a pid, got {v}"))?),
                None => None,
            };
            Ok(Cmd::Doctor { kill })
        }
        "daemon" => match one(&split(rest, none, none)?, "start|stop|restart|status")?.as_str() {
            "start" => Ok(Cmd::Daemon(DaemonCmd::Start)),
            "stop" => Ok(Cmd::Daemon(DaemonCmd::Stop)),
            "restart" => Ok(Cmd::Daemon(DaemonCmd::Restart)),
            "status" => Ok(Cmd::Daemon(DaemonCmd::Status)),
            other => Err(format!("unknown daemon command {other}")),
        },
        "help" | "-h" | "--help" => Ok(Cmd::Help),
        other => Err(format!("unknown command {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cmd, String> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn new_takes_flags_anywhere_and_joins_the_goal() {
        assert_eq!(
            p("new ~/app --agent codex fix the login --branch feat/login --base=release").unwrap(),
            Cmd::New {
                repo: "~/app".into(),
                agent: AgentKind::Codex,
                branch: "feat/login".into(),
                base: Some("release".into()),
                title: None,
                goal: "fix the login".into(),
                trust: false,
            }
        );
        assert!(matches!(p("new . --agent claude --branch b --trust go").unwrap(), Cmd::New { trust: true, .. }));
    }

    #[test]
    fn new_reports_what_is_missing() {
        assert_eq!(p("new . --branch b go").unwrap_err(), "missing --agent claude|codex");
        assert_eq!(p("new . --agent claude go").unwrap_err(), "missing --branch <name>");
        assert_eq!(p("new . --agent claude --branch b").unwrap_err(), "missing <goal>");
        assert_eq!(p("new . --agent gpt --branch b go").unwrap_err(), "unknown agent gpt (claude or codex)");
        assert_eq!(p("new . --agent").unwrap_err(), "--agent needs a value");
    }

    #[test]
    fn say_defaults_to_steer_and_double_dash_protects_text() {
        assert_eq!(
            p("say feat use tabs").unwrap(),
            Cmd::Say { task: "feat".into(), text: "use tabs".into(), queue: false }
        );
        assert_eq!(
            p("say --queue feat -- --not-a-flag").unwrap(),
            Cmd::Say { task: "feat".into(), text: "--not-a-flag".into(), queue: true }
        );
        assert_eq!(p("say feat").unwrap_err(), "missing <message>");
    }

    #[test]
    fn queue_commands_take_one_based_positions() {
        assert_eq!(p("queue ls feat").unwrap(), Cmd::Queue(QueueCmd::Ls { task: "feat".into() }));
        assert_eq!(
            p("queue edit feat 2 run all the tests").unwrap(),
            Cmd::Queue(QueueCmd::Edit { task: "feat".into(), n: 2, text: "run all the tests".into() })
        );
        assert_eq!(p("queue rm feat 1").unwrap(), Cmd::Queue(QueueCmd::Rm { task: "feat".into(), n: 1 }));
        assert_eq!(p("queue mv feat 3 1").unwrap(), Cmd::Queue(QueueCmd::Mv { task: "feat".into(), n: 3, to: 1 }));
        assert_eq!(p("queue rm feat 0").unwrap_err(), "0 is not a queue position (1 = next)");
        assert_eq!(p("queue edit feat 1").unwrap_err(), "missing <message>");
        assert_eq!(p("queue mv feat 1").unwrap_err(), "missing <to>");
        assert_eq!(p("queue rm feat 1 2").unwrap_err(), "queue rm takes 2 arguments");
        assert_eq!(p("queue pop feat").unwrap_err(), "unknown queue command pop (ls, edit, rm, mv)");
    }

    #[test]
    fn memory_review_commands() {
        assert_eq!(
            p("memory accept p1 --title Better --note ok").unwrap(),
            Cmd::Memory(MemoryCmd::Review {
                id: "p1".into(),
                accept: true,
                title: Some("Better".into()),
                note: Some("ok".into())
            })
        );
        assert_eq!(
            p("memory reject p1").unwrap(),
            Cmd::Memory(MemoryCmd::Review { id: "p1".into(), accept: false, title: None, note: None })
        );
        assert_eq!(p("memory reject p1 --title x").unwrap_err(), "unknown option --title");
        assert_eq!(p("memory accept").unwrap_err(), "missing <proposal>");
        assert!(p("memory forget p1").unwrap_err().starts_with("unknown memory command forget ("));
    }

    #[test]
    fn memory_subcommands_parse() {
        let m = |s: &str| {
            p(s).map(|c| match c {
                Cmd::Memory(m) => m,
                other => panic!("{other:?}"),
            })
        };
        assert_eq!(m("memory enable").unwrap(), MemoryCmd::Enable { repo: None, on: true });
        assert_eq!(m("memory disable ~/app").unwrap(), MemoryCmd::Enable { repo: Some("~/app".into()), on: false });
        assert_eq!(m("memory ls").unwrap(), MemoryCmd::Ls { repo: None });
        assert_eq!(m("memory ls /r").unwrap(), MemoryCmd::Ls { repo: Some("/r".into()) });
        assert_eq!(
            m("memory show hook-spool --repo /r").unwrap(),
            MemoryCmd::Show { slug: "hook-spool".into(), repo: Some("/r".into()) }
        );
        assert_eq!(
            m("memory search hook spool fallback").unwrap(),
            MemoryCmd::Search { query: "hook spool fallback".into(), repo: None }
        );
        assert_eq!(
            m("memory proposals --all --repo /r").unwrap(),
            MemoryCmd::Proposals { repo: Some("/r".into()), all: true }
        );
        assert_eq!(m("memory proposals").unwrap(), MemoryCmd::Proposals { repo: None, all: false });
        assert_eq!(
            m("memory confirm a-note").unwrap(),
            MemoryCmd::NoteAction { slug: "a-note".into(), repo: None, confirm: true }
        );
        assert_eq!(
            m("memory retire a-note --repo /r").unwrap(),
            MemoryCmd::NoteAction { slug: "a-note".into(), repo: Some("/r".into()), confirm: false }
        );
        assert_eq!(m("memory check").unwrap(), MemoryCmd::Check { repo: None, rev: None, offline: false });
        assert_eq!(
            m("memory check /r --offline --rev HEAD~1").unwrap(),
            MemoryCmd::Check { repo: Some("/r".into()), rev: Some("HEAD~1".into()), offline: true }
        );
        assert_eq!(
            m("memory propose --task feat --title Spool --kind gotcha --cite src/a.rs:1-3#spool --cite b.rs:4-4 uses the spool")
                .unwrap(),
            MemoryCmd::Propose {
                task: "feat".into(),
                title: "Spool".into(),
                kind: "gotcha".into(),
                cites: vec![
                    Cite { path: "src/a.rs".into(), start: 1, end: 3, symbol: Some("spool".into()) },
                    Cite { path: "b.rs".into(), start: 4, end: 4, symbol: None },
                ],
                body: "uses the spool".into(),
            }
        );
        // Errors.
        assert_eq!(m("memory check --offline --rev").unwrap_err(), "--rev needs a value");
        assert!(m("memory check --rev HEAD").unwrap_err().contains("--offline"));
        assert_eq!(m("memory search").unwrap_err(), "missing <query>");
        assert_eq!(m("memory show").unwrap_err(), "missing <slug>");
        assert_eq!(m("memory ls a b").unwrap_err(), "expected at most one <repo>, got 2");
        assert!(m("memory forget x").unwrap_err().starts_with("unknown memory command forget"));
        assert!(m("memory").is_err());
        let propose = "memory propose --task t --title T --kind fact";
        assert_eq!(m(&format!("{propose} body")).unwrap_err(), "missing --cite path:start-end[#symbol]");
        assert_eq!(m(&format!("{propose} --cite a.rs:1-2")).unwrap_err(), "missing <body>");
        assert!(m(&format!("{propose} --cite a.rs:2-1 b")).unwrap_err().contains("a.rs:2-1"));
        assert!(m(&format!("{propose} --cite a.rs b")).unwrap_err().contains("path:start-end"));
        assert!(m("memory propose --task t --title T --kind rumor --cite a.rs:1-1 b").unwrap_err().contains("gotcha"));
        assert_eq!(m("memory propose --title T --kind fact --cite a.rs:1-1 b").unwrap_err(), "missing --task <task>");
    }

    #[test]
    fn simple_commands() {
        assert_eq!(p("").unwrap(), Cmd::Help);
        assert_eq!(p("ls --all").unwrap(), Cmd::Ls { all: true });
        assert_eq!(p("allow ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: true });
        assert_eq!(p("deny ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: false });
        assert_eq!(p("resume feat --trust").unwrap(), Cmd::Resume { task: "feat".into(), trust: true, fresh: false });
        assert_eq!(p("resume --fresh feat").unwrap(), Cmd::Resume { task: "feat".into(), trust: false, fresh: true });
        assert_eq!(p("daemon status").unwrap(), Cmd::Daemon(DaemonCmd::Status));
        assert_eq!(p("daemon restart").unwrap(), Cmd::Daemon(DaemonCmd::Restart));
        assert_eq!(p("watch").unwrap(), Cmd::Watch);
        assert_eq!(p("hibernate feat").unwrap(), Cmd::Hibernate { task: "feat".into() });
        assert_eq!(p("wake feat --trust").unwrap(), Cmd::Wake { task: "feat".into(), trust: true });
        assert_eq!(p("doctor").unwrap(), Cmd::Doctor { kill: None });
        assert_eq!(p("doctor --kill 4242").unwrap(), Cmd::Doctor { kill: Some(4242) });
        assert_eq!(p("doctor --kill x").unwrap_err(), "--kill needs a pid, got x");
        assert!(p("stop").is_err());
        assert!(p("stop a b").is_err());
        assert!(p("ls --bogus").is_err());
        assert!(p("frobnicate").is_err());
    }
    #[test]
    fn skills_subcommands_parse() {
        let k = |s: &str| {
            p(s).map(|c| match c {
                Cmd::Skills(k) => k,
                other => panic!("{other:?}"),
            })
        };
        assert_eq!(k("skills tenant").unwrap(), SkillsCmd::Tenant { repo: None });
        assert_eq!(k("skills tenant ~/app").unwrap(), SkillsCmd::Tenant { repo: Some("~/app".into()) });
        assert_eq!(k("skills learn on").unwrap(), SkillsCmd::Learn { on: true, repo: None });
        assert_eq!(k("skills learn off /r").unwrap(), SkillsCmd::Learn { on: false, repo: Some("/r".into()) });
        assert_eq!(k("skills ls").unwrap(), SkillsCmd::Ls);
        assert_eq!(
            k("skills show tests-first").unwrap(),
            SkillsCmd::Show { name: "tests-first".into(), version: None }
        );
        assert_eq!(
            k("skills show tests-first --version 2").unwrap(),
            SkillsCmd::Show { name: "tests-first".into(), version: Some(2) }
        );
        assert_eq!(k("skills proposals").unwrap(), SkillsCmd::Proposals { all: false });
        assert_eq!(k("skills proposals --all").unwrap(), SkillsCmd::Proposals { all: true });
        assert_eq!(
            k("skills accept p1 --scope org:github.com/acme --activation on_demand").unwrap(),
            SkillsCmd::Review {
                id: "p1".into(),
                accept: true,
                scope: Some("org:github.com/acme".into()),
                activation: Some("on_demand".into()),
                note: None
            }
        );
        assert_eq!(
            k("skills reject p1 --note dup").unwrap(),
            SkillsCmd::Review {
                id: "p1".into(),
                accept: false,
                scope: None,
                activation: None,
                note: Some("dup".into())
            }
        );
        assert_eq!(k("skills retire a").unwrap(), SkillsCmd::Action { name: "a".into(), op: SkillOp::Retire });
        assert_eq!(k("skills restore a").unwrap(), SkillsCmd::Action { name: "a".into(), op: SkillOp::Restore });
        assert_eq!(
            k("skills rescope a personal").unwrap(),
            SkillsCmd::Action { name: "a".into(), op: SkillOp::Rescope("personal".into()) }
        );
        assert_eq!(k("skills rollback a 3").unwrap(), SkillsCmd::Action { name: "a".into(), op: SkillOp::Rollback(3) });
        assert_eq!(k("skills mine").unwrap(), SkillsCmd::Mine { repo: None });
        assert_eq!(k("skills mine /r").unwrap(), SkillsCmd::Mine { repo: Some("/r".into()) });
        assert_eq!(
            k("skills add ./my-skill --scope repo:github.com/acme/app").unwrap(),
            SkillsCmd::Add { dir: "./my-skill".into(), scope: "repo:github.com/acme/app".into(), activation: None }
        );
        assert_eq!(
            k("skills add d --scope personal --activation always").unwrap(),
            SkillsCmd::Add { dir: "d".into(), scope: "personal".into(), activation: Some("always".into()) }
        );
        assert_eq!(k("skills lint x/SKILL.md").unwrap(), SkillsCmd::Lint { file: "x/SKILL.md".into() });
        // Errors.
        assert!(k("skills").is_err());
        assert!(k("skills forget a").unwrap_err().starts_with("unknown skills command forget ("));
        assert_eq!(k("skills learn maybe").unwrap_err(), "skills learn on|off [<repo>]");
        assert_eq!(k("skills learn").unwrap_err(), "skills learn on|off [<repo>]");
        assert_eq!(k("skills show a --version x").unwrap_err(), "--version needs a version number, got x");
        assert_eq!(k("skills accept").unwrap_err(), "missing <proposal>");
        assert_eq!(k("skills reject p1 --scope personal").unwrap_err(), "unknown option --scope");
        assert!(k("skills accept p1 --activation sometimes").unwrap_err().contains("always or on_demand"));
        assert_eq!(k("skills rescope a").unwrap_err(), "missing <scope>");
        assert_eq!(k("skills rollback a v2").unwrap_err(), "rollback needs a version number, got v2");
        assert_eq!(k("skills add d").unwrap_err(), "missing --scope personal|org:<prefix>|repo:<key>");
        assert_eq!(k("skills lint").unwrap_err(), "missing <SKILL.md>");
        assert_eq!(k("skills ls x").unwrap_err(), "ls takes no arguments");
    }
}
