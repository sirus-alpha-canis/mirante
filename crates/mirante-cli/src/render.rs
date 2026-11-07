//! Plain-text tables. Colors only on a terminal (`color` = stdout is a tty).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, Status};
use mirante_ipc::types::{BootState, PermissionView, TaskView};

/// Last 8 characters of an id (UUID v7 prefixes are time-based and collide; suffixes do not).
pub fn short(id: &str) -> &str {
    let mut start = id.len().saturating_sub(8);
    while !id.is_char_boundary(start) {
        start += 1;
    }
    &id[start..]
}

/// `s` cut to `width` characters (with `…`) and padded.
pub fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n > width {
        let cut: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

pub fn status_label(s: Status) -> &'static str {
    s.as_str()
}

/// The approval mode Mirante runs each agent in (fixed per agent; never a bypass mode).
pub fn mode_label(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "manual",
        AgentKind::Codex => "auto-edits",
    }
}

/// What the mode means, for `mirante show`.
pub fn mode_help(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude asks before every tool that needs permission (--permission-mode manual)",
        AgentKind::Codex => {
            "edits and commands inside the worktree run without asking (sandbox workspace-write); \
             anything beyond it asks you (approvalPolicy on-request, approvalsReviewer user)"
        }
    }
}

fn paint(s: Status, text: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let code = match s {
        Status::Working => "33",
        Status::NeedsYou => "1;31",
        Status::Idle => "32",
        Status::Ended => "2",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn note(v: &TaskView) -> String {
    let state = match (v.boot, &v.detail) {
        _ if v.task.lifecycle == Lifecycle::Archiving => Some("archiving".to_string()),
        (Some(BootState::TrustPrompt), _) => Some("trust prompt: mirante resume / attach".to_string()),
        (Some(BootState::Booting), _) => Some("starting".to_string()),
        (_, Some(d)) => Some(d.clone()),
        _ => None,
    };
    state.into_iter().chain(v.usage.clone()).collect::<Vec<_>>().join(" · ")
}

pub fn tasks_table(tasks: &[TaskView], color: bool) -> String {
    if tasks.is_empty() {
        return "no tasks (mirante new <repo> --agent claude|codex --branch <name> <goal>)\n".into();
    }
    let mut out = format!(
        "{}  {}  {}  {}  {}  {}  {}  {}\n",
        fit("ID", 8),
        fit("BRANCH", 22),
        fit("AGENT", 6),
        fit("MODE", 10),
        fit("STATUS", 9),
        fit("NEEDS", 5),
        fit("Q", 2),
        "TITLE"
    );
    for v in tasks {
        let needs = if v.needs_you > 0 { v.needs_you.to_string() } else { String::new() };
        let queued = if v.queued > 0 { v.queued.to_string() } else { String::new() };
        let note = note(v);
        let title = if note.is_empty() { v.task.title.clone() } else { format!("{}  [{note}]", v.task.title) };
        out.push_str(&format!(
            "{}  {}  {}  {}  {}  {}  {}  {}\n",
            fit(short(&v.task.id), 8),
            fit(&v.task.branch, 22),
            fit(v.task.agent.as_str(), 6),
            fit(mode_label(v.task.agent), 10),
            paint(v.task.status, &fit(status_label(v.task.status), 9), color),
            fit(&needs, 5),
            fit(&queued, 2),
            title
        ));
    }
    out
}

pub fn perms_table(perms: &[PermissionView]) -> String {
    if perms.is_empty() {
        return "no pending permission requests\n".into();
    }
    let mut out = format!("{}  {}  {}  {}\n", fit("PERM", 8), fit("TASK", 22), fit("TOOL", 10), "DETAIL");
    for p in perms {
        out.push_str(&format!(
            "{}  {}  {}  {}\n",
            fit(short(&p.request.id), 8),
            fit(&p.branch, 22),
            fit(&p.request.tool, 10),
            p.request.detail.lines().next().unwrap_or("")
        ));
    }
    out.push_str("answer once: mirante allow <perm> | mirante deny <perm>\n");
    out
}

pub fn task_detail(v: &TaskView, perms: &[PermissionRequest]) -> String {
    let t = &v.task;
    let mut out = format!(
        "{}  {}\n  id:        {}\n  agent:     {}\n  status:    {}{}\n  lifecycle: {}\n  branch:    {}\n  worktree:  {}\n  repo:      {}\n  goal:      {}\n",
        short(&t.id),
        t.title,
        t.id,
        t.agent.as_str(),
        t.status.as_str(),
        if note(v).is_empty() { String::new() } else { format!(" [{}]", note(v)) },
        t.lifecycle.as_str(),
        t.branch,
        t.worktree_path,
        t.repo_path,
        t.goal.lines().next().unwrap_or("")
    );
    out.push_str(&format!("  mode:      {} ({})\n", mode_label(t.agent), mode_help(t.agent)));
    out.push_str(&format!("  live:      {}  queued: {}\n", v.live, v.queued));
    for p in perms {
        out.push_str(&format!("  needs you: {} {} {}\n", short(&p.id), p.tool, p.detail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{PermissionStatus, Task};

    fn view(status: Status, needs: usize, detail: Option<&str>) -> TaskView {
        TaskView {
            task: Task {
                id: "0190aaaa-bbbb-7ccc-8ddd-eeee12345678".into(),
                title: "Fix the login".into(),
                repo_path: "/r".into(),
                worktree_path: "/w".into(),
                branch: "feat/login".into(),
                agent: AgentKind::Claude,
                goal: "fix it".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status,
                created_at: 0,
                updated_at: 0,
            },
            live: true,
            boot: Some(BootState::Ready),
            needs_you: needs,
            queued: 0,
            detail: detail.map(String::from),
            usage: None,
        }
    }

    #[test]
    fn short_ids_are_the_unique_suffix() {
        assert_eq!(short("0190aaaa-bbbb-7ccc-8ddd-eeee12345678"), "12345678");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn fit_truncates_and_pads_by_characters() {
        assert_eq!(fit("ação", 6), "ação  ");
        assert_eq!(fit("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn table_shows_status_needs_and_detail() {
        let t = tasks_table(
            &[view(Status::NeedsYou, 2, None), view(Status::Working, 0, Some("turn done, 1 command still finishing"))],
            false,
        );
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("ID"));
        assert!(lines[1].contains("12345678") && lines[1].contains("needs_you") && lines[1].contains(" 2 "));
        assert!(lines[2].contains("[turn done, 1 command still finishing]"));
        assert!(lines[0].contains("MODE") && lines[1].contains("manual"));
        assert!(!t.contains('\x1b'));
        assert!(tasks_table(&[view(Status::Idle, 0, None)], true).contains("\x1b[32m"));
    }

    #[test]
    fn codex_rows_show_the_auto_edits_mode_and_usage() {
        let mut v = view(Status::Working, 0, Some("turn done, 1 command still finishing"));
        v.task.agent = AgentKind::Codex;
        v.usage = Some("5h 42% · 31k tokens".into());
        let t = tasks_table(&[v.clone()], false);
        assert!(t.contains("auto-edits"), "{t}");
        assert!(t.contains("[turn done, 1 command still finishing · 5h 42% · 31k tokens]"), "{t}");
        assert!(task_detail(&v, &[]).contains("mode:      auto-edits (edits and commands inside the worktree"));
        v.task.lifecycle = Lifecycle::Archiving;
        assert!(tasks_table(&[v], false).contains("[archiving · 5h 42% · 31k tokens]"));
    }

    #[test]
    fn perms_table_says_how_to_answer() {
        let p = PermissionView {
            request: PermissionRequest {
                id: "p-0000-abcdef12".into(),
                task_id: "t".into(),
                session_id: "s".into(),
                tool: "Bash".into(),
                detail: "rm -rf build\nsecond line".into(),
                status: PermissionStatus::Pending,
                created_at: 0,
                answered_at: None,
            },
            task_title: "T".into(),
            branch: "feat".into(),
        };
        let t = perms_table(&[p]);
        assert!(t.contains("abcdef12") && t.contains("rm -rf build") && !t.contains("second line"));
        assert!(t.contains("mirante allow <perm>"));
        assert_eq!(perms_table(&[]), "no pending permission requests\n");
    }
}
