//! Command-line parsing (no dependencies). Flags may come before or after positionals;
//! `--` ends flags; `--flag=value` is accepted.
use std::collections::HashMap;

use mirante_core::model::AgentKind;

/// One command's usage block (as the full help lists it) and the names it answers to.
struct Help {
    names: &'static [&'static str],
    text: &'static str,
}

const COMMANDS: &[Help] = &[
    Help {
        names: &["new"],
        text: "  new <repo> --agent claude|codex [--branch <name>] [--base <ref>] [--title <t>] [--trust] <goal...>
                         create a worktree + task and start the agent with <goal>; the branch
                         defaults to mirante/<words-of-the-goal> (-2, -3… when taken)
  new <folder> --repos <a,b,…> | --all --agent claude|codex [--branch <name>] <goal...>
                         one task across several repos of a folder: a worktree per repo named
                         (a folder name or path inside <folder>; --all: every repo in it), one
                         branch; the folder's other repos are read-only for the agent
  new <repo> --group <group> --agent claude|codex <goal...>
                         one more child of a fan-out group (branch <group>--<n>-<slug>)",
    },
    Help {
        names: &["fanout"],
        text: "  fanout <repo> --agents claude,codex[,…] [--repos <a,b,…> | --all] [--trust] <goal...>
                         one goal on several agents to compare them: a group (no worktree of
                         its own) and one child task per agent (at most 8), each in its own
                         worktree on mirante/<slug>--<n>-<agent>, started with the goal",
    },
    Help {
        names: &["group"],
        text: "  group <group>          a fan-out group: each child's status, diff and last answer
  group archive <group>  archive every child, then the group (nothing when one is live or dirty)",
    },
    Help {
        names: &["spawn"],
        text: "  spawn on <task> | spawn off <task>
                         let that task's agent propose sub-tasks (its next session gets the
                         mirante-orchestrator server), or stop it; a person's switch
  spawn ls [<task>] [--all]
                         sub-tasks agents proposed that wait for you (--all: decided ones too)
  spawn accept <id> [--goal <g>] [--title <t>] [--agent claude|codex] [--repos <a,b,…>] [--no-start]
                         create that sub-task under its parent (your edits first) and start it;
                         typed confirmation on this terminal
  spawn reject <id>      drop it (typed confirmation on this terminal)",
    },
    Help { names: &["ls", "list"], text: "  ls [--all]             tasks with status and what needs you (--all: include archived)" },
    Help { names: &["show"], text: "  show <task>            one task in detail" },
    Help { names: &["watch"], text: "  watch                  live status board (rings the bell when a task needs you)" },
    Help {
        names: &["attach"],
        text: "  attach <task>          live terminal (Claude) or event stream (Codex); detach with Ctrl-]",
    },
    Help {
        names: &["say"],
        text: "  say <task> [--queue] <message...>
                         steer now (default) or queue for the end of the running turn",
    },
    Help {
        names: &["queue"],
        text: "  queue ls <task>        the messages queued for the turn end, numbered (1 = next)
  queue edit <task> <n> <message...> | queue rm <task> <n> | queue mv <task> <n> <to>
                         change, drop or move queued message <n> (not once it is delivered)",
    },
    Help { names: &["perms"], text: "  perms                  pending permission requests" },
    Help {
        names: &["allow", "deny"],
        text: "  allow <perm> | deny <perm>
                         answer one request, once (never \"always\"); a Claude dialog is
                         answered in the terminal (offers to attach) unless the daemon runs
                         with MIRANTE_CLAUDE_INLINE_ANSWERS=1 and has the evidence",
    },
    Help { names: &["interrupt"], text: "  interrupt <task>       stop the current step (Esc / turn/interrupt)" },
    Help { names: &["stop"], text: "  stop <task>            end the agent process (resumable)" },
    Help {
        names: &["resume"],
        text: "  resume <task> [--trust] [--fresh]
                         restart the agent on its native session (context kept);
                         --fresh starts a new conversation instead (your explicit choice)",
    },
    Help {
        names: &["hibernate"],
        text: "  hibernate <task>       end an idle agent to free memory (resumable; never one waiting on you)",
    },
    Help { names: &["wake"], text: "  wake <task> [--trust]  resume a hibernated task on its native conversation" },
    Help {
        names: &["doctor"],
        text: "  doctor [--kill <pid>]  orphan processes / checkouts, memory, agent binaries; --kill asks, then kills one orphan",
    },
    Help {
        names: &["archive"],
        text: "  archive [--stop] [--cascade] <task>
                         remove the worktree (branch kept) and hide the task; --stop ends a
                         live session first; a task with open threads or sub-tasks is
                         refused unless --cascade (they are archived first, all or nothing)",
    },
    Help {
        names: &["memory"],
        text: "  memory enable|disable [<repo>]
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
                         propose a note yourself, citing committed code in the task worktree",
    },
    Help {
        names: &["skills"],
        text: "  skills tenant [<repo>]  the repo's normalized origin and client (tenant), and its learning state
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
  skills lint <SKILL.md> offline lint; exits 1 on an error finding, 3 when it could not run",
    },
    Help {
        names: &["app", "open"],
        text: "  app [--no-daemon]      open the desktop app (macOS; also: mirante open), starting the
                         daemon first unless --no-daemon. Looks for $MIRANTE_APP, then
                         /Applications/Mirante.app, ~/Applications/Mirante.app, then the
                         build in the checkout this mirante was built from",
    },
    Help {
        names: &["daemon"],
        text: "  daemon start|stop|restart|status
                         restart: stop (ends live sessions) then start, e.g. after an upgrade;
                         the new daemon resumes them unless auto-resume is off
                         (MIRANTE_AUTO_RESUME=off, or the cockpit's setting); then
                         `mirante app` opens the desktop app (the daemon has no window)
  daemon install-agent|uninstall-agent
                         (macOS) have launchd start the daemon, at login and whenever it is
                         started: its privacy prompts (Documents access) are then its own,
                         not the app's. Run in your terminal; again after changing PATH or
                         MIRANTE_* (they are written into the agent)",
    },
    Help {
        names: &["help"],
        text: "  help [<command>]       this text, or one command's (also: mirante <command> --help)
  --version, -V          the version of this CLI",
    },
];

const TERMS: &str = "\
<task> is a task id, a unique id suffix (4+ chars) or the branch name.
<repo> is a git checkout (. for the current directory's); <folder> holds several.
<scope> is personal, org:<host>/<owner>[/…] or repo:<host>/<path>.
<perm> is a permission id or a unique suffix.";

const ENV: &str = "\
environment (the daemon reads it when it starts: run `mirante daemon restart` after a change):
  MIRANTE_HOME                 where Mirante keeps its state (default ~/.mirante; keep it short)
  MIRANTE_CLAUDE_BIN, MIRANTE_CODEX_BIN
                               the real agent binaries (default: PATH, then the usual places)
  MIRANTE_AUTO_RESUME=off      do not resume sessions when the daemon starts again
  MIRANTE_CLAUDE_INLINE_ANSWERS=1
                               let the daemon type Claude answers when it has the evidence
  MIRANTE_MEM_BUDGET_MB, MIRANTE_IDLE_HIBERNATE_MIN, MIRANTE_MEMGUARD=off
                               the memory guard's per-session budget, idle hibernation, off switch
  MIRANTE_APP                  the Mirante.app `mirante app` opens (read by the CLI, not the daemon)
every variable: the Environment section of the README";

const FLAGS: &str = "\
global: --verbose before the command adds error codes to error messages";

/// The full help: every command, the terms, the environment.
pub fn usage() -> String {
    let mut out = String::from("usage: mirante <command>\n\n");
    for h in COMMANDS {
        out.push_str(h.text);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(TERMS);
    out.push_str("\n\n");
    out.push_str(FLAGS);
    out.push_str("\n\n");
    out.push_str(ENV);
    out
}

/// `name`'s usage block (`None` for an unknown command), with the terms it uses.
pub fn command_usage(name: &str) -> Option<String> {
    let h = COMMANDS.iter().find(|h| h.names.contains(&name))?;
    let mut out = String::from("usage:\n");
    for line in h.text.lines() {
        match line.strip_prefix("  ") {
            Some(rest) if !rest.starts_with(' ') && !rest.starts_with('-') => {
                out.push_str(&format!("  mirante {rest}\n"))
            }
            _ => out.push_str(&format!("{line}\n")),
        }
    }
    let terms: Vec<&str> = TERMS.lines().filter(|l| l.split(' ').next().is_some_and(|t| h.text.contains(t))).collect();
    if !terms.is_empty() {
        out.push('\n');
        out.push_str(&terms.join("\n"));
        out.push('\n');
    }
    Some(out)
}

/// The command a command line names (its first word), when it is one `mirante` knows.
pub fn command_of(args: &[String]) -> Option<&str> {
    let first = args.first()?.as_str();
    COMMANDS.iter().any(|h| h.names.contains(&first)).then_some(first)
}

/// What a usage error prints: the reason, that command's usage, and where the rest is.
pub fn usage_error(args: &[String], reason: &str) -> String {
    match command_of(args).and_then(command_usage) {
        Some(u) => format!("mirante: {reason}\n{u}run 'mirante help' for every command"),
        None => format!("mirante: {reason}\nrun 'mirante help' for every command"),
    }
}

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
    /// Install the per-user LaunchAgent `com.mirante.daemon` (macOS).
    InstallAgent,
    /// Unload and remove it.
    UninstallAgent,
}

/// Which repos of a folder a task may change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoPick {
    /// Folder names or paths inside it, in order (the first is the primary).
    Named(Vec<String>),
    /// Every git checkout in the folder.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    New {
        repo: String,
        agent: AgentKind,
        /// `None`: named from the goal (`mirante/<slug>`, `-2`, `-3`… when taken).
        branch: Option<String>,
        base: Option<String>,
        title: Option<String>,
        goal: String,
        trust: bool,
        /// A task across several repos of the folder `repo` (`--repos` / `--all`).
        repos: Option<RepoPick>,
        /// A child of this fan-out group (`--group`).
        group: Option<String>,
    },
    /// One goal on several agents: a compare group and a child per agent.
    Fanout {
        repo: String,
        agents: Vec<AgentKind>,
        goal: String,
        trust: bool,
        repos: Option<RepoPick>,
    },
    Group {
        group: String,
    },
    GroupArchive {
        group: String,
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
        /// End a live session first (with `cascade`, its children's too).
        stop: bool,
        /// Archive its open threads and sub-tasks first.
        cascade: bool,
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
    /// Open the desktop app, starting the daemon first unless `no_daemon`.
    App {
        no_daemon: bool,
    },
    Memory(MemoryCmd),
    Skills(SkillsCmd),
    Spawn(SpawnCmd),
    /// The full help, or one command's.
    Help(Option<String>),
    Version,
}

/// `mirante spawn …`: sub-tasks a task's agent proposed (a person decides each one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnCmd {
    /// Orchestration on or off for a task.
    Orchestrate {
        task: String,
        on: bool,
    },
    Ls {
        task: Option<String>,
        all: bool,
    },
    Accept {
        id: String,
        goal: Option<String>,
        title: Option<String>,
        agent: Option<AgentKind>,
        repos: Option<Vec<String>>,
        start: bool,
    },
    Reject {
        id: String,
    },
}

fn spawn(args: &[String]) -> Result<SpawnCmd, String> {
    let (op, rest) = args.split_first().ok_or("spawn on|off|ls|accept|reject")?;
    let none: &[&str] = &[];
    match op.as_str() {
        "on" | "off" => Ok(SpawnCmd::Orchestrate { task: one(&split(rest, none, none)?, "task")?, on: op == "on" }),
        "ls" | "list" => {
            let p = split(rest, none, &["--all"])?;
            let task = match p.positional.as_slice() {
                [] => None,
                [t] => Some(t.clone()),
                _ => return Err("spawn ls takes at most one <task>".into()),
            };
            Ok(SpawnCmd::Ls { task, all: !p.bools.is_empty() })
        }
        "accept" => {
            let p = split(rest, &["--goal", "--title", "--agent", "--repos"], &["--no-start"])?;
            let agent = match p.values.get("--agent").map(String::as_str) {
                Some("claude") => Some(AgentKind::Claude),
                Some("codex") => Some(AgentKind::Codex),
                Some(other) => return Err(format!("unknown agent {other} (claude or codex)")),
                None => None,
            };
            let repos = p
                .values
                .get("--repos")
                .map(|l| l.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect::<Vec<_>>());
            if repos.as_ref().is_some_and(Vec::is_empty) {
                return Err("--repos needs at least one repo name".into());
            }
            Ok(SpawnCmd::Accept {
                id: one(&p, "id")?,
                goal: p.values.get("--goal").cloned(),
                title: p.values.get("--title").cloned(),
                agent,
                repos,
                start: p.bools.is_empty(),
            })
        }
        "reject" => Ok(SpawnCmd::Reject { id: one(&split(rest, none, none)?, "id")? }),
        other => Err(format!("unknown spawn command {other} (on, off, ls, accept, reject)")),
    }
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

/// `--repos <a,b,…>` or `--all`: the repos of a folder a task changes.
fn repo_pick(p: &Parsed) -> Result<Option<RepoPick>, String> {
    let all = p.bools.iter().any(|b| b == "--all");
    match (p.values.get("--repos"), all) {
        (Some(_), true) => Err("--repos and --all: pass one".into()),
        (Some(list), false) => {
            let names: Vec<String> =
                list.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect();
            if names.is_empty() {
                return Err("--repos needs at least one repo name".into());
            }
            Ok(Some(RepoPick::Named(names)))
        }
        (None, true) => Ok(Some(RepoPick::All)),
        (None, false) => Ok(None),
    }
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

/// `--help` or `-h` among a command's arguments (before a `--`).
fn asks_help(rest: &[String]) -> bool {
    rest.iter().take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h")
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Ok(Cmd::Help(None));
    };
    let none: &[&str] = &[];
    if command_of(args).is_some() && sub != "help" && asks_help(rest) {
        return Ok(Cmd::Help(Some(sub.clone())));
    }
    match sub.as_str() {
        "fanout" => {
            let p = split(rest, &["--agents", "--repos"], &["--trust", "--all"])?;
            let (repo, goal) = task_and_text(&p, "goal").map_err(|e| e.replace("<task>", "<repo>"))?;
            let list = p.values.get("--agents").ok_or("missing --agents claude,codex")?;
            let agents = list
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(|a| match a {
                    "claude" => Ok(AgentKind::Claude),
                    "codex" => Ok(AgentKind::Codex),
                    other => Err(format!("unknown agent {other} (claude or codex)")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if agents.len() < 2 {
                return Err("--agents needs at least two (e.g. claude,codex or claude,claude)".into());
            }
            if agents.len() > 8 {
                return Err("--agents: at most 8 children in one group".into());
            }
            Ok(Cmd::Fanout { repo, agents, goal, trust: p.bools.iter().any(|b| b == "--trust"), repos: repo_pick(&p)? })
        }
        "group" => {
            let p = split(rest, none, none)?;
            match p.positional.as_slice() {
                [a, g] if a == "archive" => Ok(Cmd::GroupArchive { group: g.clone() }),
                [g] => Ok(Cmd::Group { group: g.clone() }),
                [] => Err("missing <group>".into()),
                _ => Err("usage: group <group> | group archive <group>".into()),
            }
        }
        "new" => {
            let p = split(
                rest,
                &["--agent", "--branch", "--base", "--title", "--repos", "--group"],
                &["--trust", "--all"],
            )?;
            let (repo, goal) = task_and_text(&p, "goal").map_err(|e| e.replace("<task>", "<repo>"))?;
            let agent = match p.values.get("--agent").map(String::as_str) {
                Some("claude") => AgentKind::Claude,
                Some("codex") => AgentKind::Codex,
                Some(other) => return Err(format!("unknown agent {other} (claude or codex)")),
                None => return Err("missing --agent claude|codex".into()),
            };
            let branch = p.values.get("--branch").cloned();
            let repos = repo_pick(&p)?;
            if repos.is_some() && p.values.contains_key("--base") {
                return Err(
                    "--base does not apply to --repos/--all: each repo starts from its own current branch".into()
                );
            }
            Ok(Cmd::New {
                repo,
                agent,
                branch,
                base: p.values.get("--base").cloned(),
                title: p.values.get("--title").cloned(),
                goal,
                trust: p.bools.iter().any(|b| b == "--trust"),
                repos,
                group: p.values.get("--group").cloned(),
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
        "spawn" => spawn(rest).map(Cmd::Spawn),
        "perms" => split(rest, none, none)?
            .positional
            .is_empty()
            .then_some(Cmd::Perms)
            .ok_or("perms takes no arguments".into()),
        "allow" | "deny" => Ok(Cmd::Answer { id: one(&split(rest, none, none)?, "perm")?, allow: sub == "allow" }),
        "interrupt" => Ok(Cmd::Interrupt { task: one(&split(rest, none, none)?, "task")? }),
        "stop" => Ok(Cmd::Stop { task: one(&split(rest, none, none)?, "task")? }),
        "resume" => {
            let p = split(rest, none, &["--trust", "--fresh"])?;
            let has = |f: &str| p.bools.iter().any(|b| b == f);
            Ok(Cmd::Resume { task: one(&p, "task")?, trust: has("--trust"), fresh: has("--fresh") })
        }
        "archive" => {
            let p = split(rest, none, &["--stop", "--cascade"])?;
            let has = |f: &str| p.bools.iter().any(|b| b == f);
            Ok(Cmd::Archive { task: one(&p, "task")?, stop: has("--stop"), cascade: has("--cascade") })
        }
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
        "daemon" => match one(&split(rest, none, none)?, "start|stop|restart|status|install-agent|uninstall-agent")?.as_str() {
            "start" => Ok(Cmd::Daemon(DaemonCmd::Start)),
            "stop" => Ok(Cmd::Daemon(DaemonCmd::Stop)),
            "restart" => Ok(Cmd::Daemon(DaemonCmd::Restart)),
            "status" => Ok(Cmd::Daemon(DaemonCmd::Status)),
            "install-agent" => Ok(Cmd::Daemon(DaemonCmd::InstallAgent)),
            "uninstall-agent" => Ok(Cmd::Daemon(DaemonCmd::UninstallAgent)),
            other => Err(format!("unknown daemon command {other}")),
        },
        "app" | "open" => {
            let p = split(rest, none, &["--no-daemon"])?;
            if !p.positional.is_empty() {
                return Err(format!("{sub} takes no arguments"));
            }
            Ok(Cmd::App { no_daemon: !p.bools.is_empty() })
        }
        "help" | "-h" | "--help" => match rest {
            [] => Ok(Cmd::Help(None)),
            [c] if COMMANDS.iter().any(|h| h.names.contains(&c.as_str())) => Ok(Cmd::Help(Some(c.clone()))),
            [c] => Err(format!("unknown command {c}")),
            _ => Err("help takes at most one <command>".into()),
        },
        "--version" | "-V" | "version" => Ok(Cmd::Version),
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
    fn spawn_parses_on_off_ls_accept_with_edits_and_reject() {
        assert_eq!(
            p("spawn on mirante/x").unwrap(),
            Cmd::Spawn(SpawnCmd::Orchestrate { task: "mirante/x".into(), on: true })
        );
        assert_eq!(p("spawn off x").unwrap(), Cmd::Spawn(SpawnCmd::Orchestrate { task: "x".into(), on: false }));
        assert_eq!(p("spawn ls").unwrap(), Cmd::Spawn(SpawnCmd::Ls { task: None, all: false }));
        assert_eq!(p("spawn ls t1 --all").unwrap(), Cmd::Spawn(SpawnCmd::Ls { task: Some("t1".into()), all: true }));
        assert_eq!(
            p("spawn accept 1a2b --agent codex --title Tests --repos site,bff --no-start").unwrap(),
            Cmd::Spawn(SpawnCmd::Accept {
                id: "1a2b".into(),
                goal: None,
                title: Some("Tests".into()),
                agent: Some(AgentKind::Codex),
                repos: Some(vec!["site".into(), "bff".into()]),
                start: false,
            })
        );
        assert_eq!(p("spawn reject 1a2b").unwrap(), Cmd::Spawn(SpawnCmd::Reject { id: "1a2b".into() }));
        assert!(p("spawn accept 1a2b --agent gpt").is_err());
        assert!(p("spawn budget 3").is_err(), "no budget in v1");
        assert!(p("spawn").is_err());
    }

    #[test]
    fn new_takes_flags_anywhere_and_joins_the_goal() {
        assert_eq!(
            p("new ~/app --agent codex fix the login --branch feat/login --base=release").unwrap(),
            Cmd::New {
                repo: "~/app".into(),
                agent: AgentKind::Codex,
                branch: Some("feat/login".into()),
                base: Some("release".into()),
                title: None,
                goal: "fix the login".into(),
                trust: false,
                repos: None,
                group: None,
            }
        );
        assert!(matches!(p("new . --agent claude --branch b --trust go").unwrap(), Cmd::New { trust: true, .. }));
    }

    #[test]
    fn new_reports_what_is_missing() {
        assert_eq!(p("new . --branch b go").unwrap_err(), "missing --agent claude|codex");
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
        assert_eq!(p("").unwrap(), Cmd::Help(None));
        assert_eq!(p("ls --all").unwrap(), Cmd::Ls { all: true });
        assert_eq!(p("allow ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: true });
        assert_eq!(p("deny ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: false });
        assert_eq!(p("resume feat --trust").unwrap(), Cmd::Resume { task: "feat".into(), trust: true, fresh: false });
        assert_eq!(p("resume --fresh feat").unwrap(), Cmd::Resume { task: "feat".into(), trust: false, fresh: true });
        assert_eq!(p("daemon status").unwrap(), Cmd::Daemon(DaemonCmd::Status));
        assert_eq!(p("daemon restart").unwrap(), Cmd::Daemon(DaemonCmd::Restart));
        assert_eq!(p("daemon install-agent").unwrap(), Cmd::Daemon(DaemonCmd::InstallAgent));
        assert_eq!(p("daemon uninstall-agent").unwrap(), Cmd::Daemon(DaemonCmd::UninstallAgent));
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

    #[test]
    fn new_names_the_branch_itself_when_none_is_given() {
        assert!(matches!(p("new . --agent claude fix the login").unwrap(), Cmd::New { branch: None, repos: None, .. }));
    }

    #[test]
    fn new_across_a_folder_names_its_repos_or_takes_all() {
        let named = p("new ~/gol --repos Site-B2C,bff-flight --agent claude change both").unwrap();
        let Cmd::New { repo, repos, goal, .. } = named else { panic!() };
        assert_eq!(repo, "~/gol");
        assert_eq!(repos, Some(RepoPick::Named(vec!["Site-B2C".into(), "bff-flight".into()])));
        assert_eq!(goal, "change both");
        assert!(matches!(p("new gol --all --agent codex go").unwrap(), Cmd::New { repos: Some(RepoPick::All), .. }));
        assert_eq!(p("new gol --all --repos a --agent codex go").unwrap_err(), "--repos and --all: pass one");
        assert_eq!(p("new gol --repos , --agent codex go").unwrap_err(), "--repos needs at least one repo name");
        assert!(p("new gol --all --base main --agent codex go").unwrap_err().contains("--base does not apply"));
        assert!(command_usage("new").unwrap().contains("--repos <a,b,…> | --all"));
    }

    #[test]
    fn fanout_takes_two_to_eight_agents_and_group_reads_or_archives() {
        let f = p("fanout ~/app --agents claude,codex fix the login").unwrap();
        assert_eq!(
            f,
            Cmd::Fanout {
                repo: "~/app".into(),
                agents: vec![AgentKind::Claude, AgentKind::Codex],
                goal: "fix the login".into(),
                trust: false,
                repos: None,
            }
        );
        assert!(matches!(
            p("fanout gol --all --agents claude,claude go").unwrap(),
            Cmd::Fanout { repos: Some(RepoPick::All), .. }
        ));
        assert!(p("fanout . --agents claude go").unwrap_err().contains("at least two"));
        assert!(p("fanout . --agents claude,gpt go").unwrap_err().contains("unknown agent gpt"));
        assert!(p("fanout . --agents a,b,c,d,e,f,g,h,i go").is_err());
        assert_eq!(p("group 1a2b").unwrap(), Cmd::Group { group: "1a2b".into() });
        assert_eq!(p("group archive 1a2b").unwrap(), Cmd::GroupArchive { group: "1a2b".into() });
        let Cmd::New { group, .. } = p("new . --group 1a2b --agent codex add tests").unwrap() else { panic!() };
        assert_eq!(group.as_deref(), Some("1a2b"));
        assert!(command_usage("fanout").unwrap().contains("--agents claude,codex"));
    }

    #[test]
    fn archive_can_stop_first_and_cascade() {
        let a = |stop, cascade| Cmd::Archive { task: "feat".into(), stop, cascade };
        assert_eq!(p("archive feat").unwrap(), a(false, false));
        assert_eq!(p("archive --stop feat").unwrap(), a(true, false));
        assert_eq!(p("archive --cascade feat").unwrap(), a(false, true));
        assert_eq!(p("archive feat --cascade --stop").unwrap(), a(true, true));
        assert!(command_usage("archive").unwrap().contains("--cascade"));
    }

    #[test]
    fn app_and_its_alias_open() {
        assert_eq!(p("app").unwrap(), Cmd::App { no_daemon: false });
        assert_eq!(p("open --no-daemon").unwrap(), Cmd::App { no_daemon: true });
        assert_eq!(p("app x").unwrap_err(), "app takes no arguments");
        assert!(p("app --bogus").is_err());
        assert_eq!(p("open --help").unwrap(), Cmd::Help(Some("open".into())));
        let u = command_usage("open").unwrap();
        assert!(u.starts_with("usage:\n  mirante app [--no-daemon]"), "{u}");
        assert!(usage().contains("MIRANTE_APP"));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p("help").unwrap(), Cmd::Help(None));
        assert_eq!(p("--help").unwrap(), Cmd::Help(None));
        assert_eq!(p("help new").unwrap(), Cmd::Help(Some("new".into())));
        assert_eq!(p("new --help").unwrap(), Cmd::Help(Some("new".into())));
        assert_eq!(p("memory accept -h").unwrap(), Cmd::Help(Some("memory".into())));
        // After `--` it is text, not a flag.
        assert!(matches!(p("say feat -- --help").unwrap(), Cmd::Say { .. }));
        assert_eq!(p("help frobnicate").unwrap_err(), "unknown command frobnicate");
        for v in ["--version", "-V", "version"] {
            assert_eq!(p(v).unwrap(), Cmd::Version);
        }
    }

    #[test]
    fn a_command_has_its_own_usage_and_the_full_help_has_every_one() {
        let new = command_usage("new").unwrap();
        assert!(new.starts_with("usage:\n  mirante new <repo> --agent claude|codex [--branch <name>]"), "{new}");
        assert!(new.contains("<repo> is a git checkout"), "{new}");
        assert!(!new.contains("<perm>"), "{new}");
        assert!(command_usage("deny").unwrap().contains("mirante allow <perm> | deny <perm>"));
        assert!(command_usage("frobnicate").is_none());
        let all = usage();
        for h in COMMANDS {
            assert!(all.contains(h.text), "{:?}", h.names);
        }
        assert!(all.contains("MIRANTE_HOME") && all.contains("MIRANTE_CLAUDE_BIN"), "{all}");
    }

    #[test]
    fn a_usage_error_is_short_and_points_at_help() {
        let args: Vec<String> = ["stop"].map(String::from).to_vec();
        let e = usage_error(&args, "missing <task>");
        assert_eq!(e, "mirante: missing <task>\nusage:\n  mirante stop <task>            end the agent process (resumable)\n\n<task> is a task id, a unique id suffix (4+ chars) or the branch name.\nrun 'mirante help' for every command");
        let e = usage_error(&["frobnicate".to_string()], "unknown command frobnicate");
        assert_eq!(e, "mirante: unknown command frobnicate\nrun 'mirante help' for every command");
    }
}
