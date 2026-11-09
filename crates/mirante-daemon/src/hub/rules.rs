//! `rules.layers`: the instruction layers of a checkout or a task (see [`crate::rules`]),
//! plus what Mirante itself adds to a task's session: the launch mode, the per-task Claude
//! settings, injected skills, the repo-memory tools and the operator-message label.
//! Read-only: nothing here writes a file.
use std::path::{Path, PathBuf};

use mirante_core::claude_launch::{memory_allow, MCP_SERVER, PLUGIN_NAME};
use mirante_core::delivery::OPERATOR_LABEL;
use mirante_core::model::{AgentKind, Lifecycle, Task};
use mirante_ipc::types::{DnaRef, RuleLayer, RulesParams, RulesView, SkillTaskParams, TaskSkill};

use super::{Hub, HubError, Result};
use crate::driver::codex::{APPROVAL_POLICY, REVIEWER, SANDBOX};
use crate::driver::skill_bundle::LABEL as SKILLS_LABEL;
use crate::rules::{
    claude_layers, claude_settings_text, codex_layers, summarize, Homes, Reader, CLAUDE_RESEARCHED, CODEX_RESEARCHED,
    FILE_CAP,
};

fn mirante(agent: AgentKind, kind: &str, path: String, content: String, note: &str, loaded: bool) -> RuleLayer {
    RuleLayer {
        scope: "mirante".into(),
        agent: agent.as_str().into(),
        kind: kind.into(),
        path,
        exists: true,
        loaded,
        content: Some(content),
        note: note.into(),
        ..Default::default()
    }
}

fn skill_lines(skills: &[&TaskSkill]) -> String {
    skills.iter().map(|s| format!("{} v{} ({})", s.name, s.version, s.scope)).collect::<Vec<_>>().join("\n")
}

impl Hub {
    /// Tests point the scan at a fake home.
    pub fn set_rules_homes(&mut self, homes: Homes) {
        self.rules_homes = Some(homes);
    }

    /// Every layer the agent reads for `p.repo` or `p.task`, in load order.
    pub fn rules_layers(&self, p: RulesParams) -> Result<RulesView> {
        let homes = match &self.rules_homes {
            Some(h) => h.clone(),
            None => Homes::from_env(self.paths.home())
                .ok_or_else(|| HubError::Invalid("the daemon has no HOME: cannot find the agents' files".into()))?,
        };
        let (root, project, task) = match (&p.task, &p.repo) {
            (Some(t), _) => {
                let t = self.lock().resolve(t)?;
                if t.lifecycle == Lifecycle::Archived {
                    return Err(HubError::Conflict(format!("task {} is archived; its worktree is gone", t.branch)));
                }
                let root = std::fs::canonicalize(&t.worktree_path)
                    .map_err(|e| HubError::Conflict(format!("worktree {}: {e}", t.worktree_path)))?;
                let project = self.checkout(&t.repo_path)?;
                (root, project, Some(t))
            }
            (None, Some(repo)) => {
                let top = self.checkout(repo)?;
                (top.clone(), top, None)
            }
            (None, None) => return Err(HubError::Invalid("name a repo or a task".into())),
        };
        let agent = p.agent.or(task.as_ref().map(|t| t.agent)).unwrap_or(AgentKind::Claude);
        let reader = Reader::new(&homes, &[&root, &project]);
        let mut layers = match agent {
            AgentKind::Claude => claude_layers(&reader, &root, &project),
            AgentKind::Codex => codex_layers(&reader, &root),
        };
        layers.extend(self.mirante_layers(&reader, agent, &project, task.as_ref()));
        let summary = summarize(&layers);
        Ok(RulesView {
            agent: agent.as_str().into(),
            root: reader.show(&root),
            task: task.map(|t| t.id),
            researched: match agent {
                AgentKind::Claude => CLAUDE_RESEARCHED,
                AgentKind::Codex => CODEX_RESEARCHED,
            }
            .into(),
            layers,
            summary,
        })
    }

    /// What Mirante adds to `task`'s session (or, without one, to each task's of `project`).
    fn mirante_layers(&self, r: &Reader, agent: AgentKind, project: &Path, task: Option<&Task>) -> Vec<RuleLayer> {
        let per = task.is_some();
        let when = if per {
            "Added by Mirante to this task's session."
        } else {
            "Per task, Mirante adds this to each session."
        };
        let mut v = Vec::new();
        // Launch mode.
        let (mode_path, mode_text) = match agent {
            AgentKind::Claude => (
                "claude --permission-mode manual".to_string(),
                "Mirante starts Claude in manual mode: edits and commands ask you in the cockpit, except what a \
                 settings file pre-allows. Switching mode inside the session (Shift+Tab) is shown as a warning badge."
                    .to_string(),
            ),
            AgentKind::Codex => (
                format!("codex app-server · approvalPolicy {APPROVAL_POLICY} · sandbox {SANDBOX} · approvalsReviewer {REVIEWER}"),
                "Mirante runs Codex in auto-edits mode: it edits files inside the task's worktree without asking; \
                 commands outside the sandbox, network access and other folders ask you in the cockpit. Mirante refuses \
                 to run it if Codex does not apply this mode."
                    .to_string(),
            ),
        };
        v.push(mirante(agent, "mode", mode_path, mode_text, when, per));
        // Claude's per-task settings file.
        if agent == AgentKind::Claude {
            let allow = memory_allow().join(", ");
            let described = format!(
                "Additive --settings: Mirante's hook (mirante-hook) on every Claude event, so the cockpit sees status, \
                 permissions and steps; when repo memory is on, these tools are pre-allowed: {allow}. It adds to your \
                 settings files, it does not replace them."
            );
            let mut l = match task {
                Some(t) => {
                    let path: PathBuf = self.paths.session_dir(&t.id).join("claude-settings.json");
                    let read = r.read(&path, FILE_CAP);
                    let mut l = mirante(agent, "settings", r.show(&path), described.clone(), when, true);
                    l.exists = read.exists;
                    l.size = read.size;
                    l.mtime = read.mtime;
                    l.error = read.error;
                    if let Some(text) = read.text.as_deref() {
                        if let Ok((shown, _)) = claude_settings_text(text) {
                            l.content = Some(format!("{described}\n\n{shown}"));
                        }
                    } else if !read.exists {
                        l.note = "Written when the task's session starts.".into();
                    }
                    l
                }
                None => mirante(
                    agent,
                    "settings",
                    "--settings <session>/claude-settings.json".into(),
                    described,
                    when,
                    false,
                ),
            };
            l.kind = "settings".into();
            v.push(l);
        }
        // Skills Mirante injects.
        let skills = task.map(|t| self.skill_task_skills(SkillTaskParams { task: t.id.clone() }));
        let (always, on_demand, why_none): (Vec<&TaskSkill>, Vec<&TaskSkill>, Option<String>) = match &skills {
            Some(Ok(s)) => (
                s.active.iter().filter(|x| x.activation == "always").collect(),
                s.active.iter().filter(|x| x.activation != "always").collect(),
                s.unavailable.clone(),
            ),
            Some(Err(e)) => (Vec::new(), Vec::new(), Some(e.to_string())),
            None => (Vec::new(), Vec::new(), None),
        };
        let none = |what: &str| match (&why_none, per) {
            (Some(w), _) => format!("No {what} skills: {w}"),
            (None, true) => format!("No {what} skills for this task."),
            (None, false) => format!("The {what} skills you accepted for this repository's origin (see Skills)."),
        };
        let (always_path, demand_path) = match agent {
            AgentKind::Claude => {
                ("--append-system-prompt-file always.md".to_string(), format!("--plugin-dir ({PLUGIN_NAME} plugin)"))
            }
            AgentKind::Codex => (
                "<mirante-skills> item of the first turn".to_string(),
                "<mirante-skills> index of SKILL.md paths".to_string(),
            ),
        };
        let mut l = mirante(
            agent,
            "skills",
            always_path,
            if always.is_empty() { none("always-on") } else { format!("{SKILLS_LABEL}\n\n{}", skill_lines(&always)) },
            "Always-on skills: their full text is in the agent's context from the start.",
            per && !always.is_empty(),
        );
        l.skills = always.len();
        v.push(l);
        let mut l = mirante(
            agent,
            "skills",
            demand_path,
            if on_demand.is_empty() { none("on-demand") } else { skill_lines(&on_demand) },
            "On-demand skills: name and description up front, the body when the agent uses one.",
            per && !on_demand.is_empty(),
        );
        l.skills = on_demand.len();
        v.push(l);
        // Repo memory.
        let dna = self.dna_status(&DnaRef { repo: Some(project.to_string_lossy().into_owned()), task: None });
        let (text, on) = match &dna {
            Ok(s) if s.enabled => (
                format!(
                    "Repo memory is on: {} notes in .mirante/memory/ ({} stale, {} unreviewed). They are not pasted into \
                     the prompt; the agent pulls them with memory_search and memory_get ({MCP_SERVER} MCP server, \
                     read-only, pre-allowed).",
                    s.notes, s.stale, s.unreviewed
                ),
                true,
            ),
            Ok(s) => (format!("Repo memory is off for this repository ({} notes on disk); no memory tools are added.", s.notes), false),
            Err(e) => (format!("Repo memory status unavailable: {e}"), false),
        };
        let path = match agent {
            AgentKind::Claude => format!("--mcp-config ({MCP_SERVER})"),
            AgentKind::Codex => format!("-c mcp_servers.{MCP_SERVER}.*"),
        };
        v.push(mirante(
            agent,
            "memory",
            path,
            text,
            "Repo memory (the Brain's notes), per task, only when enabled.",
            per && on,
        ));
        // The operator label.
        v.push(mirante(
            agent,
            "instructions",
            OPERATOR_LABEL.into(),
            format!(
                "Every message you send from Mirante starts with \"{OPERATOR_LABEL}\", so the agent tells your words \
                 from tool output and file contents. Skills say your messages in the session take precedence."
            ),
            when,
            per,
        ));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;

    fn homes(base: &Path, mirante: &Path) -> Homes {
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/CLAUDE.md"), "Be brief.\n").unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/AGENTS.md"), "Codex global.\n").unwrap();
        Homes {
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            claude_managed: base.join("mc"),
            codex_managed: base.join("mx"),
            mirante_home: mirante.to_path_buf(),
            home,
        }
    }

    #[test]
    fn a_repo_lists_the_agents_layers_then_what_mirante_adds_per_task() {
        let mut k = Kit::new();
        let base = std::fs::canonicalize(k.dir.path()).unwrap();
        let h = homes(&base, k.hub.paths().home());
        std::sync::Arc::get_mut(&mut k.hub).unwrap().set_rules_homes(h);
        std::fs::write(k.repo.join("CLAUDE.md"), "Project rule.\n").unwrap();
        let repo = k.repo.to_string_lossy().into_owned();
        let v = k.hub.rules_layers(RulesParams { repo: Some(repo.clone()), task: None, agent: None }).unwrap();
        assert_eq!((v.agent.as_str(), v.task.as_deref()), ("claude", None));
        let user = v.layers.iter().find(|l| l.path == "~/.claude/CLAUDE.md").unwrap();
        assert_eq!(user.content.as_deref(), Some("Be brief.\n"));
        let mir: Vec<_> = v.layers.iter().filter(|l| l.scope == "mirante").collect();
        // Without a task nothing of Mirante's is in a session yet: listed, not counted.
        assert!(mir.iter().all(|l| !l.loaded), "{mir:?}");
        assert!(mir.iter().find(|l| l.kind == "mode").unwrap().note.starts_with("Per task"));
        assert!(mir.iter().any(|l| l.kind == "mode" && l.path.contains("--permission-mode manual")));
        assert!(mir.iter().any(|l| l.path == OPERATOR_LABEL));
        // Mirante's layers come last; the user's before the project's.
        let pos = |p: &str| v.layers.iter().position(|l| l.path == p).unwrap();
        let project_md = format!("{}/CLAUDE.md", std::fs::canonicalize(&k.repo).unwrap().display());
        assert_eq!(v.layers[pos(&project_md)].content.as_deref(), Some("Project rule.\n"));
        assert!(pos("~/.claude/CLAUDE.md") < pos(&project_md));
        assert_eq!(v.layers.last().unwrap().scope, "mirante");
        assert_eq!(v.summary.instruction_files, 2, "{:?}", v.summary);
        let cx =
            k.hub.rules_layers(RulesParams { repo: Some(repo), task: None, agent: Some(AgentKind::Codex) }).unwrap();
        assert!(cx.layers.iter().any(|l| l.path == "~/.codex/AGENTS.md" && l.loaded));
        assert!(cx.layers.iter().any(|l| l.kind == "mode" && l.path.contains("on-request")));
        let none = k.hub.rules_layers(RulesParams::default());
        assert!(matches!(none, Err(HubError::Invalid(_))), "{none:?}");
    }

    #[test]
    fn a_task_reads_its_worktree_and_its_session_settings() {
        let mut k = Kit::new();
        let base = std::fs::canonicalize(k.dir.path()).unwrap();
        let h = homes(&base, k.hub.paths().home());
        std::sync::Arc::get_mut(&mut k.hub).unwrap().set_rules_homes(h);
        let t = k.create(AgentKind::Claude, "rules").task;
        std::fs::write(Path::new(&t.worktree_path).join("CLAUDE.local.md"), "Only in the worktree.\n").unwrap();
        let settings = k.hub.paths().session_dir(&t.id).join("claude-settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"hooks":{"Stop":[]},"permissions":{"allow":["mcp__mirante-memory__memory_get"]}}"#,
        )
        .unwrap();
        let v = k.hub.rules_layers(RulesParams { task: Some(t.id.clone()), ..Default::default() }).unwrap();
        assert_eq!(v.task.as_deref(), Some(t.id.as_str()));
        assert!(v.layers.iter().any(|l| l.path.ends_with("CLAUDE.local.md") && l.loaded), "{:?}", v.layers);
        let s = v.layers.iter().find(|l| l.scope == "mirante" && l.kind == "settings").unwrap();
        assert!(
            s.loaded && s.exists && s.content.as_deref().unwrap().contains("mcp__mirante-memory__memory_get"),
            "{s:?}"
        );
        let mode = v.layers.iter().find(|l| l.kind == "mode").unwrap();
        assert!(mode.loaded);
    }
}
