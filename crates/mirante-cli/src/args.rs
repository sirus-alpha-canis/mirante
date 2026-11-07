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
  perms                  pending permission requests
  allow <perm> | deny <perm>
                         answer one request, once (never \"always\")
  interrupt <task>       stop the current step (Esc / turn/interrupt)
  stop <task>            end the agent process (resumable)
  resume <task> [--trust] [--fresh]
                         restart the agent on its native session (context kept);
                         --fresh starts a new conversation instead (your explicit choice)
  archive <task>         remove the worktree (branch kept) and hide the task
  daemon start|stop|restart|status
                         restart: stop (ends live sessions) then start, e.g. after an upgrade

<task> is a task id, a unique id suffix (4+ chars) or the branch name.
<perm> is a permission id or a unique suffix.";

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
    Daemon(DaemonCmd),
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
        assert!(p("stop").is_err());
        assert!(p("stop a b").is_err());
        assert!(p("ls --bogus").is_err());
        assert!(p("frobnicate").is_err());
    }
}
