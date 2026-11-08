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
  memory accept <proposal> [--title <t>] [--note <why>] | memory reject <proposal> [--note <why>]
                         decide a memory proposal; you type the verb on this terminal to
                         confirm (Mirante-launched agents are refused)
  daemon start|stop|restart|status
                         restart: stop (ends live sessions) then start, e.g. after an upgrade

<task> is a task id, a unique id suffix (4+ chars) or the branch name.
<perm> is a permission id or a unique suffix.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueCmd {
    Ls { task: String },
    Edit { task: String, n: usize, text: String },
    Rm { task: String, n: usize },
    Mv { task: String, n: usize, to: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCmd {
    /// Accept (writes the note, optionally retitled) or reject a proposal.
    Review { id: String, accept: bool, title: Option<String>, note: Option<String> },
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
    Help,
}

struct Parsed {
    positional: Vec<String>,
    values: HashMap<String, String>,
    bools: Vec<String>,
}

fn split(args: &[String], with_value: &[&str], bools: &[&str]) -> Result<Parsed, String> {
    let mut p = Parsed { positional: Vec::new(), values: HashMap::new(), bools: Vec::new() };
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

fn memory(args: &[String]) -> Result<MemoryCmd, String> {
    let (op, rest) = args.split_first().ok_or("memory accept|reject <proposal>")?;
    let accept = match op.as_str() {
        "accept" => true,
        "reject" => false,
        other => return Err(format!("unknown memory command {other} (accept, reject)")),
    };
    let flags: &[&str] = if accept { &["--title", "--note"] } else { &["--note"] };
    let p = split(rest, flags, &[])?;
    let id = one(&p, "proposal")?;
    Ok(MemoryCmd::Review { id, accept, title: p.values.get("--title").cloned(), note: p.values.get("--note").cloned() })
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
        assert_eq!(p("memory forget p1").unwrap_err(), "unknown memory command forget (accept, reject)");
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
}
