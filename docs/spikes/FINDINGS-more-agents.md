# Plan 5.5 spike findings: more agents over ACP

Date 2026-11-08. Machine: Apple M3 Pro, 18 GiB, macOS 26.5 arm64, Rust 1.99.1.
Raw evidence: `docs/spikes/raw/acp-opencode.md`. Tools: `spike-acp` and `spike-mcp-echo` (crates/spike-kit).

## Toolchain (Task 0)

| Tool | Version | State |
|---|---|---|
| OpenCode | **1.18.25** (`opencode --version`; Homebrew `anomalyco/tap`) | Used for every spike below. Not upgraded. Homebrew stable is 1.19.5: upgrading means rerunning Batch A. |
| Gemini CLI | — | **Not installed** (Task 0 Step 2, founder) |
| Ollama | — | **Not installed** (Task 0 Step 3, founder) |
| OpenRouter account and key | — | **None** (Task 0 Step 4, founder) |
| Antigravity `agy` | — | **Out** (founder decision 3). The `agy` on PATH is a cmux shim; it was not run. |

Pending for the founder (Task 0):
- **Step 2:** `npm i -g @google/gemini-cli@0.67.0`, then an AI Studio key from aistudio.google.com/apikey (Gemini CLI's free Google login ended on 2026-06-18; only the key path remains).
- **Step 3:** `brew install ollama` (0.40.15), `ollama serve` in a terminal, `ollama pull qwen2.5-coder:7b` (~4.7 GB).
- **Step 4:** an OpenRouter account and key; in its privacy settings, allow free endpoints. Note the cap: 50 requests/day under $10 lifetime credit.
- After each step, append the version here.

## Method

- Every run used a throwaway repo (`/tmp/acp-spike/repo`) and a **temporary `HOME` and `XDG_*_HOME`** under `/tmp/acp-spike/home`. The founder's `~/.config/opencode` and `~/.local/share/opencode` were not read or written (db mtime unchanged).
- Launch: `opencode acp --pure`, env scrubbed like `spike-codex`, own process group, reaped on exit. The client declares `fs:{readTextFile:false,writeTextFile:false}, terminal:false`.
- Permissions were answered by script, **never** with an `*_always` option. `allow_once` was given only to tool calls whose JSON matched the expected command and named no path outside `/tmp/acp-spike` (`--allow-match`); everything else got `reject_once`.
- Models: OpenCode Zen's free models, no key. The plan's `opencode/nemotron-3-ultra-free` **left the catalog during the session** (see S7), so the runs used `opencode/nemotron-3.5-lightning-free`. One S1 run used `opencode/big-pickle` by accident.

## OpenCode 1.18.25 over ACP (Task 2)

| Spike | Result | Evidence | Decision |
|---|---|---|---|
| S0 handshake | `protocolVersion 1`, `loadSession: true`, `sessionCapabilities {close, fork, list, resume}`, `mcpCapabilities {http, sse}`, `promptCapabilities {embeddedContext, image}`, `authMethods [opencode-login]`. `session/new` has **no `modes`/`models`**, only `configOptions`: `model` (category `model`, default `opencode/big-pickle`) and `mode` (`build`/`plan`). | raw S0 (verbatim) | Model via the `model` config option |
| S1 steer (3 runs) | A second `session/prompt` sent mid-turn is **accepted and absorbed at the next model step**, 3/3: the agent finished the step in flight, then answered BANANA. **Both requests are answered** back to back with the same `stopReason` and `usage`. No error. Fallback: `session/cancel` → `stopReason: cancelled` in 57–75 ms; the running shell is aborted; the next prompt is accepted at once. | raw S1 table, `classify_steer` → Absorbed ×3 | `caps.steer = true` for this version (see Decision 6 change) |
| S2 usage | `usage_update {used, size, cost{amount, currency}}` at each turn end, and `usage {inputTokens, outputTokens, cachedReadTokens, thoughtTokens, totalTokens}` in every prompt result. **An inline `model` missing from the catalog silently falls back to `big-pickle`; `set_config_option` with it fails with `-32602 model not found`.** Rate-limit error: pending (OpenRouter). | raw S2 | `caps.usage = true`; model only through `set_config_option`, then verify `currentValue` |
| S3 status (3 runs, 29–35 checkpoints each) | Allow, deny (`reject_once` → tool `failed`, turn goes on, `end_turn`) and cancel-with-pending-permission (`cancelled` accepted, tool `failed` "aborted", `stopReason: cancelled`): **0 mismatches** (only same-ms ordering ties). Tool still running after the prompt response (Codex rule 8): **never seen** (0/6 runs). **Sub-agent (`task` tool): its permission request is never sent over ACP**, and its child session emits nothing. The turn hangs "working" until the process dies: 1 unexplained mismatch per run (3/3). With `"task":"deny"` the call ends at once (`Invalid Tool`). | raw S3 | Force `task: deny` |
| S4 resume | Across a SIGKILL: `session/resume` recalls JABUTICABA with **no replay**. `session/load` **replays** user and agent chunks **before** its response, then recalls it too. The model is kept per session. | raw S4 | `caps.resume = true` via `session/resume` |
| S5 MCP | A stdio server in `session/new.mcpServers` is launched at `session/new` with Mirante's env pairs **plus the entire agent env**. Tools are listed as `<server>_<tool>`. **By default MCP tools run without asking** (readOnlyHint ignored). With `"<server>_*":"ask"` plus `"<server>_<read tool>":"allow"`, the write tool asks and the read tool does not. MCP permission cards carry **empty `rawInput`** (no arguments). | raw S5 | `caps.mcp = true`, with per-server permission rules |
| S6 (a) | The key is **`permission`**; `permissions` (V2) makes 1.18.25 exit with a config error (fails closed). With no rules, shell runs without asking. Under Mirante's policy, edit, bash, external directory and configured MCP tools ask. Options are `once/allow_once`, `always/allow_always`, `reject/reject_once` (no `reject_always`). | raw S6a | Pin the key name in profile tests |
| S6 (b) | `bash: allow` planted in `opencode.json`, `opencode.jsonc`, `.opencode/opencode.json`, a parent-dir `opencode.json`, and as a pattern rule (`{"touch*":"allow"}`): **the inline `OPENCODE_CONFIG_CONTENT` won in all 5**. | raw S6b | No refusal needed for planted permission keys |
| S6 (c) | User and repo plugins (`plugin/`, `plugins/`): **not loaded with `--pure`**. Without it, both load and run code at session start. | raw S6c | `--pure` mandatory (as planned) |
| Founder posture | `edit: {"*":"allow", ".git/**", "opencode.json*", ".opencode/**", ".mirante/**", ".claude/**", ".codex/**": "deny"}`: worktree edit ran without a card, all four protected writes were refused by rule, and a write outside the worktree raised an `external_directory` card. | raw S6 posture | Decision 1 is implementable as written |
| S6 (d) | **`opencode acp` listens on TCP `127.0.0.1:4096`** (random port if 4096 is taken), and the ACP layer is itself an HTTP client of it. Public docs: no auth unless `OPENCODE_SERVER_PASSWORD` is set; the API answers permissions (`POST /session/:id/permissions/:permissionID`). Setting the password keeps ACP working, but **the password is inherited by tool processes**. An HTTP request to the listener was **not** made (blocked by the spike runner's own permission policy): reachability without credentials is from the docs, not measured. | raw S6d | **G5 open** (below) |
| S6 (e) | With `share: disabled`, the only remote endpoint during a prompt was `opencode.ai` (Zen API); nothing to the share host `opncd.ai`; 0 share rows over 32 sessions (sampled, not a full capture). | raw S6e | Keep `share: disabled` |
| S6 (f) | Catalog probe (`initialize` + `session/new`): loopback only, no model request, but it **leaves an empty session row** in `opencode.db`. | raw S6f | Catalog from `opencode models`, not the probe |
| S7 (a) | Free Zen models work with no login or key. The free list **changed within minutes** (`nemotron-3-ultra-free` listed at 15:43, gone at 15:47; 6 models, then 11). The default model when nothing valid is chosen is `opencode/big-pickle`. | raw S0, S7 | Nothing hard-coded as free; refresh the catalog |
| S7 (b–d) | OpenRouter, Google AI Studio and Ollama paths | — | **Pending** (founder: Task 0 Steps 2–4) |
| S8 memory | Ollama footprint and Metal buffers | — | **Pending** (needs S7d) |

Other facts for the profile:
- After an approved edit, OpenCode writes the file itself **and** sends the client `fs/write_text_file` although the client declared `writeTextFile:false`. Answering `-32601` is harmless (stderr only).
- A tool call goes `pending` → `in_progress` **before** its `session/request_permission`. The bypass detector must flag only a `completed` edit/execute that had no permission request, not an `in_progress` one.
- Even with `--pure`, OpenCode writes into the user's config dir: it creates `$XDG_CONFIG_HOME/opencode/opencode.jsonc`, `.gitignore` and `package.json` and runs an npm install of `@opencode-ai/plugin` there (network, ~110 MB npm cache). Sessions, a shadow git snapshot of the worktree (`$XDG_DATA_HOME/opencode/snapshot/`), logs and the model cache land under the user's XDG dirs.
- Two `opencode acp` processes sharing one data dir ran concurrently without trouble (the second took a random port).

## Gemini CLI and Antigravity (Task 3)

- **Antigravity (`agy`): skipped.** Founder decision 3 (2026-11-08) puts it out for now (closed source). No `agy` spike was run; the `agy` on PATH (a cmux shim) was not invoked. Revisit later.
- **Gemini CLI: pending.** `gemini` is not installed, and since 2026-06-18 it has no free Google login: it needs an AI Studio key (`GEMINI_API_KEY`). Nothing was run.
  - To run after Task 0 Step 2: S0, S1, S3 (one run), S4, S5 and S6(a–d) with `spike-acp --bin $(which gemini) --arg --acp --arg --approval-mode --arg default` (fall back to `--experimental-acp`; record which one 0.67.0 accepts; never `yolo`), the key only in the spike's env (`--env GEMINI_API_KEY=…`), and the same temp `HOME`/XDG isolation as OpenCode. Record whether ACP mode honours `--sandbox`, and G1–G5.
- Task 15 (Gemini/Antigravity profiles) stays gated: no agent other than OpenCode has a verdict.
