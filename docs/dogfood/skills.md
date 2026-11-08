# Evolving skills dogfood checklist

Run against the real daemon, the cockpit and real agents (Claude Code 2.1.285, codex-cli
0.167), on throwaway repos first, then on real ones. Tick each line on the day you try it;
write what went wrong next to it. The "Run on 2026-11-08" section records what was checked
without an interactive agent session; "For the founder" lists what is left for you.

This sits on top of `cockpit.md` and `memory.md`: same daemon, same app, now with skills
(Plan 5: `docs/plans/2026-11-08-plan-5-evolving-skills.md`).

Setup: as in `cockpit.md` (`cargo install --path crates/mirante-daemon`, `crates/mirante-hook`,
`crates/mirante-cli`, `crates/mirante-mcp`); build and open the app. The CLI, the daemon and
`mirante-mcp` must be the same build. `/usr/bin/ssh-keygen` must support `-Y sign|verify`
(OpenSSH 8.2+; this Mac has 10.2).

Careful: every `mirante skills` command except `lint` talks to the daemon at
`$MIRANTE_HOME` (default `~/.mirante`) and **starts one if none is running**. For
experiments, `export MIRANTE_HOME=/tmp/mh` (short: the socket path has a length limit).
`mirante skills learn|accept|reject|retire|…` ask you to type the verb on `/dev/tty`; text
typed before the prompt appears is discarded (so a script cannot pre-type it).

## A. Isolation between clients

- [ ] Two checkouts of different clients (e.g. one through the `github.com-client` ssh alias,
      one through `azure-work`). `mirante skills tenant` in each shows the origin
      normalized (`github.com/<owner>/<repo>`, `dev.azure.com/<org>/<project>/<repo>`), with
      no ssh alias, user or token in it, and `(pinned)` after the first task.
- [ ] An org skill of client A is absent from a client B task: Claude's `/skills` in B's
      task does not list it; B's Codex first turn does not mention it; the cockpit's Skills
      chip on B's task does not list it.
- [ ] Before and after a day of use: `stat -f '%m %N' ~/.claude/settings.json
      ~/.codex/config.toml` print the same thing, and `ls ~/.claude/skills ~/.claude/plugins`
      shows nothing from Mirante (skills are per session, under `~/.mirante/sessions/`).

## B. Claude

- [ ] An always-on skill (e.g. "end every answer with OK-SKILL") is followed from the first
      turn, with nothing typed.
- [ ] An on-demand skill: ask for what its description covers; Claude invokes it (the
      `Skill` tool in the transcript) and the cockpit chip shows it as "injected, used";
      Skills shows its "used" count go up.
- [ ] `claude plugin validate --json <session dir>/skills/<session>/claude-plugin` passes
      (see the run below for how to do it without touching `~/.claude`).

## C. Codex

- [ ] A new Codex task: its first turn starts with the `<mirante-skills>` item (always-on
      bodies, then an index of the on-demand `SKILL.md` paths). `mirante attach <task>`
      shows it.
- [ ] `mirante stop <task>` then `mirante resume <task>`: the first turn after the resume
      carries **no** `<mirante-skills>` item again.
- [ ] Ask for what an on-demand skill covers: Codex reads its `SKILL.md` from the session
      folder inside its sandbox (no approval needed for the read).
- [ ] Retire that skill in the cockpit, then stop and resume the task: the feed shows "skills
      withdrawn from this conversation (retired or changed since): <name>" and "the agent
      was told to stop following withdrawn skills and their files were removed: <name>";
      the first turn after the resume starts with `## Withdrawn skills`; the skill's folder
      is gone from the session folder; ask about it: Codex no longer follows it.
- [ ] If a first turn times out, the feed shows a warning line "the first turn carrying this
      session's skills may not have reached the agent (not sent again)".

## D. Evolve (the miner)

- [ ] Turn learning on for a client (cockpit: Skills → Clients → Start learning; or
      `mirante skills learn on`). Type the same instruction in 3 tasks of that client.
      **Find repeated instructions** gives one proposal with the 3 examples (redacted).
- [ ] Accept: a signed v1 (`mirante skills ls` says `verified`). A 4th task gets it with no
      typing.
- [ ] Repeat a new instruction in tasks where that skill was active: an **edit** proposal;
      the card shows "Changes from v1" with the added line, and the activation **unchanged**
      (fixed on 2026-11-08, see below). Accept: v2.
- [ ] Judge the edit: is the new line really about that skill? (The miner appends it to any
      in-scope skill that ran in 2 of 3 of the tasks; see "Findings".) Reject if not.

## E. Agent proposals

- [ ] Ask a learning task's agent to propose a skill. `skill_propose` raises one Claude
      dialog; the card appears in Skills ("from task <branch>") and the rail's
      "Skills (n to review)" counts it.
- [ ] Ask the agent to run `mirante skills accept <id>` with its Bash tool: refused
      (FORBIDDEN "Mirante-launched agents are refused", or no tty). Type it yourself into
      the agent's Terminal tab: refused too.
- [ ] **Refine with agent** on a proposal: only tasks of the same client with learning on
      are offered; the whole message is shown before **Queue the request**; it fences the
      proposal between two `<<<proposal <marker>` / `proposal <marker>>>>` lines and says to
      treat it as data. The agent's answer arrives as a new proposal.

## F. Lint and secrets

- [ ] A proposal containing "use --dangerously-skip-permissions" or a fake token
      (`ghp_` + 36 letters): the card shows the lint error and Accept is off; the token is
      not in SQLite (`sqlite3 ~/.mirante/mirante.sqlite .dump | grep ghp_` finds nothing).
- [ ] Edit a proposal's body in the card to add a fake token: after a moment the lint list
      shows "1 secret will be redacted when this is accepted"; accept: the skill has
      `«redacted»`.

## G. Tamper

- [ ] Edit `~/.mirante/skills/<name>/v1/SKILL.md` by hand. `mirante skills ls` says
      `MODIFIED`; the next session's feed says "skill <name> refused: modified on disk:
      SKILL.md" and the chip counts it as refused.
- [ ] Skills → the skill → **Re-review**: a proposal for that same skill appears; its card
      says "Current version does not verify (…): no trusted base; read the full text" (no
      diff against the changed folder). Accept: v2, verified.
- [ ] Change the folder's `SKILL.md` to say `name: other-skill` and Re-review: refused
      ("now names the skill …"), nothing queued.

## H. No signer

- [ ] `MIRANTE_SSH_KEYGEN=/nonexistent mirante daemon restart`, then start a task: the feed
      says "no skills: ssh-keygen not found at /nonexistent: skills cannot be verified" and
      the session still starts. Restart without it afterwards.

## I. Batch C review live checks

- [ ] **I.1 `` !`cmd` ``.** Write a plugin by hand (outside Mirante) with
      `skills/x/SKILL.md` whose body has `` Today is !`date`. `` and run
      `claude --plugin-dir <it>`; invoke `x`. Record whether Claude runs `date` when it
      loads the skill (if yes, the lint error is warranted). `mirante skills lint` and the
      cockpit refuse it either way.
- [ ] **I.2 Nested `SKILL.md`.** A hand-made plugin with `skills/x/reference/SKILL.md` holding
      `allowed-tools: Bash`: record whether `/skills` lists `reference` (or `x/reference`) as
      a skill. Mirante refuses such a path at any depth either way.
- [ ] **I.3 Changed remote.** In a checkout with a task, `git remote set-url origin <other>`:
      `mirante skills tenant` shows the pinned origin and a WARNING; `learn` and `mine`
      refuse; a new task there gets no skills until a person confirms; the cockpit chip shows
      "Pinned: … (client …)" and "Remote now: … (client …)", and **Use … for new tasks…**
      asks again, naming both clients, before it moves anything.

## J. The cockpit, by eye

- [ ] Skills view with real data: the library groups, the clients, a refused skill, the review
      queue (an edit's "What this edit changes" block with bundled files added / changed /
      removed), at 960×600 and full screen. Nothing overlaps.
- [ ] The Skills chip popover: Tab moves inside it, Escape closes it from anywhere and the
      focus is back on the chip; Escape does not leave the task.

## Run on 2026-11-08 (no interactive agent)

Against an isolated debug daemon (`MIRANTE_HOME=/tmp/mrs.XXXX`, removed afterwards), a
scratch repo with origin `git@github.com:acme-demo/app.git`, and a **fake `claude`**
(`MIRANTE_CLAUDE_BIN`: a script that records its arguments, copies the `--plugin-dir` folder
and the `--append-system-prompt-file` file, then sleeps), so every session's skills could be
read without running a model. Typed confirmations went through `script -q /dev/null`.

| Check | Result |
|---|---|
| `mirante skills tenant` (scratch repo) | `origin github.com/acme-demo/app (pinned)`, `tenant github.com/acme-demo (learning off; 1 tasks, 0 skills)`, exit 0 |
| `skills learn on` with the answer typed before the prompt | refused ("not confirmed"): typeahead is discarded |
| `skills learn on`, typed after the prompt | `skill learning on for github.com/acme-demo` |
| `skills lint`: clean skill / `` !`date` `` / token + `--dangerously-skip-permissions` / missing file | exit 0 "no findings" / exit 1 `shell (error)` / exit 1 `secret` + `forbidden` / exit 3 |
| `skills add` of a folder with `reference/SKILL.md` | refused: `file path "reference/SKILL.md": reserved name` |
| `skills add` of the token proposal | queued with 1 lint error; SQLite holds `«redacted»`, 0 rows with the token; `accept` refused with the lint error |
| `skills accept` of an on-demand org skill (with `reference/endpoints.md`) and an always-on repo skill | the card printed the whole body and the bundled file; `v1 written and signed`; `skills ls` `verified` |
| `ssh-keygen -Y verify` of a library version with `keys/allowed_signers` | `Good "mirante-skill" signature for mirante-local`; one changed byte of the manifest: `incorrect signature` (exit 255); namespace `git`: not verified. A fresh key's sign/verify roundtrip: good (OpenSSH_10.2p1) |
| Session bundle (fake claude) | `--plugin-dir …/skills/<session>/claude-plugin` with `.claude-plugin/plugin.json` and `skills/lookup-api/{SKILL.md,reference/endpoints.md}`; `--append-system-prompt-file …/always.md` with the label and `## tests-first` |
| `claude plugin validate --json` on that bundle (`HOME` and `CLAUDE_CONFIG_DIR` set to a temp folder) | `"success": true`, one warning ("No author information provided"); `--strict`: `"success": false` for that warning only. `~/.claude.json`, `~/.claude/settings.json`, `~/.codex/config.toml` mtimes unchanged |
| Miner: the same instruction in 3 task goals, then `skills mine` | 1 proposal with the 3 examples, but an **edit** of `lookup-api` (on demand, injected in 2 of the 3 tasks) that also turned it **always on**: fixed (below) |
| A 4th task after accepting it | its `always.md` carries the mined line |
| E: `skills accept` with the agent env marker (`MIRANTE_TASK=<task>`) | refused before the prompt: "Mirante-launched agents are refused: skill.may_review is a person's action" (code 6); without a tty: refused ("needs you at a terminal") |
| G: `SKILL.md` of `tests-first` v1 edited by hand | `skills ls`: `MODIFIED`; next session: `skill.refused {"reason":"modified on disk: SKILL.md"}` and not in `always.md`; `skills add <its v1 folder>` + accept: v2 `verified` |
| I.3: `git remote set-url origin https://x-token:ghp_…@github.com/other-org/app.git` | `tenant`: pinned origin + `WARNING: remote.origin.url now reads github.com/other-org/app`; `learn off` and `mine`: refused, nothing changed (exit 1); a new task: `skill.unavailable` "the checkout's origin is github.com/other-org/app, not the confirmed …"; the token appears nowhere in SQLite or `daemon.log` |
| H: daemon restarted with `MIRANTE_SSH_KEYGEN=/nonexistent` | new task: `skill.unavailable {"reason":"ssh-keygen not found at /nonexistent: skills cannot be verified"}`; the agent was still launched (without `--plugin-dir` / `--append-system-prompt-file`) |
| Verification | `cargo test --workspace`, clippy with and without `--features mirante-bridge/fake`, `pnpm test` / `typecheck` / `build`, `src-tauri` test and clippy, `smoke.sh`: see the plan's as-built note |

**Findings.**

1. **Fixed: a mined edit made an on-demand skill always on.** The miner built every proposal
   with activation `always`, an edit included, so accepting the edit above would have put
   `lookup-api` into the system prompt of every matching session. An edit now keeps the
   skill's activation (`fix(skills): a mined edit keeps the skill's activation`); the
   cockpit's edit card also says "Activation: was on demand, now always on" whenever they
   differ.
2. **Open: the miner's edit target.** An instruction repeated in tasks where some in-scope
   skill merely ran (2 of 3 tasks) is appended to that skill, related or not ("Rode cargo
   clippy antes de commitar" landed in an API-lookup skill). Review catches it, but a new
   skill would be the better default unless the skill's text is related. Decide in D.
3. `claude plugin validate --strict` fails on the missing `author` in Mirante's generated
   `plugin.json`; the non-strict check (what the daemon's probe runs) passes.

Not run (need a model, the app or your hands): A's Claude `/skills` and Codex checks, B, C, D
in the app, E's dialog and Refine, F and G in the cockpit, I.1, I.2, J.

## For the founder

1. **C (Codex)**, all of it, on a throwaway repo: first-turn block, no repeat on resume,
   on-demand file read in the sandbox, the withdrawal after a retire. This is the part of
   Plan 5 no test has run against a real Codex.
2. **I.1 and I.2** with a hand-made plugin and `claude --plugin-dir`: does Claude run
   `` !`date` `` at load; does it list a nested `reference/SKILL.md`. Record both.
3. **B**: an always-on skill followed, an on-demand one used and counted as "used".
4. **A** on your two real clients (GOL and Azure aliases): tenants normalized, no leak.
5. **D**: decide finding 2 (the miner's edit target).
6. **E–G and J** in the app: Refine, the edit card's changes block, Re-review of a tampered
   skill, the chip's two-step move and its keyboard behavior.

## Log

| Date | Section | Result | Notes |
|---|---|---|---|
| 2026-11-08 | A (tenant), D (miner), E (CLI), F (CLI), G (CLI), H, I.3, lint, signing, plugin validate | pass after one fix | see "Run on 2026-11-08"; the mined edit's activation was the finding |
