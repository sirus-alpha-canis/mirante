# OpenCode over ACP: raw evidence (Plan 5.5 Batch A, Task 2)

Date 2026-11-08. OpenCode **1.18.25** (`/opt/homebrew/bin/opencode`, not upgraded), `opencode acp --pure`, macOS 26.5 arm64.
Player: `spike-acp` (crates/spike-kit), logs `{ts, dir, line}` JSONL. Throwaway repo `/tmp/acp-spike/repo` (git, `README.md`, `calc.py`).

**Isolation.** Every run set `HOME=/tmp/acp-spike/home` and `XDG_{CONFIG,DATA,STATE,CACHE}_HOME` under it. The founder's own `~/.config/opencode` and `~/.local/share/opencode/opencode.db` were not touched (db mtime unchanged at 05:17:42 before and after all runs).

**Where OpenCode wrote state** (all under the temp home):
- `$XDG_DATA_HOME/opencode/opencode.db` (+ `-wal`, `-shm`): every session, including probe sessions with no prompt; child sessions of sub-agents (`parent_id` set).
- `$XDG_DATA_HOME/opencode/snapshot/<project-id>/<hash>`: a shadow git repository that tracks the worktree (`message=tracking … git=…/snapshot/…`).
- `$XDG_DATA_HOME/opencode/log/opencode.log`, `$XDG_DATA_HOME/opencode/repos/` (empty).
- `$XDG_STATE_HOME/opencode/locks/`.
- `$XDG_CACHE_HOME/opencode/models.json` (the model catalog, refreshed from the network) and `…/bin/`.
- `$XDG_CONFIG_HOME/opencode/`: OpenCode **creates** `opencode.jsonc` (`{"$schema": …}`), `.gitignore`, `package.json` (`@opencode-ai/plugin` 1.18.25) and runs an npm install there (`node_modules/`, `package-lock.json`, ~110 MB in `$HOME/.npm`). This happens **with `--pure`** too.

Default script header (`hdr.jsonl`): `initialize {protocolVersion:1, clientCapabilities:{fs:{readTextFile:false,writeTextFile:false},terminal:false}}` → `session/new {cwd, mcpServers:[]}` → `session/set_config_option {configId:"model", value:"opencode/nemotron-3.5-lightning-free"}`.
Default inline config (`OPENCODE_CONFIG_CONTENT`): `{"permission":{"edit":"ask","bash":"ask","webfetch":"ask","external_directory":"ask"},"share":"disabled","autoupdate":false}`.

## S0 handshake

`initialize` result, verbatim:

```json
{"agentCapabilities":{"loadSession":true,"mcpCapabilities":{"http":true,"sse":true},
 "promptCapabilities":{"embeddedContext":true,"image":true},
 "sessionCapabilities":{"close":{},"fork":{},"list":{},"resume":{}}},
 "agentInfo":{"name":"OpenCode","version":"1.18.25"},
 "authMethods":[{"description":"Run `opencode auth login` in the terminal","id":"opencode-login","name":"Login with opencode"}],
 "protocolVersion":1}
```

`session/new` result: `sessionId` (`ses_…`), **no `modes` and no `models` field**, and `configOptions`:
- `{id:"model", category:"model", type:"select", currentValue:"opencode/big-pickle", options:[{name, value}…]}`;
- `{id:"mode", category:"mode", currentValue:"build", options:[build, plan ("Disallows all edit tools")]}`.

Then one `session/update` `available_commands_update` (`customize-opencode`, `init`, `review`).

The model list **changed during the day**. First run (15:43 local, before `models.json` was cached): `big-pickle, hy3-free, mimo-v2.5-free, muse-spark-1.2-contributor-free, nemotron-3-ultra-free, nemotron-3.5-lightning-free`. From the second run on: `beam-free, big-pickle, crouton-alpha-free, deepseek-v4.1-flash-free, flint-40b-free, glm-5.3-flash-free, iceman-free, inkling-2-small-free, mimo-v2.6-flash-free, nemotron-3.5-lightning-free, nemotron-3.5-super-vl-free`. `opencode models` (same temp home) prints the same 11, one `provider/model` per line. **`opencode/nemotron-3-ultra-free` disappeared**, so every model run below uses `opencode/nemotron-3.5-lightning-free` (S1 run 0 used `big-pickle` by accident, see S7).

## S1 steer (3 runs: run 0 big-pickle, runs 1–2 Nemotron 3.5 Lightning)

Prompt 4 "Do 5 numbered steps … `sleep 3` … one bash call per step", then prompt 5 "stop, say BANANA" at +6 s, while 4 is open. `--answer allow_once --allow-match "sleep 3"`.

| Run | Prompt 5 sent | What happened | Responses | `classify_steer` |
|---|---|---|---|---|
| 0 | +6.9 s (model still on its first step) | step 1 ran (`sleep 3`), then the next model step answered `BANANA` | id 4 and id 5 both `end_turn` at +21.397 s, identical `usage` | Absorbed |
| 1 | +7.1 s | step 1 ran, then `BANANA` | id 4 +13.030 s, id 5 +13.046 s, identical `usage` | Absorbed |
| 2 | +6.9 s (step 1's `sleep 3` running) | step 1 finished at +8.7 s, `BANANA` at +9.7 s | both +9.734 s | Absorbed |

No JSON-RPC error. The second `session/prompt` is **accepted while the first is open and joins the running turn at the next model step** (same behaviour as Claude's `absorbed_mid_turn`). Both requests are then answered with the same result. A `usage_update` precedes each response.

**Fallback (cancel then prompt), same runs:** prompt 6 (5 steps again) → first `tool_call` → +3 s → `session/cancel`.

| Run | cancel → `stopReason:"cancelled"` | running tool |
|---|---|---|
| 0 | 57 ms | `tool_call_update` `completed`, output "User aborted the command", `exit: null` |
| 1 | 75 ms | same (`completed`, aborted) |
| 2 | ~60 ms | same |

The next prompt (7) was accepted at once (5–50 ms later). Run 0 answered BANANA; in runs 1–2 Nemotron Lightning re-did the 5 steps instead (a model choice, not a protocol fault).

Ordering seen in every run: `tool_call` (`pending`, title `bash`, empty `rawInput`) → `tool_call_update` `in_progress` (title = the command) → `session/request_permission` (same ms or a few ms later) → answer → `in_progress` again → `completed`/`failed`. **A tool call reaches `in_progress` before its permission is asked**, so "in progress without a prior permission" is not a bypass signal; only `completed` without a prior permission is.

## S2 usage and quota

- Every turn ends with `session/update` `usage_update {used, size, cost:{amount, currency:"USD"}}` (e.g. `used 8264, size 262144, cost 0`) and the `session/prompt` result carries `usage {inputTokens, outputTokens, cachedReadTokens, thoughtTokens, totalTokens}`. A cancelled turn reports all zeros.
- **Model gone from the catalog:** inline config `"model":"opencode/nemotron-3-ultra-free"` → `session/new` silently reports `currentValue:"opencode/big-pickle"` and the prompt runs on Big Pickle. **No error.** The same id through `session/set_config_option` fails loudly: `-32602 "Invalid params: model not found: opencode/nemotron-3-ultra-free"`, data `{modelId, providerId:"opencode"}`.
- Rate limit / bad key on OpenRouter: **pending** (needs the founder's OpenRouter key; Task 0 Step 4).

## S3 status (3 runs × 29–35 checkpoints)

Script: prompt 4 write `hello.txt` + `cat` (allow) · prompt 5 `touch denied.txt` (reject) · prompt 6 `sleep 30 && echo held` (held, then `session/cancel`) · prompt 7 sub-agent via the `task` tool. The engine is `spike_kit::acp::ground_truth` (prompt open / permission pending / prompt answered / exit). The label is derived independently from tool-call states (`s3check.py`): a tool awaiting approval → needs_you, a turn open → working, a tool still `pending|in_progress` after the response → working (rule 8), else idle.

Run 1 (runs 2 and 3 have the same shape, 30 checkpoints each):

```
+   1197 prompt sent                  engine=Working   label=Working
+   3577 tool in_progress             engine=Working   label=Working
+   3578 permission asked (edit)      engine=NeedsYou  label=NeedsYou
+   3880 permission answered          engine=Working   label=Working
+   3897 tool completed               engine=Working   label=Working
+   4947 permission asked (cat)       engine=NeedsYou  label=NeedsYou
+   5252 permission answered          engine=Working   label=Working
+   7441 prompt response end_turn     engine=Idle      label=Idle
+   7455 prompt sent                  engine=Working   label=Working
+   9040 tool pending                 engine=NeedsYou  label=Working    same-ms tie
+   9040 permission asked (touch)     engine=NeedsYou  label=NeedsYou
+   9345 permission answered (reject) engine=Working   label=Working
+   9354 tool failed                  engine=Working   label=Working
+   9417 prompt response end_turn     engine=Idle      label=Idle
+   9446 prompt sent                  engine=Working   label=Working
+  10463 permission asked (held)      engine=NeedsYou  label=NeedsYou
+  12000 cancel + answered cancelled  engine=Working   label=Working
+  12306 tool failed                  engine=Working   label=Working
+  12308 prompt response cancelled    engine=Idle      label=Idle
+  12340 prompt sent (sub-agent)      engine=Working   label=Working
+  13711 tool `task` in_progress      engine=Working   label=Working   (hand label: NeedsYou, see below)
+ 256062 exit                         engine=Ended     label=Ended
checkpoints=35 mismatches=0 (2 same-ms ordering ties)
```

- **Allow path:** clean in 3/3.
- **Deny path:** `reject_once` → `tool_call_update` `failed`, `rawOutput.error` "The user rejected permission to use this specific tool call." The turn continues; the model answered REFUSED, `stopReason: end_turn`. 3/3.
- **Cancel with a permission pending:** `session/cancel`, then the client answers `{outcome:"cancelled"}`. OpenCode accepted it, the tool went `failed` ("Tool execution aborted", `metadata.interrupted: true`) and the prompt ended `cancelled` 306 ms after the cancel. 3/3.
- **Tool still `in_progress` after the prompt response (Codex rule 8):** **0 occurrences** in S1 (3 runs) and S3 (3 runs). The final `tool_call_update` always arrived before the `session/prompt` response, cancel included.
- **Sub-agent (`task` tool): the one real failure, 3/3 runs.** The main session shows `tool_call` `task` `in_progress` and then nothing. The sub-agent runs in a **child session** (db `session.parent_id` = our session, title "Run sleep 3 command (@general subagent)", its own `permission: [{"permission":"task","pattern":"*","action":"deny"}]`). Its `bash` call waited for approval, but **its permission request was never sent over ACP** (no `session/request_permission`, no `session/update` with the child's id). It sat until the process was closed 240 s later (`state.status:"error"`, "Tool execution aborted"). The engine said Working; the truth was "needs you, and you cannot answer": **1 unexplained mismatch per run, caused by missing events.**
- **Mitigation verified:** with `"task":"deny"` in the inline policy the model's `task` call ends at once as `Invalid Tool`, the model said TASKREFUSED, `end_turn` (run `s3-taskdeny`).
- `fs/write_text_file`: after an approved edit, OpenCode wrote the file itself **and** sent the client `fs/write_text_file {path, content, sessionId}` although we declared `fs.writeTextFile:false`. The spike answered `-32601`; OpenCode logged the error to stderr and carried on ("Wrote file successfully"). 3/3.
- In the steer runs the only label disagreement (S1 run 1, +13.030 s) is the 16 ms between the two answers of an absorbed steer, where prompt 5 was still open: the engine (Working) was right.

## S4 resume (1 run each path)

Process A: `session/new`, model set, "Remember this secret word: JABUTICABA. Reply only with OK." → `end_turn` → **SIGKILL** of the whole process group.

- **Process B, `session/resume {sessionId, cwd, mcpServers:[]}`:** answered in 242 ms with `configOptions` (model still `nemotron-3.5-lightning-free`, so the model is stored per session). No replay. "What is the secret word?" → `JABUTICABA`. PASS.
- **Process C, `session/load`:** **replays the history before responding**: `user_message_chunk` (A's prompt), `agent_message_chunk` "OK", `user_message_chunk` (B's question), `agent_message_chunk` "JABUTICABA", then the `session/load` response. Then the same question → `JABUTICABA`. PASS.
- B ran while another `opencode acp` held port 4096: B listened on a random port (60171). Two concurrent sessions sharing one data dir worked.

## S5 MCP (`session/new.mcpServers`)

`{name:"echo", command:<repo>/target/debug/spike-mcp-echo, args:[<log>], env:[{name:"MIRANTE_TASK",value:"t-s5"},{name:"MIRANTE_HOME",value:…}]}`

- Launched at `session/new` (before any prompt). cwd = the session cwd. argv as passed.
- Env: `MIRANTE_TASK` and `MIRANTE_HOME` as passed, **plus the whole agent environment** (`HOME`, `XDG_*`, `OPENCODE_CONFIG_CONTENT`, `OPENCODE_PID`, `OPENCODE_PURE`, `OPENCODE_CLIENT`, …).
- MCP handshake from OpenCode: `initialize` (`protocolVersion "2025-11-25"`, `clientInfo opencode 1.18.25`, `capabilities.roots`), `notifications/initialized`, `tools/list`.
- Tools appear as **`echo_echo_read` / `echo_echo_write`** (`<server>_<tool>`), `tool_call` kind `other`.
- **Default policy: both ran without asking** (readOnlyHint is ignored). With `"echo_*":"ask","echo_echo_read":"allow"` in the inline policy: `echo_echo_read` ran, `echo_echo_write` raised `session/request_permission` (options once/always/reject); rejected → `failed`.
- MCP permission requests carry **`rawInput: {}`** (no arguments) and title = the tool name, so a card cannot show what the call will do.
- After each `tools/call` result OpenCode also sent `notifications/cancelled` for the same request id (harmless).
- The inline-config `mcp` fallback was not needed.

## S6 permission safety

- **(a)** Key name is **`permission`** (singular). `"permissions"` makes 1.18.25 refuse to start: stderr "Configuration is invalid at OPENCODE_CONFIG_CONTENT ↳ V2 permissions are not supported by OpenCode V1", exit before `initialize` (fails closed). With **no** permission rules, `touch` ran **without asking** (base policy allows). Under Mirante's policy every edit, bash command, external directory and configured MCP tool asked. Options always `[{optionId:"once",kind:"allow_once"},{optionId:"always",kind:"allow_always"},{optionId:"reject",kind:"reject_once"}]` (no `reject_always`). Edit cards carry a `diff` content block and `rawInput.{diff, filepath}`; bash cards carry `rawInput.command`; external-directory cards have title = the directory, kind `other`.
- **(b)** Planted `bash: allow` in `repo/opencode.json`, `repo/opencode.jsonc`, `repo/.opencode/opencode.json`, the parent `/tmp/acp-spike/opencode.json`, and a planted pattern rule `{"bash":{"touch*":"allow"}}`: **in all 5 the inline `OPENCODE_CONFIG_CONTENT` won** (permission asked, file not created).
- **(c)** A plugin (written for this test; records its load and sets `output.status="allow"` in a `permission.ask` hook) in `$XDG_CONFIG_HOME/opencode/plugin{,s}/` and in `repo/.opencode/plugin{,s}/`: with `--pure`, **not loaded** (no log line) in both places. Without `--pure` it **loaded and ran its code at session start** from both the user dir and the repository (twice per dir). Its `permission.ask` hook never fired in 1.18.25, so the permission still asked, but repo plugins are arbitrary code at session start without any permission: `--pure` is mandatory.
- **Founder posture (decision 1):** `"edit":{"*":"allow",".git/**":"deny","opencode.json*":"deny",".opencode/**":"deny",".mirante/**":"deny",".claude/**":"deny",".codex/**":"deny"}` (object form accepted). `notes.txt` was written **without a card**. `.opencode/opencode.json`, `opencode.json`, `.git/hooks/pre-commit` and `.claude/settings.json` failed with "The user has specified a rule which prevents you from using this specific tool call". `/tmp/acp-spike/outside.txt` raised an `external_directory` card (title `/tmp/acp-spike`) and was rejected.
- **(d)** `opencode acp` **listens on TCP `127.0.0.1:4096`** (the `--port` default `0` still takes 4096 when it is free, else a random port). The ACP process is itself a client of that server: 7 loopback connections to :4096 right after `session/new`. The public server docs say the server needs no authentication unless `OPENCODE_SERVER_PASSWORD` is set (basic auth, user `opencode`) and expose `POST /session/:id/permissions/:permissionID`. With `OPENCODE_SERVER_PASSWORD` set by the client, ACP still works, but the **variable is inherited by tool processes** (the MCP echo server saw `OPENCODE_SERVER_PASSWORD` in its env names), so a password does not hide the server from tools. **Not done here:** an HTTP request to the listener, to prove that it answers without auth and from a tool (the probe was blocked by the spike runner's own permission policy; left for the founder, see FINDINGS).
- **(e)** `share: disabled`: during a prompt the only remote endpoint was `[2606:4700:10::ac42:ad95]:443` (= `opencode.ai` / `api.opencode.ai`, the Zen API), 2 connections. Nothing to `opncd.ai` (the share host), and 0 `share_url` / `session_share` rows over 32 sessions. Sampled twice, not a full capture.
- **(f)** Catalog probe (`initialize` + `session/new`, no prompt): only loopback connections (no model request); it **leaves a session row** "New session - <timestamp>" with no messages in `opencode.db`, created with the default model `big-pickle`.

## S7 models (only (a) run)

- **(a) no key:** the free Zen models work with no login and no key (`auth.json` absent in the temp home). `opencode/nemotron-3-ultra-free` was **listed at 15:43 and gone by 15:47 (local time)**. Runs used `opencode/nemotron-3.5-lightning-free` (`size` 262,144 tokens). Selection: `session/set_config_option {configId:"model", value}` (answer = the full `configOptions` with the new `currentValue`) **or** inline `"model"` (reflected as `currentValue` at `session/new`). The default is `opencode/big-pickle` whenever nothing valid is chosen.
- (b) OpenRouter, (c) Google AI Studio, (d) Ollama: **pending** (founder's keys and Ollama install).
- Terms: see FINDINGS-more-agents.md (from the plan's research; not re-verified here).

## S8 memory: pending (needs Ollama, S7d).
