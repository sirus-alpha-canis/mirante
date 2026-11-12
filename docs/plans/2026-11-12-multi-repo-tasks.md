# Multi-repo tasks (workspace tasks)

**Ask (founder):** "I need to be able to select the GOL folder, because I want to work on all
the projects inside it, and tell the agent e.g. 'here are Site-B2C etc. that you need to
change'." Until now a task targets one git checkout (one worktree per task). Projects added
from a folder show as a group ("gol (25)") in the rail and the repository picker, but a group
could not be a dispatch target.

## Decisions

1. **A group (the folder) is a target; the person picks which repos the task may change.**
   `@claude gol <goal>` or picking "gol (25)" in the Repository chip targets the group. The
   composer then shows a checklist of the group's repos. Default: nothing checked, except the
   repos whose names appear in the goal ("change Site-B2C and bff-flight" pre-checks both),
   plus "Select all". Nothing checked: the send is refused with a reason.
2. **Selected repos get worktrees; the other repos of the group are read-only.** Each selected
   repo gets its own worktree, all on one branch name. The agent is told where the others are
   (their checkout paths) and that they are for reading only. Why not give every repo a
   worktree: 25 checkouts per task cost disk and time, and the diff/PR surface should be
   what the person meant to change. Why not hide them: cross-repo work ("make bff-flight
   match what Site-B2C calls") needs to read the neighbours.
   - Claude: the group folder is pre-allowed for reading with a permission rule
     (`Read(//<group>/**)`, which Claude also applies to Grep and Glob). Writes outside the
     workspace ask the person (Mirante starts Claude in manual mode), as before.
   - Codex: `workspace-write` already reads everywhere and writes only under its cwd (the
     workspace). Nothing to add.
3. **Layout.** `~/.mirante/worktrees/_workspaces/<slug>/<repo-name>/`, one worktree per
   selected repo, `<slug>` = the branch without a leading `mirante/`, `/` turned into `-`.
   The agent's cwd is the workspace folder `_workspaces/<slug>/`, which is not a git
   repository, so files written there are never committed. Mirante writes three read-only
   files there: `MIRANTE_WORKSPACE.md` (each repo: name, worktree path, branch, base, its
   main checkout; and the read-only repos' paths), `AGENTS.md` (the same text: Codex reads it
   from its cwd) and `CLAUDE.md` (`@MIRANTE_WORKSPACE.md`: Claude reads it from its cwd).
   Files survive resumes, unlike first-turn context.
4. **One branch name for every repo; the collision rule per repo.** `task.create` with a base
   (the clients send `HEAD`) refuses a branch that exists in *any* selected repo ("branch
   already exists in <repo>"), so the CLI's and the cockpit's existing rename loop
   (`-2`, `-3`…) moves on to a name free everywhere. Without a base an existing branch is
   checked out as is, in each repo (the single-repo rule). A base other than `HEAD` is
   refused for a workspace task (each repo starts from its own current branch).
5. **Primary repo = the first selected.** `tasks.repo_path` is the primary's main checkout and
   `tasks.worktree_path` the primary's worktree, so everything that already reads them (DNA
   memory, rules, skills origin, mining, `repo.files`, `mirante ls` grouping) keeps working
   unchanged on the primary. The agent's cwd is `tasks.workspace_path` when set.
6. **Tenants.** The task's skills origin is fixed from the primary. If the selected repos
   resolve to different tenants (normalized origin owners), no origin is recorded (the
   existing fail-closed path: no skills), the repo memory server is not given, and a
   `workspace_tenants` event says so ("skills and repo memory are off: the repos belong to
   gol-sa, acme"). Same tenant: skills and memory follow the primary.
7. **No new RPC methods.** `task.create` gains `repos: [paths]` (the group folder goes in the
   existing `repo`); `task.diff` gains `repos: [sections]`. `access.rs`, the bridge allowlist
   and `security.test.ts` stay as they are.

## Data model

Migration `0010_workspace_tasks.sql`:

- `tasks.workspace_path TEXT NULL` and `tasks.group_path TEXT NULL` (both set for a workspace
  task, never for a single-repo one).
- `task_repos (task_id, idx, name, repo_path, worktree_path, branch, created_branch, head)`,
  primary key `(task_id, idx)`, unique `(task_id, name)`. Rows are fixed at creation
  (update trigger aborts) and kept after archive (like the branch).

`Task` gains `workspace_path` and `group_path` (serde default: older clients ignore them).
`TaskView` gains `repos: [{name, repo_path, worktree_path, branch}]` (absent when empty).

## Wire

```jsonc
// task.create — single repo (unchanged)
{ "repo": "/src/shop", "agent": "claude", "branch": "mirante/x", "base": "HEAD", "goal": "…" }
// task.create — workspace task
{ "repo": "/src/gol", "repos": ["/src/gol/Site-B2C", "/src/gol/bff-flight"],
  "agent": "claude", "branch": "mirante/x", "base": "HEAD", "goal": "…" }
// task.diff result for a workspace task: the primary's diff at the top level (older
// clients see it), and one section per repo
{ "base": "main", …, "repos": [ { "name": "Site-B2C", "repo": "/src/gol/Site-B2C",
  "base": "main", "files": [...], "compare_url": "…", … }, … ] }
```

An older daemon given `repos` ignores the field and refuses the group folder as "not a git
repository": a clear failure, nothing created.

## Validation (daemon, before anything is made)

- `repo` (the group): absolute, canonical, a directory, not inside Mirante's worktree root.
- each of `repos`: absolute; canonicalized (a symlink resolves to its target, which must
  still be inside the canonical group: no symlink escape); inside the group, not the group
  itself; the top level of a git checkout (`rev-parse --show-toplevel` equals it); with a
  commit; not inside the worktree root; listed once; at most 50.
- names (folder names) are unique among the selected; a clash gets `-2`.
- every git call through the hardened `Git` runner.

## Lifecycle

- **Create:** validate all → check the agent → per repo: worktree (rolled back on any
  failure: every worktree made so far is discarded, branches only if made and unmoved, the
  workspace folder removed) → write the workspace files → task row + repo rows in one
  transaction (rolled back the same way if it fails).
- **Diff:** one section per repo (each patch capped at `PATCH_CAP / n`, at least 64 KiB).
- **Archive / discard:** every worktree's state is checked first; any dirty one refuses the
  whole archive, listing each dirty repo and its changes. Extra files the agent left in the
  workspace folder itself (outside every repo) refuse it too. Then each worktree is removed
  (branches kept), then the workspace folder. Recovery finishes an interrupted archive the
  same way; the orphan report counts every repo worktree as owned.
- **Status / hooks / sessions:** unchanged: one session per task.

## Clients

- **CLI:** `mirante new <folder> --repos a,b "goal"` (names or paths under the folder) or
  `--all`; `mirante ls` shows `gol: Site-B2C, bff-flight` in the repo column.
- **Cockpit:** group rows in the repo picker are selectable (the arrow folds); `@agent
  <group>` on the one-line syntax; the checklist under the composer; hint
  `→ gol · 2 repos · mirante/<slug>`; Detail header lists the repos; the Diff tab and the
  Done card show one section per repo, each with its own Open PR link.

## Risks

- **Codex commits.** In a linked worktree `git commit` writes into the main repository's
  `.git` (outside every writable root), exactly as for single-repo tasks today: Codex asks
  the person (an approval card). Unchanged by this feature, and on purpose: adding each
  repo's `.git` as a writable root would let the agent rewrite the repository config and
  hooks the hardened git runner guards against.

## Verified live (2026-11-12, Codex 0.167.0, Claude Code 2.1.285)

In a throwaway folder under `$HOME` (not `/tmp`, which Codex's sandbox always allows) laid
out like a workspace (`gol/{A,B,C}`, worktrees of A and B under `_workspaces/x/`):

- `codex sandbox -P :workspace -C _workspaces/x` (the `workspace-write` policy Mirante
  sets): writes into `A/`, `B/` and a new `B/src/deep/` succeed; writes into the read-only
  repo `gol/C` and into A's main checkout are refused; reading `gol/C` works; `git add` in
  `A/` fails on `gol/A/.git/worktrees/A/index.lock` (the commit caveat above).
- `claude -p` in `_workspaces/x` with `acceptEdits`: Write into `A/hello.txt` and
  `B/sub/hello.txt` succeeds, into `gol/C/hello.txt` is denied. With the
  `Read(//…/gol/**)` rule in `--settings`, reading `gol/C/NOTES.md` works in `default`
  mode; without it the read is denied.
- `CLAUDE.md` = `@MIRANTE_WORKSPACE.md` in the cwd: Claude has the file's text in its
  instructions. `AGENTS.md` in the cwd (not a git repo, `codex exec
  --skip-git-repo-check`): Codex has it too.

Not run live: a full Mirante session (daemon + PTY) with the real agents across two repos;
the daemon path is covered by the e2e test with the fake Claude.
- **Read-only is advisory for Claude in bypass modes.** If the person switches Claude to
  `bypassPermissions`, it can write anywhere; Mirante already warns about that mode.
- **Workspace scratch files.** Files the agent writes in the workspace folder (not in a repo)
  are not in any diff; archive refuses while they exist rather than deleting them.
- **A repo named `_workspaces`** in the single-repo layout would share the folder; refused
  at create.
