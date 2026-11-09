# Instruction layers: what each agent session reads

What Claude Code and Codex load as instructions, settings, skills and memory when a session
starts, and what Mirante adds per task. This is what `rules.layers` (the Rules view, ⌘R, in the
app) lists, in this order. Researched on 2026-11-09.

**Versions checked:** Claude Code **2.1.285** (Homebrew cask), Codex CLI **0.167.0** (npm
`@openai/codex`, darwin-arm64), on macOS (Darwin 25.5).

**How it was checked:** the public docs (below), then the file and setting names were
confirmed in the installed binaries (`strings`): Claude Code contains `.claude/rules`,
`CLAUDE.local.md`, `managed-settings.json`, `managed-settings.d`, `claudeMdExcludes`,
`autoMemoryEnabled`, `autoMemoryDirectory`, `MEMORY.md`, `output-styles`; Codex contains
`AGENTS.override.md`, `project_doc_max_bytes`, `project_doc_fallback_filenames`,
`project_root_markers`, `developer_instructions`, `model_instructions_file`,
`.agents/skills`, `requirements.toml`, `managed_config.toml`, and **not**
`experimental_instructions_file` (renamed `model_instructions_file`).

Rules change between versions: when an agent updates, recheck this page and
`crates/mirante-daemon/src/rules.rs` (`CLAUDE_RESEARCHED`, `CODEX_RESEARCHED`).

## Claude Code 2.1.285

Sources: <https://code.claude.com/docs/en/memory>,
<https://code.claude.com/docs/en/settings>.

### Instructions, in load order (broadest first; later text is read later)

| Scope | Path | When |
| --- | --- | --- |
| Managed | macOS `/Library/Application Support/ClaudeCode/CLAUDE.md` (Linux/WSL `/etc/claude-code/CLAUDE.md`, Windows `C:\Program Files\ClaudeCode\CLAUDE.md`) | Every session, first; `claudeMdExcludes` cannot exclude it. |
| User | `~/.claude/CLAUDE.md` (`$CLAUDE_CONFIG_DIR` moves `~/.claude`) | Every project; after managed. |
| User | `~/.claude/rules/**/*.md` | Every project, before project rules. A rule with `paths:` frontmatter loads only when Claude touches a matching file. |
| Project (ancestors) | `CLAUDE.md`, `CLAUDE.local.md` in every folder above the working folder | At launch, ordered from `/` down. |
| Project | `./CLAUDE.md` or `./.claude/CLAUDE.md` | At launch. |
| Project | `./.claude/rules/**/*.md` | At launch (path-scoped rules on demand). Symlinks pointing outside the project wait for an approval of external imports. |
| Project | `./AGENTS.md`, `./.claude/AGENTS.md` | Since 2.1.277, **only when there is no** `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md` in the working folder or above (default `claude-md-or-agents-md`; `pluginConfigs."cc-plugin-agents-md@builtin".options.instructionFiles` changes it). User and managed `CLAUDE.md` do not count. |
| Local | `./CLAUDE.local.md` | At launch, after `CLAUDE.md` in the same folder (read last at that level). |
| Nested | `CLAUDE.md` / `CLAUDE.local.md` in subfolders | On demand: once Claude reads, writes or edits a file in that subfolder. |

- Files are **concatenated, not overridden**: if two contradict, Claude may follow either.
- **`@path` imports**: relative to the importing file (not the working folder), absolute
  and `~/` paths allowed; recursive, **at most four hops**; not inside code spans or fenced
  code blocks. Imports outside the project ask for a one-time approval (user-scope files
  are trusted). Block-level HTML comments are stripped before injection.
- **Auto memory**: `~/.claude/projects/<project>/memory/MEMORY.md`, where `<project>` comes
  from the git repository (all worktrees and subfolders share it); in practice the path
  with every non-alphanumeric character turned into `-` (e.g.
  `-Users-me-Documents-Develop-app`). The **first 200 lines or 25 KB** load in every
  session; topic files beside it are read on demand. `autoMemoryEnabled: false` turns it
  off; `autoMemoryDirectory` moves it (any settings scope).
- **Output styles**: `~/.claude/output-styles/` and `.claude/output-styles/`; one is used
  only when a settings file sets `outputStyle`, and it changes the system prompt.
- `--append-system-prompt` / `--append-system-prompt-file` add text to the system prompt
  (Mirante uses the file form for always-on skills).

### Settings that shape behaviour

Precedence, highest first: **managed** (`managed-settings.json` in the managed folder, plus
`managed-settings.d/*.json` drop-ins, MDM, or server-managed) > **command line**
(`--settings`) > **local** (`.claude/settings.local.json`; in a worktree, the main
checkout's root) > **project** (`.claude/settings.json`) > **user**
(`~/.claude/settings.json`). Lists (e.g. `permissions.allow`) merge across files.
Relevant keys: `permissions` (allow / ask / deny, `defaultMode`), `hooks`, `env`,
`enabledPlugins`, `outputStyle`, `claudeMdExcludes`, `autoMemoryEnabled`,
`autoMemoryDirectory`, `model`, `apiKeyHelper`, `pluginConfigs`, `sandbox`. A project
file's `permissions.allow`, `env` and marketplaces wait for workspace trust;
`defaultMode: auto | bypassPermissions` is ignored in project and local files.

### Skills and plugins

- `~/.claude/skills/<name>/SKILL.md` (user), `.claude/skills/<name>/SKILL.md` (project),
  and the skills of enabled plugins (`enabledPlugins`; installed under
  `~/.claude/plugins/`). Name and description load at launch; the body when used.
- Plugins can also bring commands, agents, hooks and MCP servers.

## Codex CLI 0.167.0

Sources: <https://developers.openai.com/codex/guides/agents-md> (now
<https://learn.chatgpt.com/docs/agent-configuration/agents-md>),
<https://developers.openai.com/codex/skills> (now
<https://learn.chatgpt.com/docs/build-skills>), and the binary's strings.

### Instructions

1. **Global**, in `$CODEX_HOME` (default `~/.codex`): `AGENTS.override.md` if present and
   not empty, else `AGENTS.md`. Only the first non-empty one.
2. **Project**: from the git root (the first folder up with a `project_root_markers` entry,
   default `.git`; else the working folder) **down to** the working folder; in each folder
   the first of `AGENTS.override.md`, `AGENTS.md`, then `project_doc_fallback_filenames`.
   **At most one file per folder.** Concatenated root first, joined with blank lines;
   empty files skipped; stops once the total reaches **`project_doc_max_bytes` (32 KiB by
   default)**. Folders **below** the working folder are not read by Codex itself.
3. **`config.toml`**: `developer_instructions` (text added as a developer message) and
   `model_instructions_file` (replaces the built-in base instructions; formerly
   `experimental_instructions_file`, which 0.167 no longer reads). Profiles
   (`[profiles.<name>]`, `profile = …`) and `-c key=value` override the file.

### Config layers

`/etc/codex/requirements.toml` (admin requirements that cannot be lifted), the legacy
`/etc/codex/managed_config.toml` (and its MDM form), `/etc/codex/config.toml` (system),
`~/.codex/config.toml` (user), `.codex/config.toml` in a **trusted** project, then
session flags (`-c`). Relevant keys: `approval_policy`, `sandbox_mode`,
`[sandbox_workspace_write]`, `[mcp_servers.*]`, `[features]`, `[projects."<path>"]
trust_level`, `project_doc_*`, `developer_instructions`, `model_instructions_file`.

### Skills

`.agents/skills` in every folder from the working folder up to the repo root; user
`$HOME/.agents/skills`; admin `/etc/codex/skills`; bundled system skills (installed under
`~/.codex/skills/.system`). Name and description at launch (at most 2% of the context, or
8,000 characters); the full `SKILL.md` when the agent picks a skill.

## What Mirante adds per task

From `crates/mirante-daemon/src/driver/real.rs`, `driver/codex.rs`,
`driver/skill_bundle.rs`, `mirante-core/src/claude_launch.rs`, `delivery.rs`:

- **Claude**: `--permission-mode manual`; an additive `--settings
  ~/.mirante/sessions/<task>/claude-settings.json` (Mirante's hook on every event; when repo
  memory is on, `mcp__mirante-memory__memory_search` and `memory_get` pre-allowed);
  always-on skills with `--append-system-prompt-file always.md`; on-demand skills as the
  `mirante-skills` plugin (`--plugin-dir`); repo memory through `--mcp-config`.
- **Codex**: `codex app-server` with `approvalPolicy on-request`, sandbox
  `workspace-write`, `approvalsReviewer user` (refused if not applied); repo memory and
  skills MCP servers through `-c mcp_servers.*` overrides; skills as the first turn's
  `<mirante-skills>` item.
- **Both**: every message a person sends starts with
  `[Mirante · message from the human operator]`; the skills label says the operator's
  messages take precedence. Repo memory notes are not pasted into the prompt: the agent
  pulls them with `memory_search` / `memory_get`.
- Mirante never writes to `~/.claude` or `~/.codex`.

## How Mirante reads them (`rules.layers`)

A person only (agents and unverifiable callers are refused). The daemon picks every path
from the lists above; the caller names only a checkout or a task. A path is read when it
resolves (symlinks followed, dotfile repos are common) inside the home folder, the project,
the managed folders or Mirante's home; only regular files, opened `O_NONBLOCK | O_NOFOLLOW`
on the resolved path (never a FIFO or device); at most 64 KiB each (Claude's `MEMORY.md` is
shown as Claude loads it: 200 lines or 25 KB). Files that usually hold credentials (ssh,
cloud and agent auth files, `.env`, keys, `~/.claude.json`, `~/.codex/auth.json`) are never
shown, even when an instruction file imports them. Everything shown goes through the
redactor; settings show their relevant keys, every `env` value masked.
