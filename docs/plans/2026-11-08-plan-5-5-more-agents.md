# Plan 5.5: Mirante more agents (free and local models behind the same cockpit guarantees)

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to work through this plan task by task. Steps use checkbox (`- [ ]`) syntax for tracking. Only one committing agent runs at a time (Plan 2b lesson). Read-only reviewers may run in parallel. **Batch A (spikes) gates everything after it:** no driver code before `docs/spikes/FINDINGS-more-agents.md` says go.

**Goal:** Dispatch tasks to agents backed by **free or local models** with the same guarantees Claude and Codex have:
- status only from protocol events (never screen scraping);
- mid-run steer or queue, or an explicit fallback when steering is impossible;
- permission cards a person answers (never auto-approve);
- resume, per-task memory (DNA MCP), skills injection, and memguard.

Examples: "OpenCode · Nemotron 3 Ultra (free)", "OpenCode · qwen/qwen3-coder:free (OpenRouter)", "OpenCode · qwen2.5-coder:7b (local, Ollama)", and, if its spike passes, "Antigravity · Gemini Flash (free)".

**Architecture:**
- **One generic ACP driver, many profiles.** The [Agent Client Protocol](https://agentclientprotocol.com) (JSON-RPC 2.0 over stdio, protocol version `1`) is spoken by OpenCode (`opencode acp`), Gemini CLI (`gemini --acp`, formerly `--experimental-acp`) and Antigravity (`agy acp`). The protocol covers what Mirante needs:
  - `initialize` returns `agentCapabilities` (`loadSession`, `sessionCapabilities.resume`, `mcpCapabilities`, `promptCapabilities`).
  - `session/new {cwd, mcpServers[]}` creates a session.
  - `session/prompt` returns a `stopReason` (`end_turn | max_tokens | max_turn_requests | refusal | cancelled`).
  - `session/update` notifications stream `agent_message_chunk`, `tool_call`, `tool_call_update`, `plan`, `config_option_update` and so on.
  - `session/request_permission` is a server→client request with options of kind `allow_once | allow_always | reject_once | reject_always`.
  - `session/cancel` is a notification. The client MUST answer every pending permission request with `cancelled`, and the agent MUST end the prompt with `stopReason: cancelled`.
  - `session/load` replays history; `session/resume` does not.
  - `session/set_config_option` sets an option, with category `model` for model selection.
  - Agents MUST support stdio MCP servers passed in `session/new`.
  - Per-CLI differences (argv, environment, inline config, how a model is picked, which agent-side permission config forces "ask") live in a pure `AcpProfile` table. The driver, the status engine and the hub logic are shared.
- **Same shape as Codex.** Codex is already a stdio JSON-RPC "protocol-family" agent: the driver computes status (`DriverEvent::Status`), permissions are keyed requests (`PermissionRequested{key}` / `PermissionResolved`), and resume uses a native id. The ACP driver follows that path exactly.
  - The hub's `AgentKind::Claude` / `AgentKind::Codex` branches become `AgentKind::family()` branches: `Terminal` (Claude) or `Protocol` (Codex and every ACP agent).
  - `codex_rpc.rs` is generic already. It moves to `driver/jsonrpc.rs`, and `codex_rpc` re-exports it.
- **What the agent can do is reported, not assumed.** `Driver::caps()` returns `AgentCaps {steer, resume, mcp, skills, permissions, usage, sandbox}`. The values come from the profile, refined by the live `initialize` answer. They are surfaced in `TaskView.caps`, in `agent.catalog`, and in the UI ("no mid-turn steer: queue, or interrupt and send").
- **Agent · Model.** A task has `agent` plus an optional `model` (the agent's own model id, e.g. `opencode/nemotron-3-ultra-free`, `openrouter/qwen/qwen3-coder:free`, `ollama/qwen2.5-coder:7b`). The provider is the id's first segment. The model is fixed at `task.create` and reused on resume. Claude and Codex keep `model: null` ("your plan's default").
- **Credentials never touch Mirante's storage.**
  - First choice: the agent CLI's own login (`opencode providers login`, `agy` Google login).
  - Optional: the macOS Keychain (service `dev.mirante.provider`, account = provider id). The key is written only by `/usr/bin/security` prompting on the person's own tty. The daemon reads it at spawn time with `security find-generic-password … -w` and passes it **only through the agent's environment** (never argv, never a file, never SQLite, never logs).
- **Local model servers count against memory.** Ollama runs outside every agent tree. memguard learns to find it, sample it, show it beside the sessions that use it, and offer a person-only "unload model". Hibernating an agent frees nothing on the model server; the plan says so and the policy accounts for it.

**Tech Stack:**
- Rust 2021 (std threads), the existing `serde_json`, rusqlite 0.41 bundled. **No new crates.** The loopback HTTP GET to Ollama is ~40 lines over `std::net::TcpStream`, fixed to `127.0.0.1:11434`.
- External tools, each version recorded in FINDINGS:
  - OpenCode **1.18.25** (installed; Homebrew stable is 1.19.5; pin whatever the spike validates);
  - Gemini CLI **0.67.0** (`npm i -g @google/gemini-cli`, Apache-2.0, not installed);
  - Antigravity CLI `agy` (antigravity.google download, closed source, not installed);
  - Ollama **0.40.15** (`brew install ollama`, MIT, not installed);
  - `/usr/bin/security` (macOS).
- Frontend: React 19 + Vitest. No new npm dependency.
- Clean-room: designed from the public ACP spec and the public OpenCode, Gemini and Antigravity docs. No third-party code is copied, and the ACP SDKs (`@agentclientprotocol/sdk` 2.4.0) are not used.

---

## Before you start

Read these:
- `docs/spikes/FINDINGS.md` (S1–S4 method, rules 1–9). Batch A repeats S1–S4 for ACP agents.
- `crates/mirante-daemon/src/driver/mod.rs` (`Driver`, `DriverEvent`, `SpawnRequest`, `DriverError::Unsupported`).
- `crates/mirante-daemon/src/driver/codex.rs`, `codex_rpc.rs` and `codex_status.rs`: the template for `acp.rs`, `jsonrpc.rs` and `acp_status.rs`.
- `crates/mirante-daemon/src/driver/real.rs` (`codex_mcp_args`, `codex_env_set`, `prepare_append_log`, `RealFactory::spawn`) and `skill_bundle.rs` (the Codex preamble path).
- `crates/mirante-daemon/src/hub/messages.rs` (`send_by`, `answer_permission`: both branch on `AgentKind`), `hub/session.rs` (`start_session`, spawn request at ~L509), `hub/queue.rs:74`, `hub/skills.rs:1336,1381` (skills filtered by the manifest's `agents`).
- `crates/mirante-daemon/src/access.rs` (every method classified; unclassified means person-only and a failing test) and `crates/mirante-bridge/src/rpc.rs` (`ALLOWED`, pinned by `the_allowlist_is_exactly_these_methods`).
- `crates/mirante-core/src/migrations/0003_lifecycle_archiving.sql`, the `tasks` rebuild pattern. `agent` has `CHECK (agent IN ('claude','codex'))`.
- `apps/cockpit/src/components/{DispatchComposer.tsx,MessageComposer.tsx,AgentCard.tsx,LeftRail.tsx}` and `lib/{dispatch.ts,labels.ts,protocol.ts}`. MessageComposer already offers "interrupt, then send" when the daemon answers `UNSUPPORTED`.

**Written against `main` at `7062bdb`.** Land or stash the uncommitted edits first: `git status` shows `apps/cockpit/src-tauri/src/lib.rs`, `apps/cockpit/src/security.test.ts` and `crates/mirante-bridge/src/attach.rs`. Rust prefix: `export PATH=/opt/homebrew/opt/rustup/bin:$PATH`. Frontend: `pnpm --dir apps/cockpit …`. This machine: Apple M3 Pro, **18 GiB RAM** (memguard's default budget is 4 GiB per session).

### Decisions this plan makes (and why)

1. **A generic ACP driver, not one driver per CLI.**
   - Three candidate agents speak ACP over stdio. One driver plus a profile table is about a third of the code of three drivers, and every future ACP agent costs a profile and a spike.
   - The OpenCode HTTP server (`opencode serve`: `/session/:id/prompt_async`, `/event` SSE, `POST /session/:id/permissions/:permissionID`) is the **documented fallback** only. It is OpenCode-specific, listens on a TCP port, and Mirante has no HTTP client crate. Batch A decides: ACP unless a must-have (G1–G5 below) fails over ACP and passes over HTTP.
2. **`AgentKind` grows and gains a family.**
   - `AgentKind::{Claude, Codex, OpenCode}` now. `Gemini` and `Antigravity` are added in Task 15 only if their spikes pass.
   - `fn family(self) -> Family { Terminal | Protocol }`. Every hub branch that means "the TUI typist" tests `family() == Terminal`, and every branch that means "keyed protocol request" tests `Protocol`. No `AgentKind::Codex` match arm survives in the hub except Codex's skill withdrawal and its sandbox label.
   - **Migration 0010** rebuilds `tasks`: `agent TEXT NOT NULL CHECK (agent GLOB '[a-z]*' AND length(agent) <= 32)`, so no rebuild is needed per future agent and Rust's `AgentKind::parse` stays the gate. It also adds `model TEXT` (NULL means the agent's default).
3. **The model id is opaque, validated and fixed.**
   - `model::ModelId` accepts `^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}$` (no spaces, no control characters, no leading `-`, so it can never become a flag).
   - It is stored on the task, passed to the agent through the profile (ACP `session/set_config_option {configId:<model option>, value}` when the agent advertises a `model` config option, else the profile's inline-config key), and re-applied on resume. Changing models mid-task is deferred.
4. **Permissions: Mirante's policy is forced, checked, and never "always".**
   - ACP agents run **without a sandbox**. Default posture is **"asks for edits and commands"**: reads, globs and greps run; edits, shell, web fetch, external directories, MCP tools other than memory's read tools, and sub-agents ask (open question 1).
   - For OpenCode, the policy goes in `OPENCODE_CONFIG_CONTENT` (inline config), together with `"share":"disabled"`, `"autoupdate":false` and the model.
   - Launch with `--pure`, so no user plugin can answer a permission on its own. **Never `--auto`.**
   - Batch A proves whether a repo-planted `opencode.json` / `.opencode/` can loosen this.
     - If it can, the driver refuses to start in a worktree that holds such a file: `DriverError::Failed("this repository's OpenCode config could widen permissions: …")`.
     - In any case, every `tool_call` whose kind is `edit` / `execute` / `delete` / `move` and was not preceded by a `request_permission` raises a `permission_bypassed` step. The task gets a red `NeedsYou` detail ("ran without asking: <title>").
   - Answers pick **only** an option of kind `allow_once` or `reject_once`. If no `allow_once` option is offered, the card is deny-only and says why. If no `reject_once` is offered, denying answers `cancelled`.
   - The cancel rule from the spec is implemented: on `session/cancel`, every pending request is answered `{outcome:"cancelled"}` and its card settled (`PermissionResolved`).
5. **Status comes from the protocol only (pure `AcpEngine`, time injected).**
   - The session moves to `Working` when a `session/prompt` is sent, or `tool_call` / chunk updates arrive while a prompt is open.
   - It moves to `NeedsYou` while any `request_permission` is pending.
   - It moves to `Idle` on the `session/prompt` response (any `stopReason`; `refusal`, `max_tokens` and `max_turn_requests` become the detail).
   - It moves to `Ended` when the process exits.
   - Codex rule 8 is carried over: a `tool_call` still `in_progress` after the prompt answered keeps `Working` with "turn done, N tool call(s) still finishing", settling on its update or after 30 s. Batch A S3 measures whether this happens.
   - Updates from `session/load` replay are history: they fill the stream and never move the status.
6. **Steering is honest: `caps.steer = false` for ACP unless a spike proves otherwise.**
   - ACP has no steer method, and its spec allows a new `session/prompt` only after the previous one completes.
   - `AcpDriver::send` while a prompt is open returns `DriverError::Unsupported("steer")`. The hub maps that to `UNSUPPORTED` with the message "this agent cannot take a message mid-turn: queue it, or interrupt and send". The cockpit's existing fallback offers exactly that (`session.interrupt`, then `message.send` mode `queue`, delivered at the cancelled turn's end).
   - "Queue" is native: the hub holds the message until `Idle`.
7. **Resume is native or absent, never faked.**
   - `NativeId` = ACP `sessionId`. Resume uses `session/resume` when `sessionCapabilities.resume` is advertised, else `session/load` (replay suppressed from status, rendered dimmed as history in the attach stream), else `caps.resume = false`.
   - When resume is absent, `mirante resume` answers `NativeGone` (fresh start only with `--fresh`), and **memguard never auto-hibernates that session**: `SessionInfo.resumable = false` makes it a `Refused {reason: "cannot resume"}`.
8. **Memory and skills.**
   - Memory and skills MCP servers go in `session/new.mcpServers` as stdio entries `{name, command, args, env:[{name,value}]}`, with the same env as `codex_mcp_args` (`TASK_ENV`, `HOME_ENV`, `AGENT_ENV`).
   - If S5 shows OpenCode ignores client-passed servers, they go in the profile's inline config instead (`mcp.<name> = {type:"local", command:[bin, …args], environment:{…}}`).
   - Skills use Codex's **first-turn preamble** (`skill_bundle` `AgentKind::Codex` arm generalized to `SkillDelivery::Preamble`). It is sent as a leading text content block of the first `session/prompt` of a new session; a resumed session gets nothing again, and gets withdrawals like Codex (`codex_withdrawals` renamed `preamble_withdrawals`).
   - The signed manifest's `agents` set gains `opencode`. New skills default to every known agent. Existing skills reach OpenCode only after a person adds it with the new `skill.action {action:"agents"}`, which creates a new signed version through the same path as `rescope`.
9. **Credentials.**
   - `core::providers` is a static table: `opencode` (no key), `openrouter` → `OPENROUTER_API_KEY`, `google` → `GOOGLE_GENERATIVE_AI_API_KEY` (OpenCode) / `GEMINI_API_KEY` (Gemini CLI), `nvidia` → `NVIDIA_API_KEY`, `ollama` (no key, local). S7 verifies the env names.
   - Keychain access goes through `/usr/bin/security` only (overridable by `MIRANTE_SECURITY_BIN`, never looked up on PATH), with `env_clear()` and `PATH=/usr/bin:/bin`.
     - **Write (CLI only):** `mirante provider key set <p>` execs `security add-generic-password -U -s dev.mirante.provider -a <p> -w`, with `-w` last so `security` itself prompts on the tty. Mirante never sees the key. It is refused from an agent's tree, environment or terminal, like the review commands.
     - **Read (daemon, at spawn):** `security find-generic-password -s dev.mirante.provider -a <p> -w`. The value goes into the agent's `env_set` only.
     - **Delete:** `mirante provider key rm <p>`.
   - **No RPC ever carries a key.** `provider.status` reports `{provider, source: keychain|env|agent-login|none}` only.
   - `redact.rs` gains `nvapi-`. OpenRouter's `sk-or-v1-…` is already covered by `sk-`; a test pins it.
10. **Catalog.**
    - `agent.catalog {refresh?}` lists each agent: binary (via `agent_bin`), version, `caps` (static, refined by a probe), auth hint, and models. Each model is `{id, label, provider, free, local, may_train, needs_key}`.
    - Model source per profile:
      - OpenCode: `opencode models` (no session is created);
      - ACP probe otherwise: `initialize` + `session/new` in `<home>/tmp/probe/` with no MCP, reading the `model` config option's values, then closing. S6 confirms it calls no model.
      - Ollama's `GET /api/tags` adds `ollama/<name>`, marked local, with its size.
    - Cached per binary (path, size, mtime) for 10 minutes.
    - `free` is set **only** from the source's own marking: an OpenCode `-free` suffix, an OpenRouter `:free` suffix, or local. `may_train` comes from a small reviewed table in `core::providers`, filled from S7's terms review (OpenCode Zen documents data use for some free models, and 30-day retention for Nemotron). An unknown model shows no badge.
11. **Usage and quota: shown only when the protocol reports them.** `usage_update` / prompt-response usage (if S2 finds them) become `TaskView.usage` ("31k tokens"). Rate-limit errors (OpenRouter free: 20 requests/min, 50/day under $10 lifetime credit) appear as the status detail with the provider's own text. Nothing is estimated.
12. **Local model servers (memguard).**
    - `memguard::model_servers` finds Ollama processes (executable basename `ollama`, same uid) and samples their footprint with the existing sampler. It reads `GET 127.0.0.1:11434/api/ps` for loaded models (name, size, `size_vram`, expiry).
    - `SystemView.model_servers: Vec<ModelServerView>`. A task whose model is `ollama/*` shows "+ Ollama 4.9 GB (shared)".
    - Policy: under `Elevated`/`Critical`, when a model server is the largest consumer and no working session uses it, the guard offers `OfferUnloadModel`.
    - `model.unload {model}` (person only) sends `POST /api/generate {"model":m,"keep_alive":0}`. Never automatic.
    - The composer warns before dispatching a local model larger than free memory plus what idle sessions use.
13. **YAGNI, deferred:**
    - choosing models for Claude/Codex;
    - changing model mid-task;
    - Mirante starting or stopping `ollama serve`;
    - OpenRouter's `/api/v1/models` catalog fetched by Mirante;
    - the HTTP OpenCode driver (unless Batch A chooses it);
    - ACP `fs/*` and `terminal/*` client capabilities: Mirante declares `fs:{readTextFile:false,writeTextFile:false}, terminal:false`, so the agent uses its own tools under its own permission prompts;
    - cost in money;
    - per-model context-size configuration (documented in dogfood instead).

### Spike go/no-go gates (per agent, Batch A)

| Gate | Must hold | If it fails |
|---|---|---|
| G1 Permissions | Edit and shell under Mirante's config raise `session/request_permission` with an `allow_once` option. A repo-planted config cannot silently loosen this, or Mirante can detect it before launch. | No-go for that agent |
| G2 Status | S3: 0 unexplained mismatches between engine and ground truth over 3 runs (deny path, cancel path, tool still running at `stopReason`) | Fix the engine; no-go if events are missing |
| G3 MCP | A stdio server passed by Mirante (session/new or inline config) starts with Mirante's env, lists its tools, and its write tool asks permission | Memory and skills off for that agent (caps say so) |
| G4 Resume | S4 recalls the secret across processes via `session/resume` or `session/load` | `caps.resume = false` (allowed, shown) |
| G5 No side door | The agent's own tools cannot reach an unauthenticated control surface that answers permissions or sends prompts (e.g. an internal HTTP server of `opencode acp`) | Mitigate (per-session server password not visible to tools) or no-go |

### Wire contract (new and changed)

| Method | Params → result | Who may call | Bridge |
|---|---|---|---|
| `task.create` (changed) | `+ model?: string` | person | yes (already) |
| `agent.catalog` | `{refresh?: bool}` → `AgentEntry[]` `{agent, label, available, path?, version?, error?, caps, auth:"ok"\|"needs_login"\|"unknown", models: ModelEntry[]}` | person | yes |
| `provider.status` | `{}` → `ProviderView[]` `{provider, label, env, source:"keychain"\|"env"\|"agent-login"\|"none"}` | person | yes |
| `model.unload` | `{server:"ollama", model}` → `{}` | person | yes |
| `skill.action` (changed) | `+ action:"agents", agents:[…]` | person | yes (already) |

`TaskView` gains `model?: string` and `caps?: AgentCaps`. `SystemView` gains `model_servers`. Events: `permission_bypassed {tool_call_id, title, kind}`, `model_unloaded {model, by}`, `acp_mode {agent, version, caps}` (audited at boot, like Codex's sandbox). **No method carries a credential.**

## File structure

```
crates/spike-kit/src/acp.rs                         (new) ACP script vars, steer/queue/cancel classifiers, status ground truth
crates/spike-kit/src/bin/spike-acp.rs               (new) plays a JSON-RPC script against any ACP agent; scripted permission answers
crates/spike-kit/src/bin/spike-mcp-echo.rs          (new) stdio MCP server: logs its env, one read tool, one write tool
docs/spikes/FINDINGS-more-agents.md                 (new) per-agent results, G1–G5 verdicts, versions, terms
docs/spikes/raw/acp-{opencode,gemini,agy}.md        (new)
crates/mirante-core/src/model.rs                    (modify) AgentKind::OpenCode, Family, ModelId, Task.model
crates/mirante-core/src/migrations/0010_agents_and_models.sql (new)
crates/mirante-core/src/store.rs                    (modify) MIGRATIONS += 10; create/read tasks.model
crates/mirante-core/src/agent_bin.rs                (modify) Agent::OpenCode (MIRANTE_OPENCODE_BIN; ~/.opencode/bin, /opt/homebrew/bin)
crates/mirante-core/src/providers.rs                (new) provider table, model badges (free/local/may_train)
crates/mirante-core/src/keychain.rs                 (new) Security trait + SecurityCli (find/delete; add argv for the CLI)
crates/mirante-core/src/redact.rs                   (modify) nvapi-
crates/mirante-core/src/skills/format.rs            (modify) known agents += opencode
crates/mirante-ipc/src/types.rs                     (modify) AgentCaps, CreateTask.model, TaskView.{model,caps}, catalog/provider/model-server views, methods
crates/mirante-daemon/src/driver/jsonrpc.rs         (moved from codex_rpc.rs; codex_rpc = re-export)
crates/mirante-daemon/src/driver/acp_proto.rs       (new) params builders, SessionUpdate parsing, permission option choice, renderer
crates/mirante-daemon/src/driver/acp_status.rs      (new) AcpEngine (pure)
crates/mirante-daemon/src/driver/acp_profile.rs     (new) AcpProfile table: OpenCode (+Gemini/Antigravity if gated in)
crates/mirante-daemon/src/driver/acp.rs             (new) AcpDriver
crates/mirante-daemon/src/driver/{mod.rs,real.rs,skill_bundle.rs,fake.rs} (modify) Driver::caps; factory arm; Preamble delivery
crates/mirante-daemon/src/hub/{mod.rs,session.rs,messages.rs,queue.rs,skills.rs} (modify) family branches, caps, steer fallback, model, agents action
crates/mirante-daemon/src/hub/catalog.rs            (new) agent.catalog, provider.status, cache
crates/mirante-daemon/src/memguard/{model_servers.rs,policy.rs,mod.rs} (new/modify)
crates/mirante-daemon/src/{access.rs,server.rs}     (modify) classify + dispatch new methods
crates/mirante-cli/src/{args.rs,main.rs,render.rs}  (modify) --agent opencode --model, `mirante agents`, `mirante provider key …`
crates/mirante-bridge/src/{rpc.rs,fake.rs}          (modify) allowlist + canned catalog
apps/cockpit/src/lib/{protocol.ts,agents.ts,dispatch.ts,labels.ts} (+tests)
apps/cockpit/src/components/{AgentModelPicker.tsx,DispatchComposer.tsx,AgentCard.tsx,LeftRail.tsx,MessageComposer.tsx} (+tests)
apps/cockpit/src/security.test.ts                   (modify) allowlist pin
docs/dogfood/more-agents.md                         (new)
```

---

## Batch A: Spikes (week-1 style; nothing in the product changes)

### Task 0: Toolchain (the founder runs these; record versions)

- [ ] **Step 1:** `opencode --version` (expect 1.18.25). Do not upgrade during the spikes; if you upgrade, rerun all of Batch A.
- [ ] **Step 2 (optional, for the Google path):** `npm i -g @google/gemini-cli@0.67.0`; an AI Studio key from aistudio.google.com/apikey. Then install Antigravity CLI from antigravity.google/download and `agy acp --login`.
- [ ] **Step 3 (local):** `brew install ollama` (0.40.15), `ollama serve` in a terminal, `ollama pull qwen2.5-coder:7b` (~4.7 GB; it fits 18 GiB beside one agent).
- [ ] **Step 4 (OpenRouter):** an OpenRouter account and key. In its privacy settings, allow free endpoints (required by many `:free` models). Note the daily cap (50/day under $10 lifetime credit).
- [ ] **Step 5:** append the versions to `docs/spikes/FINDINGS-more-agents.md`.

### Task 1: `spike-acp` and `spike-mcp-echo`

**Files:** Create `crates/spike-kit/src/acp.rs`, `src/bin/spike-acp.rs`, `src/bin/spike-mcp-echo.rs`.

- [ ] **Step 1: Failing tests (`spike_kit::acp`):**
  - `fills_session_and_cwd_placeholders`
  - `classify_steer_sees_second_prompt_error_or_queue_or_concurrent`: three canned logs, three verdicts;
  - `ground_truth_status_from_log`: prompt sent → working; `request_permission` → needs_you; response → idle;
  - `permission_script_picks_option_by_kind_never_always`.
- [ ] **Step 2: Run** `cargo test -p spike-kit acp`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `spike-acp --bin <path> [--arg …] --script s.jsonl --out log.jsonl [--answer allow_once|reject_once|cancel] [--env K=V]`. It plays the script with `$CWD`, `$SESSION` and `$ECHO` (the echo server path), logs every line in both directions with ms timestamps, and answers `session/request_permission` per `--answer`.
  - Same hygiene as `spike-codex`: nested-session and `CMUX_*` variables removed, auth kept, a child reaper on every exit.
  - `spike-mcp-echo` logs `{env subset, cwd, argv}` to a file named in its argv. It serves `echo_read` (`readOnlyHint:true`) and `echo_write`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(spike-kit): spike-acp script player and an MCP echo server for ACP spikes`.

### Task 2: OpenCode spikes S1–S8 (`opencode acp --pure`)

**Files:** `docs/spikes/raw/acp-opencode.md`, `docs/spikes/FINDINGS-more-agents.md`. Throwaway repo under `/tmp/acp-spike/` (as in week-1).

- [ ] **S0 handshake.** Log the `initialize` response verbatim: `protocolVersion`, `agentCapabilities` (`loadSession`, `sessionCapabilities`, `mcpCapabilities`, `promptCapabilities`), `authMethods`, `agentInfo`. Log the `session/new` response: `modes`, `configOptions` (is there category `model`?), any `models` field.
- [ ] **S1 steer.** Prompt "do 5 numbered steps, one shell `sleep 3` each". At +6 s, send a second `session/prompt` "stop, say BANANA". Record whether it is rejected (error code), queued, or run concurrently. Then the fallback: `session/cancel` followed by a prompt. Record `stopReason: cancelled` and the time to idle. 2 runs. **Verdict:** `caps.steer`.
- [ ] **S2 usage and quota.** Log any `usage_update` or usage field in prompt responses. With an OpenRouter `:free` model, make one call past the rate limit (or a deliberately bad key) and record the JSON-RPC error and `stopReason`. No money: free models only.
- [ ] **S3 status.** 3 scripted runs × ~15 checkpoints, comparing ground truth from `spike_kit::acp` with a hand-labelled timeline. Cover:
  - allow path;
  - deny path (does the turn end? which `stopReason`? do `tool_call_update` events with `failed` arrive?);
  - cancel with a permission pending (does the agent tolerate `cancelled`?);
  - a `tool_call` still `in_progress` after the prompt response (Codex rule 8);
  - a sub-agent (OpenCode `task` tool): does it reuse our `sessionId` or another one?
- [ ] **S4 resume.** Process A: set the secret "JABUTICABA" and kill A. Process B: `session/resume` (if advertised) or `session/load` by id. Record whether `session/load` replays the history as updates before responding.
- [ ] **S5 MCP.**
  - Pass `spike-mcp-echo` in `session/new.mcpServers` with `env:[MIRANTE_TASK=…]`. Record: launched? env as passed? tools listed? Does `echo_write` ask permission and `echo_read` not?
  - If it is not launched, retry through `OPENCODE_CONFIG_CONTENT` `mcp`.
- [ ] **S6 permission safety (G1, G5).**
  - (a) With `OPENCODE_CONFIG_CONTENT` setting `{"permission":{"edit":"ask","bash":"ask","webfetch":"ask","external_directory":"ask"},"share":"disabled","autoupdate":false}` (S6 records the exact accepted key names: 1.18 docs say `permission`, newer docs show `permissions` rules), an edit and a `touch` each raise `request_permission`. Record the options offered (ids, kinds).
  - (b) Plant `opencode.json` and `.opencode/opencode.json` in the repo with `bash: allow`: does it win?
  - (c) A user plugin in `~/.config/opencode` with a permission hook (temp `XDG_CONFIG_HOME`): neutralized by `--pure`?
  - (d) `lsof -nP -p <pid> -iTCP -sTCP:LISTEN` on `opencode acp`: does it listen? If it does, can a bash tool call reach it, and is `OPENCODE_SERVER_PASSWORD` set by Mirante visible in the tool's `env`?
  - (e) `share` really off: no network call to a share endpoint (`nettop -p <pid>` sample).
  - (f) The catalog probe (`initialize` + `session/new`, no prompt) makes no model request, and leaves a session in OpenCode's db (record it).
- [ ] **S7 models and keys.** Run the following, and record which env names OpenCode reads (`OPENROUTER_API_KEY`, `GOOGLE_GENERATIVE_AI_API_KEY`), how a model is selected over ACP (`set_config_option` vs inline `model`), and the terms and data-use notes per provider (OpenCode Zen free-model data policy, OpenRouter free endpoints, NVIDIA retention):
  - (a) `opencode/nemotron-3-ultra-free` with no key: does it work?
  - (b) `openrouter/<a :free coder>` with the key in env only;
  - (c) `google/gemini-*-flash` with an AI Studio key;
  - (d) `ollama/qwen2.5-coder:7b` through inline provider config (`@ai-sdk/openai-compatible`, `baseURL http://127.0.0.1:11434/v1`). Check the context window: Ollama's default may truncate OpenCode's long system prompt. Try `OLLAMA_CONTEXT_LENGTH=32768` on the server and compare tool-call success.
- [ ] **S8 memory.** During S7d: `opencode acp` footprint (and LSP children it spawns), ollama runner footprint (`phys_footprint`, via `spike-kit` or `footprint -p`) vs `/api/ps` `size`/`size_vram`, and the `vm_stat` wired delta. **Question:** does the runner's footprint include Metal buffers?
- [ ] **Commit** `docs(spikes): OpenCode over ACP: steer, status, resume, MCP, permissions, models, memory`.

### Task 3: Gemini CLI and Antigravity spikes (subset)

- [ ] Rerun S0, S1, S3 (one run), S4, S5 and S6(a–d) with `gemini --acp` (falling back to `--experimental-acp`; record which one 0.67.0 accepts) and `agy acp`. Relevant settings:
  - for Gemini: `--approval-mode default` (never `yolo`), and its `--sandbox` option (record whether ACP mode honors it);
  - for `agy`: record its permission defaults and whether a Google login gives a free Flash model.
- [ ] Record G1–G5 per agent, plus the terms note: `agy` is closed source; Gemini CLI's free Google login ended on 2026-06-18, and only the API key path remains.
- [ ] **Commit** `docs(spikes): Gemini CLI and Antigravity over ACP`.

### Task 4: Findings and go/no-go

- [ ] Write the summary table (spike × agent × result × evidence × decision), the G1–G5 verdict per agent, the **caps per agent** (steer, resume, mcp, skills, usage), the profile facts (argv, env, config keys, model selection), and changes to Decisions 4–8, if any.
- [ ] Decide ACP vs HTTP for OpenCode (Decision 1).
- [ ] Founder sign-off line.
- [ ] **Commit** `docs(spikes): more-agents findings and go/no-go`.

---

## Batch B: Core (pure, no daemon)

### Task 5: `AgentKind::OpenCode`, `Family`, `ModelId`, migration 0010

**Files:** `crates/mirante-core/src/{model.rs,store.rs}`, `migrations/0010_agents_and_models.sql`.

- [ ] **Step 1: Failing tests:**
  - `agent_kind_round_trips_opencode`;
  - `family_is_terminal_only_for_claude`;
  - `model_id_accepts_provider_paths_and_free_suffixes`: `opencode/nemotron-3-ultra-free`, `openrouter/qwen/qwen3-coder:free`, `ollama/qwen2.5-coder:7b`;
  - `model_id_refuses_flags_spaces_controls_and_long` (`-m`, `a b`, `\u{1b}`, 129 chars);
  - store: `migration_10_keeps_rows_and_accepts_new_agents`: a v9 db with claude and codex rows migrates, an `opencode` row inserts, `'BAD'` fails the CHECK;
  - `task_model_is_stored_and_null_by_default`.
- [ ] **Step 2: Run** `cargo test -p mirante-core model store`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `text_enum!(AgentKind, "agent", { Claude => "claude", Codex => "codex", OpenCode => "opencode" })`.
  - `Task.model: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`; `NewTask.model`.
  - The migration copies 0003's rebuild with the GLOB check plus `model TEXT`. It runs with foreign keys off (existing `migrate`).

  ```sql
  CREATE TABLE tasks_new ( … agent TEXT NOT NULL CHECK (agent GLOB '[a-z]*' AND length(agent) <= 32), …, model TEXT );
  INSERT INTO tasks_new (…, model) SELECT …, NULL FROM tasks; DROP TABLE tasks; ALTER TABLE tasks_new RENAME TO tasks;
  ```
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): agent kinds beyond Claude and Codex, a task model id (migration 10)`.

### Task 6: `agent_bin` for OpenCode; provider table; badges

**Files:** `agent_bin.rs`, `providers.rs` (new), `lib.rs`.

- [ ] **Step 1: Failing tests:**
  - `opencode_resolves_from_env_then_path_then_well_known` (`MIRANTE_OPENCODE_BIN`, `~/.opencode/bin`, `/opt/homebrew/bin`; shims refused as for Claude and Codex);
  - `provider_of_model_is_first_segment`;
  - `badges_free_only_from_source_markers` (`-free` under `opencode/`, `:free` under `openrouter/`, `ollama/` local, everything else unmarked);
  - `may_train_table_is_explicit_and_unknown_is_none`;
  - `env_name_per_provider_and_agent`.
- [ ] **Step 2: Run** `cargo test -p mirante-core agent_bin providers`. Expected: FAIL.
- [ ] **Step 3: Implement.** `pub struct Provider { id, label, key_env: Option<&'static str>, local: bool }`; `pub fn badges(model:&str) -> Badges {free, local, may_train: Option<&'static str>, needs_key}`. The table is filled from FINDINGS S7.
- [ ] **Step 4–5:** PASS; commit `feat(core): OpenCode binary lookup and the provider table`.

### Task 7: Keychain through `/usr/bin/security`; redaction

**Files:** `keychain.rs` (new), `redact.rs`.

- [ ] **Step 1: Failing tests:**
  - `find_runs_security_with_a_clean_env_and_no_key_in_argv` (fake binary script records argv and env);
  - `not_found_is_none_not_error` (exit 44);
  - `add_argv_ends_with_dash_w_so_security_prompts`;
  - `provider_account_is_validated` (`^[a-z0-9-]{1,32}$`);
  - `value_is_trimmed_and_never_in_debug` (`Secret` newtype, `Debug` prints `«secret»`);
  - `redacts_nvapi_and_sk_or_v1`.
- [ ] **Step 2–4:** fail, implement, pass.
  - `pub trait Security { fn find(&self, provider:&str) -> Result<Option<Secret>>; fn delete(&self, provider:&str) -> Result<bool>; }`
  - `SecurityCli { bin: PathBuf }`, built from `MIRANTE_SECURITY_BIN` or `/usr/bin/security`; 5 s timeout.
- [ ] **Step 5: Commit** `feat(core): provider keys in the macOS Keychain, read through /usr/bin/security`.

---

## Batch C: The ACP driver

### Task 8: `jsonrpc.rs` (move), `acp_proto.rs`

**Files:** `driver/jsonrpc.rs` (moved), `driver/codex_rpc.rs` (now `pub use super::jsonrpc::*;`), `driver/acp_proto.rs` (new).

- [ ] **Step 1: Failing tests (`acp_proto`):**
  - `initialize_declares_no_fs_or_terminal`: `{protocolVersion:1, clientCapabilities:{fs:{readTextFile:false,writeTextFile:false},terminal:false}, clientInfo:{name:"mirante",version}}`;
  - `session_new_passes_stdio_mcp_with_env_pairs`;
  - `parses_every_session_update_kind_and_keeps_unknown_as_other`;
  - `permission_choice_is_allow_once_or_reject_once_never_always`;
  - `no_allow_once_means_deny_only_card`;
  - `no_reject_once_denies_with_cancelled`;
  - `render_lines_for_attach` (agent text, `tool_call` title + kind + status, plan entries; replayed history prefixed `[history]`);
  - `prompt_blocks_put_the_preamble_first`;
  - `card_detail_from_tool_call` (title, kind, `rawInput` command or path, ≤400 chars, redacted).
- [ ] **Step 2: Run** `cargo test -p mirante-daemon acp_proto jsonrpc codex_rpc`. Expected: FAIL (new) / PASS (moved).
- [ ] **Step 3: Implement.** Pure functions only, no I/O:
  - `enum Update { AgentText(String), Thought, ToolCall{id,title,kind,status}, ToolCallUpdate{id,status}, Plan(Vec<..>), ConfigOption(Value), Usage{..}, Other(String) }`;
  - `fn choose(options:&[Value], allow:bool) -> Answer { Selected(id) | Cancelled }`.
- [ ] **Step 4–5:** PASS; commit `refactor(daemon): jsonrpc.rs shared by Codex and ACP; ACP message builders and parsing`.

### Task 9: `AcpEngine` (status from protocol only)

**Files:** `driver/acp_status.rs`.

- [ ] **Step 1: Failing tests:**
  - `prompt_sent_is_working_response_is_idle`;
  - `permission_pending_is_needs_you_until_answered_or_cancelled`;
  - `cancel_voids_pending_permissions_and_reports_their_keys`;
  - `tool_call_open_after_response_keeps_working_then_settles_after_30s` (time injected);
  - `replayed_updates_never_move_status`;
  - `refusal_and_max_tokens_become_detail`;
  - `rpc_error_on_prompt_is_idle_with_the_error_text`;
  - `other_session_ids_do_not_move_status_but_their_permissions_do` (sub-agent rule, as for Codex);
  - `edit_or_execute_tool_call_without_a_prior_permission_is_flagged_bypassed`;
  - `process_closed_is_ended`.
- [ ] **Step 2–4:** fail, implement (mirroring `CodexEngine`'s API: `apply(&AcpInput, now) -> Option<Transition>`, `take_resolved`, `take_bypassed`, `detail`), pass.
- [ ] **Step 5: Commit** `feat(daemon): ACP status engine from protocol events only`.

### Task 10: `AcpProfile` (OpenCode)

**Files:** `driver/acp_profile.rs`.

- [ ] **Step 1: Failing tests:**
  - `opencode_argv_is_acp_pure_and_never_auto`;
  - `opencode_inline_config_forces_ask_share_off_autoupdate_off_and_model` (keys exactly as FINDINGS S6 recorded);
  - `inline_config_refers_to_keys_by_env_never_literal` (`{env:OPENROUTER_API_KEY}`);
  - `ollama_model_adds_openai_compatible_provider_on_loopback`;
  - `mcp_goes_to_session_new_or_inline_per_profile_flag`;
  - `planted_repo_config_is_detected` (`opencode.json[c]`, `.opencode/` holding permission keys → `Err`; only when S6b showed it wins);
  - `env_scrub_reuses_codex_list_and_keeps_auth`.
- [ ] **Step 2–4:** fail, implement, pass.

  ```rust
  pub struct AcpProfile {
      pub kind: AgentKind, pub agent: Agent, pub args: &'static [&'static str],
      pub static_caps: AgentCaps, pub mcp_via: McpVia /* SessionNew | InlineConfig */,
      pub model_via: ModelVia /* ConfigOption | InlineConfig | None */,
      pub config_env: Option<&'static str> /* "OPENCODE_CONFIG_CONTENT" */,
  }
  pub fn launch(p:&AcpProfile, task:&Task, mcp:&McpSet, keys:&[(String,Secret)]) -> Result<AcpLaunch, String>;
  ```
- [ ] **Step 5: Commit** `feat(daemon): OpenCode ACP profile (forced ask policy, no plugins, no share)`.

### Task 11: `AcpDriver` and the factory arm

**Files:** `driver/acp.rs`, `driver/{mod.rs,real.rs,skill_bundle.rs,fake.rs}`.

- [ ] **Step 1: Failing tests** (a fake ACP agent: a test binary script over stdio, as the Codex tests do):
  - `boot_initializes_creates_session_emits_native_id_and_ready`;
  - `send_while_idle_prompts_and_while_working_is_unsupported_steer`;
  - `interrupt_sends_cancel_and_answers_pending_permissions_cancelled`;
  - `permission_request_becomes_a_keyed_card_and_answer_selects_allow_once`;
  - `resume_prefers_session_resume_then_load_and_suppresses_replay_status`;
  - `no_load_capability_means_native_gone_on_resume`;
  - `first_prompt_of_a_new_session_carries_the_skills_preamble_resume_does_not`;
  - `model_is_set_through_config_option_when_advertised`;
  - `caps_reflect_initialize` (no `loadSession` → `resume:false`);
  - `stop_kills_the_tree`;
  - `stderr_goes_to_acp-stderr.log_capped`;
  - `bypassed_tool_call_emits_permission_bypassed_step`;
  - `keys_reach_env_only` (argv and log hold no key).
- [ ] **Step 2: Run** `cargo test -p mirante-daemon acp`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `Driver::caps(&self) -> AgentCaps`: defaults for Claude and Codex; `AcpDriver` returns the profile's caps refined by `initialize`.
  - `SpawnRequest` gains `model: Option<String>` (from the task) and `keys: Vec<(String, Secret)>`, which the hub fills from `keychain`.
  - `RealFactory::spawn` gets an `AgentKind::OpenCode => self.acp(&req, sink, &OPENCODE)` arm.
  - `skill_bundle`: `match agent.skill_delivery() { Native => Claude arm, Preamble => Codex arm }`.
  - The attach stream starts with a mode banner: `[mirante] opencode, asks for edits and commands (no sandbox); model <id>`.
  - `DriverEvent::Sandbox` is reused for the MODE cell (`ask`).
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(daemon): ACP driver (OpenCode): protocol status, keyed permissions, native resume, preamble skills`.

### Task 12: Hub generalization

**Files:** `hub/{mod.rs,session.rs,messages.rs,queue.rs,skills.rs}`, `server.rs`, `access.rs`, `mirante-ipc/src/types.rs`.

- [ ] **Step 1: Failing tests:**
  - `steer_to_a_working_acp_task_is_unsupported_with_the_fallback_message`;
  - `queue_to_a_working_acp_task_delivers_at_idle`;
  - `acp_permission_answer_uses_perm_keys_like_codex`;
  - `typed_message_check_applies_to_terminal_family_only`;
  - `task_create_validates_and_stores_model`;
  - `task_create_refuses_model_for_an_agent_whose_profile_has_no_model_selection`;
  - `spawn_request_carries_keys_for_the_model_provider_only`: an OpenRouter key goes to an `openrouter/*` task, nothing to `opencode/*`;
  - `skills_reach_opencode_only_when_the_manifest_names_it`;
  - `skill_action_agents_creates_a_signed_version`;
  - `resumed_acp_session_gets_withdrawals_not_skills`;
  - `taskview_has_caps_and_model`;
  - `permission_bypassed_event_marks_needs_you_detail`;
  - access: `new_methods_are_classified` (existing table test).
- [ ] **Step 2–4:** fail, implement, pass.
  - Replace `AgentKind::Claude` tests at `messages.rs:82,96,124,207,262,311`, `queue.rs:74` and `session.rs:637,777` with `family() == Terminal`, and `AgentKind::Codex` at `messages.rs:241` with `Protocol`.
  - `session.rs:515` becomes `(k, Some(_)) if k.skill_delivery() == Preamble`.
  - `HubError::Unsupported` → `code::UNSUPPORTED`.
- [ ] **Step 5: Commit** `feat(daemon): the hub speaks to agent families, not to Claude and Codex by name`.

---

## Batch D: Catalog, credentials, memory, CLI

### Task 13: `agent.catalog`, `provider.status`

**Files:** `hub/catalog.rs` (new), `server.rs`, `access.rs` (both `Person`), `types.rs`.

- [ ] **Step 1: Failing tests:**
  - `catalog_lists_every_kind_with_availability_and_error`;
  - `opencode_models_parsed_from_cli_output_with_badges`;
  - `ollama_tags_add_local_models_with_size_when_reachable_and_nothing_when_not`;
  - `cache_keys_on_binary_identity_and_expires_after_10_min`;
  - `refresh_bypasses_cache`;
  - `provider_status_never_contains_a_value`;
  - `agent_callers_are_refused`.
- [ ] **Step 2–4:** fail, implement, pass.
  - `opencode models` runs with the scrubbed env and a 20 s timeout. Its output is one `provider/model` per line; S7 confirms this.
  - Ollama: HTTP/1.0 GET `/api/tags` on `127.0.0.1:11434`, 1 s timeout, 1 MiB cap.
- [ ] **Step 5: Commit** `feat(daemon): agent.catalog and provider.status (no secrets on the wire)`.

### Task 14: Model servers in memguard; `model.unload`

**Files:** `memguard/{model_servers.rs,policy.rs,mod.rs}`, `hub` (`model.unload`), `access.rs` (`Person`), `types.rs`.

- [ ] **Step 1: Failing tests:**
  - `finds_ollama_processes_by_executable_same_uid_only`;
  - `ps_json_parsed_into_loaded_models`;
  - `system_view_lists_model_servers`;
  - `policy_offers_unload_when_model_server_is_largest_and_no_working_session_uses_it`;
  - `policy_never_unloads_automatically`;
  - `non_resumable_session_is_never_auto_hibernated` (`SessionInfo.resumable`);
  - `unload_posts_keep_alive_zero_and_audits_model_unloaded`;
  - `unload_refused_for_agents`.
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5: Commit** `feat(memguard): count local model servers, offer to unload models, never hibernate what cannot resume`.

### Task 15 (gated by Task 3): Gemini CLI / Antigravity profiles

- [ ] Only for agents with G1–G5 go:
  - add `AgentKind::Gemini` / `AgentKind::Antigravity` (no migration: the GLOB check);
  - add `Agent::{Gemini, Agy}` in `agent_bin` (`MIRANTE_GEMINI_BIN`, `MIRANTE_AGY_BIN`);
  - add an `AcpProfile` each (`gemini --acp --approval-mode default`, `agy acp`), with the profile tests from Task 10;
  - add the manifest agent names.
- [ ] **Commit** `feat(daemon): <agent> over ACP`.

### Task 16: CLI

**Files:** `mirante-cli/src/{args.rs,main.rs,render.rs}`, `tests/e2e.rs`.

- [ ] **Step 1: Failing tests:**
  - `new_accepts_opencode_and_model`: `mirante new . --agent opencode --model opencode/nemotron-3-ultra-free --branch b go`;
  - `unknown_agent_lists_known_ones`;
  - `agents_prints_catalog_with_badges_and_caps`;
  - `provider_key_set_execs_security_with_dash_w_last_on_the_tty`;
  - `provider_key_refused_from_agent_env`;
  - `provider_status_prints_sources_only`;
  - `mode_cell_for_opencode_is_ask`;
  - e2e `opencode_task_round_trip_with_fake_acp_agent`.
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5: Commit** `feat(cli): --model, mirante agents, mirante provider key set|rm|status`.

---

## Batch E: Cockpit

### Task 17: Bridge allowlist, fake daemon, security pin

**Files:** `mirante-bridge/src/{rpc.rs,fake.rs}`, `apps/cockpit/src/security.test.ts`.

- [ ] Add `agent.catalog`, `provider.status` and `model.unload` to `ALLOWED`. Update `the_allowlist_is_exactly_these_methods` and the TS pin. The fake daemon answers with a canned catalog: OpenCode with 3 models, Claude, Codex, and an unavailable Gemini.
- [ ] **Commit** `feat(bridge): allow agent.catalog, provider.status, model.unload`.

### Task 18: Protocol types and `lib/agents.ts`

- [ ] **Tests:**
  - `labelFor("opencode","opencode/nemotron-3-ultra-free")` is `"OpenCode · Nemotron 3 Ultra (free)"`;
  - `remembered pick round-trips {agent, model} and falls back to claude`;
  - `capsNotes lists only degraded caps` ("no mid-turn steer", "no resume", "no repo memory");
  - `localModelFits(free, idleBytes, size)`.
- [ ] `AgentKind` becomes `"claude" | "codex" | "opencode" | …` (a string union from the catalog). `AGENT_KEY` stores JSON `{agent, model}`. `dispatch.ts` accepts `@opencode` (it uses the remembered model).
- [ ] **Commit** `feat(cockpit): agent and model types, labels, remembered pick`.

### Task 19: The "Agent · Model" picker in the composer toolbar

**Files:** `components/AgentModelPicker.tsx` (new), `DispatchComposer.tsx`.

- [ ] **Tests:**
  - `toolbar_button_shows_agent_dot_model`;
  - `popover_groups_models_by_agent_with_free_local_may_train_needs_key_badges`;
  - `unavailable_agent_is_listed_disabled_with_its_error`;
  - `needs_key_shows_the_cli_command_not_an_input` (`mirante provider key set openrouter`; the cockpit never takes a key);
  - `local_model_too_big_warns_before_dispatch`;
  - `keyboard_navigation_and_aria_listbox`;
  - `dispatch_sends_model`.
- [ ] It replaces the Claude/Codex `seg` toggle. The catalog loads on open (`refresh` button).
- [ ] **Commit** `feat(cockpit): Agent · Model picker in the dispatch toolbar`.

### Task 20: Honest capabilities on cards and composer; the rail

**Files:** `AgentCard.tsx`, `MessageComposer.tsx`, `LeftRail.tsx`, `labels.ts`.

- [ ] **Tests:**
  - `card_shows_model_and_mode_label_ask`;
  - `card_shows_caps_chips_only_when_degraded`;
  - `steer_button_disabled_with_reason_when_caps_steer_false_and_offers_interrupt_and_send`;
  - `permission_bypassed_detail_is_red`;
  - `rail_lists_agents_from_live_tasks_not_a_fixed_pair`;
  - `memory_panel_shows_ollama_and_unload_button`.
- [ ] **Commit** `feat(cockpit): capabilities, model and model servers shown honestly`.

---

## Batch F: Dogfood and verification

### Task 21: Checklist and real-agent run

**Files:** `docs/dogfood/more-agents.md`.

- [ ] **Step 1: Write the checklist.**
  - **A. Free remote:** dispatch "OpenCode · Nemotron 3 Ultra (free)" on a throwaway repo. An edit raises a card; deny it, then allow once. Status follows without the attach view open.
  - **B. OpenRouter:** `mirante provider key set openrouter`, dispatch a `:free` coder. `ps eww`, `~/.mirante` (grep), the SQLite file and the logs hold no key. Hit the rate limit: the detail shows the provider's message.
  - **C. Local:** Ollama with `qwen2.5-coder:7b`. The memory panel shows Ollama. Unload works, and dispatch warns when the model does not fit.
  - **D. Steer fallback:** "Steer now" while working shows the fallback. Interrupt & send: cards settle as cancelled, and the message arrives as the next prompt.
  - **E. Resume and hibernate:** stop, then resume recalls a secret. A non-resumable agent is never auto-hibernated.
  - **F. Memory and skills:** with DNA on, `memory_search` works from OpenCode. A skill retargeted to `opencode` arrives in the first prompt and is not repeated on resume.
  - **G. Safety:** a planted `opencode.json` with `bash: allow` is refused at start (or is harmless per S6). `--auto` never appears in `ps`. `share` stays off.
  - **H. Client repos:** a "may train" model in a client tenant shows the badge (see open question 2).
- [ ] **Step 2:** `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets --features mirante-bridge/fake -- -D warnings`. Expected: exit 0.
- [ ] **Step 3:** `pnpm --dir apps/cockpit test && pnpm --dir apps/cockpit typecheck && pnpm --dir apps/cockpit build && apps/cockpit/scripts/smoke.sh`. Expected: `SMOKE PASS`.
- [ ] **Step 4:** run A–H with the versions in FINDINGS and log the results.
- [ ] **Step 5: Commit** `docs(dogfood): more agents checklist and real-agent results`.

---

## Self-review: requirements → tasks

| Requirement | Where | Proved by |
|---|---|---|
| Free and local models dispatchable | D1, D3, 10, 11, 13, 19 | `opencode_models_parsed…`, `dispatch_sends_model`, checklist A–C |
| Status only from protocol events | D5, 9 | `replayed_updates_never_move_status`, S3 (G2) |
| Steer, queue or explicit fallback | D6, 11, 12, 20 | `steer_to_a_working_acp_task_is_unsupported…`, checklist D |
| Permission cards, never auto-approve | D4, 8, 10, 11 | `permission_choice_is_allow_once…`, `opencode_argv_is_acp_pure_and_never_auto`, S6 (G1, G5), `bypassed…` |
| Resume | D7, 11 | `resume_prefers_session_resume_then_load…`, S4 (G4) |
| Per-task memory and skills | D8, 11, 12 | `session_new_passes_stdio_mcp…`, `skills_reach_opencode_only…`, S5 (G3) |
| memguard incl. local model servers | D12, 14 | `policy_offers_unload…`, `non_resumable_session…` |
| AgentKind generalized (agent + provider + model) | D2, D3, 5 | migration 10 tests, `model_id_…` |
| Copilot-like "Agent · Model" picker | 19 | `toolbar_button_shows_agent_dot_model` |
| Capabilities surfaced honestly | D1, 11, 20 | `caps_reflect_initialize`, `card_shows_caps_chips…` |
| Credentials in Keychain or env, never SQLite or logs | D9, 7, 16 | `find_runs_security…no_key_in_argv`, `provider_status_never_contains_a_value`, checklist B |
| Cost and quota only if reported | D11, S2 | `rpc_error_on_prompt_is_idle_with_the_error_text` |
| Spikes first | Batch A, gates G1–G5 | FINDINGS-more-agents.md |
| access.rs and bridge pins | 12, 13, 14, 17 | access table test, `the_allowlist_is_exactly_these_methods`, `security.test.ts` |

## Risks

- **No sandbox.** Codex's workspace-write sandbox has no ACP equivalent here. Safety rests on the agent honoring "ask" (G1) plus Mirante's bypass detector. Planted repo config and user plugins are the known holes; S6 measures both.
- **A side door in `opencode acp`.** Its help lists `--port/--hostname`, so it may run an internal HTTP server. If agent tools can reach it unauthenticated, an agent could approve its own permissions. This is gate G5; if it cannot be mitigated, OpenCode is no-go until upstream changes.
- **Free tiers move under us.** Gemini CLI lost its free login on 2026-06-18, OpenCode Zen's free models are "for a limited time", and OpenRouter's free cap is 50 requests/day under $10 lifetime credit, which one agentic task can exhaust. The catalog reflects what the source lists today; nothing is hard-coded as free.
- **Data use on free models.** Some free models may train on prompts (OpenCode Zen lists Big Pickle and MiMo, and 30-day retention for Nemotron; many OpenRouter free endpoints require opting in to training). Client code can leave the machine under those terms. Mitigation: the badge, plus open question 2.
- **Local models on 18 GiB.** A 7B coder fits but calls tools poorly, and Ollama's default context may truncate OpenCode's system prompt silently (S7d). Expect local models to suit small, private tasks only.
- **Sessions in the user's OpenCode db.** Each ACP session, including the catalog probe when it is used, lands in `~/.local/share/opencode/opencode.db`, readable by the user's own OpenCode. This is the same posture as Codex rollouts.
- **Same-uid env.** A key in the agent's env is readable by the agent itself (it can `echo $OPENROUTER_API_KEY` once shell is allowed) and by same-uid processes. This is the same boundary as Plan 4/5. Mitigation: provider-scoped injection, so only the task's own provider key is passed.
- **ACP drift.** `configOptions`, `session/resume` and usage updates are recent spec additions. The profile tests pin what each agent version really sends; the agent version is audited at boot (`acp_mode`).

## Founder decisions (2026-11-08)

1. **Agents with no sandbox run edits inside the worktree on their own, like Codex auto-edits.** Shell commands outside plain reads, and any write outside the worktree, still ask. Deny rules are mandatory so an agent cannot widen its own next session: `.git/**`, `opencode.json*`, `.opencode/**`, `.mirante/**`, hook and settings files, `.claude/**`, `.codex/**`. Tests pin them.
2. **"May train on your data" models:** when the person picks one, the composer shows that it needs setup if it does (credentials or provider opt-in, with the steps) **and** a clear warning ("this provider may train on your prompts and code"). In a client tenant (any tenant other than `personal`), require an explicit per-tenant allow toggle, with the same person-only review as skill learning; until it is allowed, dispatch is refused with the reason. The warning badge stays visible on cards that use such a model.
3. **Antigravity (`agy`) is out for now.** Only open-source agents: OpenCode first; Gemini CLI with an AI Studio key if its spike passes. Revisit `agy` later.

## Execution handoff

Save as `docs/plans/2026-11-08-plan-5-5-more-agents.md`. Execute A → F with a review after each batch. **A gates B–F:** if OpenCode fails G1 or G5, stop and bring FINDINGS to the founder. B is pure Rust. C needs A's profile facts. D's Task 15 is skipped for any agent that failed Task 3. E can start at Task 17 once Task 13's wire types land. Run this plan before Plan 6 (bots).

### Critical Files for Implementation
- ~/Documents/Develop/mirante/crates/mirante-daemon/src/driver/codex.rs, codex_rpc.rs and codex_status.rs (the template for acp.rs, jsonrpc.rs and acp_status.rs)
- ~/Documents/Develop/mirante/crates/mirante-daemon/src/driver/real.rs and driver/mod.rs (factory arm, `SpawnRequest.{model,keys}`, `Driver::caps`, MCP env)
- ~/Documents/Develop/mirante/crates/mirante-daemon/src/hub/messages.rs, hub/session.rs and hub/queue.rs (every `AgentKind` branch becomes a family branch; steer fallback)
- ~/Documents/Develop/mirante/crates/mirante-core/src/model.rs, store.rs and migrations/0003_lifecycle_archiving.sql (the rebuild pattern for migration 0010)
- ~/Documents/Develop/mirante/apps/cockpit/src/components/DispatchComposer.tsx and MessageComposer.tsx (picker; the existing UNSUPPORTED fallback)

### Facts verified while researching (2026-11-08; not assumed)
- **On this machine:**
  - `opencode` 1.18.25 at `/opt/homebrew/bin/opencode` → `Cellar/opencode/1.18.25`, from the `anomalyco/tap` tap; Homebrew stable is 1.19.5 and npm `opencode-ai` is 1.19.5.
  - Its subcommands include `acp`, `serve`, `run --format json`, `models`, `providers login|logout|list`, `session`, `export`, `stats`. Its flags include `--pure` ("run without external plugins") and `--auto` ("auto-approve permissions… (dangerous!)"). `opencode acp` takes `--port/--hostname/--cwd` and has no `--model`.
  - `~/.config/opencode/opencode.jsonc` holds only `$schema`. There is no `~/.local/share/opencode/auth.json` (no provider logged in). The db is at `~/.local/share/opencode/opencode.db`.
  - With no credentials, `opencode models` lists 7 models, all `opencode/*`: `big-pickle`, `ling-3.0-flash-fin-free`, `mimo-v2.5-free`, `muse-spark-1.2-contributor-free`, `muse-spark-1.3-contributor-free`, `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`.
  - `gemini` and `ollama` are not installed. `claude` and `codex` on PATH are cmux shims. Node v22.14.0.
  - Apple M3 Pro, 19,327,352,832 bytes RAM.
- **Mirante code:**
  - `tasks.agent` has `CHECK (agent IN ('claude','codex'))` (0001, kept by 0003's rebuild); migrations end at 0009.
  - Skills are filtered by the manifest's `agents` (`hub/skills.rs:1336,1381`; `format.rs:202` allows only claude and codex).
  - `redact.rs` covers `sk-` and `AIza` but not `nvapi-`.
  - The bridge `ALLOWED` list is pinned by a test.
  - MessageComposer already implements the UNSUPPORTED → interrupt-and-send fallback.
- **ACP spec** (agentclientprotocol.com):
  - `protocolVersion` is an integer (1).
  - Methods: `initialize`, `authenticate`, `session/new {cwd, mcpServers}`, `session/load`, `session/resume` (`sessionCapabilities.resume`), `session/prompt` (stopReason `end_turn | max_tokens | max_turn_requests | refusal | cancelled`), `session/cancel` (notification), `session/set_config_option` (categories `mode | model | model_config | thought_level`), and agent→client `session/request_permission` (options `allow_once | allow_always | reject_once | reject_always`).
  - The client MUST answer pending permission requests `cancelled` on cancel. "All Agents MUST support… MCP servers via stdio". `session/load` MUST replay history; `session/resume` MUST NOT.
  - Prompts are sequential (no steer method).
  - npm `@agentclientprotocol/sdk` 2.4.0 exists (not used).
- **OpenCode docs:**
  - ACP supports built-in tools, MCP from OpenCode config, AGENTS.md and the permission system; `/undo` and `/redo` are unsupported.
  - Permissions are `allow | ask | deny`, and replies are `once | always | reject`. Newer docs show a base policy of allow-all, except asking for `.env`, so Mirante must force ask.
  - Config precedence and substitution: `{env:VAR}` and `{file:…}`. Project configs are merged walking up the directory tree. The server API has `POST /session/:id/permissions/:permissionID`, `/event` SSE, `prompt_async`, and `OPENCODE_SERVER_PASSWORD` basic auth.
  - OpenCode Zen free models are "for a limited time". Some use data for training; Nemotron has 30-day retention.
  - The fetched docs say Zen models require sign-in, but the local CLI lists free models without credentials. Spike S7a settles it.
- **Gemini CLI** (npm `@google/gemini-cli` 0.67.0, Apache-2.0, node ≥20, modified 2026-11-04):
  - Its README says unpaid-tier and Google One users were moved to Antigravity CLI on 2026-06-18.
  - The API key (`GEMINI_API_KEY`) and Vertex paths remain.
  - The ACP flag is `--acp` (formerly `--experimental-acp`).
- **Antigravity CLI:** `agy acp` starts an ACP server, with `agy acp --login`. Free Flash tier with reduced daily quota; closed source.
- **OpenRouter free models:** 20 requests/min flat; 50/day under 10 credits purchased, 1,000/day at 10 or more (failed requests may count). Nemotron 3 Ultra is on the free tier.
- **Ollama:** Homebrew `ollama` 0.40.15, MIT.

Sources: [ACP overview](https://agentclientprotocol.com/protocol/overview), [ACP prompt turn](https://agentclientprotocol.com/protocol/prompt-turn), [ACP session setup](https://agentclientprotocol.com/protocol/session-setup), [ACP config options](https://agentclientprotocol.com/protocol/session-config-options), [OpenCode ACP](https://docs.opencode.ai/docs/acp/), [OpenCode server](https://docs.opencode.ai/docs/server/), [OpenCode permissions](https://opencode.ai/docs/permissions/), [OpenCode config](https://opencode.ai/docs/config/), [OpenCode Zen](https://opencode.ai/docs/zen/), [Gemini CLI repo](https://github.com/google-gemini/gemini-cli), [Gemini CLI changelogs](https://geminicli.com/docs/changelogs/), [Gemini CLI → Antigravity migration](https://inventivehq.com/blog/gemini-cli-deprecated-antigravity-cli-migration), [Antigravity ACP](https://antigravity.google/docs/cli/acp/), [OpenRouter free API comparison](https://openrouter.ai/blog/tutorials/free-llm-apis-compared/), [OpenRouter free-tier limits](https://www.truefoundry.com/es/blog/openrouter-rate-limits-your-free-tier-depends-on-what-youve-already-paid).