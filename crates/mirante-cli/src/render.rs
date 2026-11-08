//! Plain-text tables. Colors only on a terminal (`color` = stdout is a tty).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, Status};
use mirante_ipc::types::{BootState, DoctorReport, MemView, PermissionView, ProposalView, SystemView, TaskView};

/// A proposal as the review prompt shows it: status, title (or the person's new title),
/// kind, origin, redactions, cited code and body. Control characters are stripped (the text
/// came from an agent and goes to a terminal).
pub fn proposal(p: &ProposalView, new_title: Option<&str>) -> String {
    use mirante_core::dna::untrusted::line;
    let mut out = format!("Proposal {} ({}, {} from {})\n", p.id, p.status, p.kind, p.origin);
    out.push_str(&format!("  title: {}\n", line(&p.title, 200)));
    if let Some(t) = new_title {
        out.push_str(&format!("  new title: {}\n", line(t, 200)));
    }
    if p.redactions > 0 {
        out.push_str(&format!("  redactions: {} secret(s) masked\n", p.redactions));
    }
    for a in &p.anchors {
        let symbol = a.symbol.as_deref().map(|s| format!(" ({})", line(s, 100))).unwrap_or_default();
        out.push_str(&format!(
            "  cites: {}:{}-{}{symbol} @ {}\n",
            line(&a.path, 300),
            a.start,
            a.end,
            &a.commit[..a.commit.len().min(7)]
        ));
    }
    out.push('\n');
    for l in p.body.lines().take(40) {
        out.push_str(&format!("  | {}\n", line(l, 300)));
    }
    if p.body.lines().count() > 40 {
        out.push_str("  | …\n");
    }
    out
}

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

/// The MODE cell: the mode plus, for Codex, the effective sandbox the agent reported
/// (`auto-edits(net:off)`), so a widened sandbox is visible at a glance.
pub fn mode_cell(v: &TaskView) -> String {
    match &v.sandbox {
        Some(s) => format!("{}({s})", mode_label(v.task.agent)),
        None => mode_label(v.task.agent).to_string(),
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
        _ if v.hibernated => Some("hibernated: mirante wake".to_string()),
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
    let mode_w = tasks.iter().map(|v| mode_cell(v).chars().count()).max().unwrap_or(0).max(10);
    let mut out = format!(
        "{}  {}  {}  {}  {}  {}  {}  {}  {}\n",
        fit("ID", 8),
        fit("BRANCH", 22),
        fit("AGENT", 6),
        fit("MODE", mode_w),
        fit("STATUS", 9),
        fit("MEM", 11),
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
            "{}  {}  {}  {}  {}  {}  {}  {}  {}\n",
            fit(short(&v.task.id), 8),
            fit(&v.task.branch, 22),
            fit(v.task.agent.as_str(), 6),
            fit(&mode_cell(v), mode_w),
            paint(v.task.status, &fit(status_label(v.task.status), 9), color),
            fit(&v.memory.as_ref().map(mem_cell).unwrap_or_default(), 11),
            fit(&needs, 5),
            fit(&queued, 2),
            title
        ));
    }
    out
}

/// Bytes as "1.5G" / "512M".
pub fn bytes(n: u64) -> String {
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        format!("{:.1}G", n as f64 / GIB as f64)
    } else {
        format!("{}M", n >> 20)
    }
}

/// "1.2G/4.0G", with "!" past the soft share and "!!" past the budget.
pub fn mem_cell(m: &MemView) -> String {
    let mark = match m.state.as_str() {
        "soft" => "!",
        "hard" => "!!",
        _ => "",
    };
    format!("{}/{}{mark}", bytes(m.bytes), bytes(m.budget))
}

/// The `watch` header line about the machine's memory.
pub fn memory_line(s: &SystemView) -> String {
    let paused = if s.paused { " · new sessions and queued messages paused" } else { "" };
    format!(
        "memory: {} · free {} of {} · swap {}/{}{paused}",
        s.level,
        bytes(s.free),
        bytes(s.total),
        bytes(s.swap_used),
        bytes(s.swap_total)
    )
}

pub fn doctor_report(d: &DoctorReport) -> String {
    let mut out = String::new();
    match &d.memory {
        Some(m) => out.push_str(&format!("{}\n", memory_line(m))),
        None => out.push_str("memory: not sampled yet\n"),
    }
    if !d.agents.is_empty() {
        out.push_str("agents (as the daemon resolves them):\n");
        for a in &d.agents {
            match (&a.path, &a.error) {
                (Some(p), _) => {
                    let from = a.source.as_deref().map(|s| format!("  (from {s})")).unwrap_or_default();
                    out.push_str(&format!("  {}  {p}{from}\n", fit(&a.agent, 6)));
                }
                (None, e) => {
                    out.push_str(&format!("  {}  not found: {}\n", fit(&a.agent, 6), e.as_deref().unwrap_or("?")));
                }
            }
        }
    }
    if d.orphan_processes.is_empty() {
        out.push_str("orphan processes: none\n");
    } else {
        out.push_str("orphan processes (parent gone, inside Mirante's worktrees):\n");
        for o in &d.orphan_processes {
            let cwd = o.cwd.as_deref().unwrap_or("?");
            out.push_str(&format!("  pid {}  cwd {}  {}\n", o.pid, cwd, o.argv));
        }
        out.push_str("kill one (asks first): mirante doctor --kill <pid>\n");
    }
    if d.orphan_worktrees.is_empty() {
        out.push_str("orphan checkouts: none\n");
    } else {
        out.push_str("orphan checkouts (no task owns them; Mirante never deletes them):\n");
        for w in &d.orphan_worktrees {
            out.push_str(&format!("  {w}\n"));
        }
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
    if let Some(sb) = &v.sandbox {
        out.push_str(&format!("  sandbox:   {sb} (effective, as the agent reported it)\n"));
    }
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

    #[test]
    fn a_proposal_prompt_shows_what_is_decided_without_terminal_controls() {
        let p: ProposalView = serde_json::from_value(serde_json::json!({
            "id": "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55", "repo": "/r", "task_id": null, "origin": "agent",
            "title": "Use <b>\u{1b}[2J", "kind": "gotcha", "body": "line one\n\u{1b}]0;pwned\u{7}two\n",
            "anchors": [{"path": "src/a.rs", "start": 1, "end": 3,
                "commit": "50ea8cc255941e739b13cdbe2c937abca0319834", "fingerprint": "0123456789abcdef"}],
            "redactions": 2, "status": "pending", "slug": null, "decision_note": "",
            "created_at": 1, "decided_at": null
        }))
        .unwrap();
        let s = proposal(&p, Some("Better"));
        assert!(!s.contains('\u{1b}') && !s.contains('\u{7}'), "{s:?}");
        assert!(s.contains("title: Use <b>[2J") && s.contains("new title: Better"), "{s}");
        assert!(s.contains("redactions: 2") && s.contains("cites: src/a.rs:1-3 @ 50ea8cc"), "{s}");
        assert!(s.contains("  | line one\n  | ]0;pwnedtwo\n"), "{s}");
    }

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
            sandbox: None,
            memory: None,
            hibernated: false,
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
    fn codex_rows_show_the_effective_sandbox_in_the_mode() {
        let mut v = view(Status::Idle, 0, None);
        v.task.agent = AgentKind::Codex;
        v.sandbox = Some("net:off".into());
        let t = tasks_table(&[v.clone()], false);
        assert!(t.lines().nth(1).unwrap().contains("auto-edits(net:off)"), "{t}");
        v.sandbox = Some("net:on, +1 granted root".into());
        assert!(task_detail(&v, &[]).contains("sandbox:   net:on, +1 granted root"), "{}", task_detail(&v, &[]));
        // Columns stay aligned whatever the mode text.
        let rows = tasks_table(&[v, view(Status::Idle, 0, None)], false);
        let starts: Vec<usize> = rows.lines().map(|l| l.find("idle").unwrap_or(0)).skip(1).collect();
        assert_eq!(starts[0], starts[1], "{rows}");
    }

    #[test]
    fn memory_shows_per_task_in_watch_and_in_doctor() {
        let mut v = view(Status::Idle, 0, None);
        v.memory = Some(MemView { bytes: 3 << 30, peak: 3 << 30, budget: 4 << 30, state: "soft".into() });
        let t = tasks_table(&[v.clone()], false);
        assert!(t.lines().next().unwrap().contains("MEM") && t.contains("3.0G/4.0G!"), "{t}");
        v.memory = None;
        v.hibernated = true;
        v.live = false;
        assert!(tasks_table(&[v], false).contains("[hibernated: mirante wake]"));
        let s = SystemView {
            level: "elevated".into(),
            total: 16 << 30,
            free: 512 << 20,
            swap_used: 9 << 30,
            swap_total: 10 << 30,
            paused: true,
        };
        assert_eq!(
            memory_line(&s),
            "memory: elevated · free 512M of 16.0G · swap 9.0G/10.0G · new sessions and queued messages paused"
        );
        let d = DoctorReport {
            orphan_processes: vec![mirante_ipc::types::OrphanView {
                pid: 4242,
                start: 1,
                cwd: Some("/w/app/feat".into()),
                argv: "node server.js".into(),
            }],
            orphan_worktrees: vec![],
            memory: Some(s),
            agents: vec![],
        };
        let r = doctor_report(&d);
        assert!(r.contains("pid 4242  cwd /w/app/feat  node server.js") && r.contains("mirante doctor --kill <pid>"));
        assert!(r.contains("orphan checkouts: none"));
    }

    #[test]
    fn doctor_says_which_agent_binaries_the_daemon_found_and_where() {
        use mirante_ipc::types::AgentBinView;
        let d = DoctorReport {
            orphan_processes: vec![],
            orphan_worktrees: vec![],
            memory: None,
            agents: vec![
                AgentBinView {
                    agent: "claude".into(),
                    path: Some("/Users/me/.local/bin/claude".into()),
                    source: Some("well-known directory".into()),
                    error: None,
                },
                AgentBinView {
                    agent: "codex".into(),
                    path: None,
                    source: None,
                    error: Some("no codex in PATH=/usr/bin".into()),
                },
            ],
        };
        let r = doctor_report(&d);
        assert!(r.contains("agents (as the daemon resolves them):\n"), "{r}");
        assert!(r.contains("  claude  /Users/me/.local/bin/claude  (from well-known directory)\n"), "{r}");
        assert!(r.contains("  codex   not found: no codex in PATH=/usr/bin\n"), "{r}");
        let old = DoctorReport { agents: vec![], ..d };
        assert!(!doctor_report(&old).contains("agents"));
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
