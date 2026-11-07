# Task 9 raw outputs (spike-claude-stream + S2)

Run on 2026-11-07. Claude Code 2.1.285 (`/opt/homebrew/bin/claude` via `MIRANTE_CLAUDE_BIN`, skipping the cmux shim). Model Opus 5.5 (`claude-opus-5-5`).
Every run used `--permission-mode manual`, an explicit allowlist, and a fresh marker-guarded sandbox from `scripts/make-sandbox.sh`. The env was scrubbed with `util::env_to_scrub()`: the 13 nested-session vars plus every `CMUX_CLAUDE_*` and `CMUX_AGENT_*` var. Nothing ran with bypassed permissions.

Note: neither mode used `--bare`. Both modes load the user's `~/.claude` setup (CLAUDE.md, skills, plugins, MCP servers, a SessionStart hook). Those add about 18–20k tokens of context per process. Mirante's own setup would be smaller.

## S1 headless (stream-json in/out, delay 6s)

Command:
```
SANDBOX=$(scripts/make-sandbox.sh ~/.mirante-spike/stream-sandbox)
MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude ./target/debug/spike-claude-stream "$SANDBOX" ~/.mirante-spike/stream.jsonl 6
```
Flags passed: `-p --input-format stream-json --output-format stream-json --verbose --permission-mode manual --allowedTools Bash(sleep:*) Bash(echo:*)`

### Run 1 (before the classifier fix)

```
results seen: 1
mid-run delivery (stream-json): Steered
session id: efc3fc0e-c81b-405f-b382-ed80211f310c
transcript: ~/.claude/projects/-Users-pedrohenriquecandidoferreira--mirante-spike-stream-sandbox/efc3fc0e-c81b-405f-b382-ed80211f310c.jsonl
mid-run delivery (transcript): Inconclusive
```

Why the transcript result was `Inconclusive`: in headless mode the prompt line is `{"type":"user","promptSource":"sdk"}` and has no `origin.kind:"human"`. So `classify_transcript` never saw a turn start.

The rest of the transcript matches the interactive case exactly:
- `queue-operation enqueue` after the first `tool_use`;
- an attachment `queued_command` (the BANANA text);
- `queue-operation remove`, `reason:"absorbed_mid_turn"`;
- the assistant answers `end_turn`.

Fix: `is_prompt` now also accepts `promptSource == "sdk"`, with a test built from this real line order. After the fix, `spike classify-transcript <run-1 transcript>` prints `Steered`.

Stream timeline, run 1 (ms since the first input line):
```
+     0ms in  user             Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma
+   492ms out system           hook_started
+   747ms out system           hook_response
+   779ms out system           commands_changed
+   792ms out system           init
+  2150ms out system           commands_changed
+  2251ms out assistant        tool_use:sleep 3 && echo passo-1
+  6007ms in  user             Mudança de planos: pare o que está fazendo e responda apenas
+  6046ms out system           task_started
+  6055ms out system           task_notification
+  6063ms out user             tool_result:passo-1
+  7763ms out system           thinking_tokens
+  8251ms out system           thinking_tokens
+  8253ms out assistant        thinking
+  8256ms out assistant        text:BANANA
+  8270ms out result           {"subtype": "success", "is_error": false, "num_turns": 2, "result": "BANANA", "total_cost_usd": 0.1776072, "duration_ms": 7499}
```

### Run 2 (after the fix: a replication)

```
results seen: 1
mid-run delivery (stream-json): Steered
session id: 6faf4797-1951-435c-8bac-e92292be0a76
transcript: ~/.claude/projects/-Users-pedrohenriquecandidoferreira--mirante-spike-stream-sandbox/6faf4797-1951-435c-8bac-e92292be0a76.jsonl
mid-run delivery (transcript): Steered
```

Stream timeline, run 2:
```
+     0ms in  user             Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma
+   350ms out system           hook_started
+   612ms out system           hook_response
+   643ms out system           init
+  2007ms out system           commands_changed
+  2941ms out assistant        tool_use:sleep 3 && echo passo-1
+  2954ms out rate_limit_event {"isUsingOverage": false, "overageDisabledReason": "out_of_credits", "overageStatus": "rejected", "rateLimitType": "five_hour", "resetsAt": 1794079200, "status": "allowed", "unifiedWindows": {"five_hour": {"resetsAt": 1794079200, "utilization": 0.36}, "seven_day": {"resetsAt": 1794394800, "utilization": 0.4}}}
+  6009ms in  user             Mudança de planos: pare o que está fazendo e responda apenas
+  6718ms out system           task_started
+  6736ms out system           task_notification
+  6744ms out user             tool_result:passo-1
+  8425ms out system           thinking_tokens
+  8569ms out system           thinking_tokens
+  8571ms out assistant        thinking
+  8574ms out assistant        text:BANANA
+  8595ms out result           {"subtype": "success", "is_error": false, "num_turns": 2, "result": "BANANA", "total_cost_usd": 0.1642938, "duration_ms": 7969}
```

### What happened to BANANA

In both runs Claude stopped mid-sequence and never reached FIM:
- The first message started `sleep 3 && echo passo-1`.
- The second message (sent at +6.0s) was taken while that Bash call was still running.
- Claude ran no further steps: no passo-2..5 and no FIM. Its next and last output was `BANANA`.
- There was exactly ONE `result` (`num_turns: 2`, `result: "BANANA"`, `queued_turn_count: 0`). The 2nd message was absorbed into the running turn; it did not start a separate turn.

The driver waits 60s after the first result in case a second one arrives. None came.

## S2 (quota): the same 3 prompts in both modes

Prompts:
1. "Liste os arquivos e diga quantos são."
2. "Crie hello.txt com o texto oi."
3. "Execute `sleep 1 && echo ok` e diga o resultado."

How each mode ran (fresh sandbox each, both ended with `hello.txt` = `oi`):
- **Interactive:** `spike-claude-pty <sandbox> ~/.mirante-spike/settings-s2.json ~/.mirante-spike/hooks.jsonl 0 prompts`. This is the new `prompts` mode: it types each prompt, waits for its Stop hook, then sends `/exit`. `settings-s2.json` holds the Task 8 hooks plus the allowlist `Bash(sleep:*)`, `Bash(echo:*)`, `Write`, `Bash(ls:*)`, the same as headless.
- **Headless:** one `claude -p "<prompt>" --output-format stream-json --verbose --permission-mode manual --allowedTools "Bash(sleep:*)" "Bash(echo:*)" "Write" "Bash(ls:*)"` per prompt, stdin `/dev/null`, scrubbed env. The prompt goes before the variadic `--allowedTools`; the plan's order (prompt last) would let `--allowedTools` swallow it.

Tokens come from `spike usage <transcript> <session-id>`. For headless, the transcript sums equal the `result.usage` fields exactly.

| mode | session | API calls | input | output | cache_creation | cache_read | total |
|---|---|---|---|---|---|---|---|
| interactive | dcd0c9a7-1802-4873-aed8-43e3530b5cd2 (1 session, 3 prompts) | 6 | 14 | 677 | 22,834 | 253,957 | **277,482** |
| headless | f7f6ff74-29cb-4bfa-b323-c4d4d92e5c6f (prompt 1) | 2 | 4 | 396 | 20,222 | 41,356 | 61,978 |
| headless | 4ed4793e-b8df-4df6-8df0-8ef0a5a56e25 (prompt 2) | 2 | 4 | 146 | 18,270 | 42,989 | 61,409 |
| headless | 592ec5d2-97d8-44fd-b95f-88d85a176b0d (prompt 3) | 2 | 4 | 170 | 18,266 | 43,006 | 61,446 |
| headless | **sum of the 3** | 6 | 12 | 712 | 56,758 | 127,351 | **184,833** |

Cache reads dominate both modes: 91.5% of the interactive total and 68.9% of the headless total.

The two modes spend differently:
- **Headless** writes the cache again for every process: about 18–20k tokens of cache_creation per `-p` call, even though the cache is `ephemeral_1h`.
- **Interactive** writes the cache once but reads a larger context on every call. It averages about 42k cache_read per call; headless averages about 21k.
- Raw token totals are not the plan's meter. Cache reads are usually weighted far below writes, so this table alone cannot say which mode costs more quota.

Headless `result` events:

| prompt | total_cost_usd | permission_denials |
|---|---|---|
| 1 | 0.1779832 | [] |
| 2 | 0.1576938 | [] |
| 3 | 0.1581452 | [] |
| sum | 0.4938222 | |

These are client-side, list-price estimates. `modelUsage.costBasis` says `"list"`, and the docs call them "client-side estimates". On a subscription nothing is billed per token.

The result event also reports `modelUsage[model].thinkingTokens`, `usage.iterations[]`, `usage.cache_creation.ephemeral_1h_input_tokens`, `service_tier`, `queued_turn_count` and `subagent_stats`.

### Usage fields in the stream: `rate_limit_event`

With `--output-format stream-json --verbose`, every headless run emitted one `rate_limit_event`. It holds the subscription's plan-window utilization:
```
{"status": "allowed", "resetsAt": 1794079200, "rateLimitType": "five_hour", "overageStatus": "rejected", "overageDisabledReason": "out_of_credits", "isUsingOverage": false, "unifiedWindows": {"five_hour": {"utilization": 0.36, "resetsAt": 1794079200}, "seven_day": {"utilization": 0.4, "resetsAt": 1794394800}}}
```

Over the whole session, five_hour utilization went from 0.35 (S1 run 1, first) to 0.36 (every later run). seven_day stayed at 0.4.

That 2-decimal resolution is too coarse to show a per-mode delta for 3 small prompts. Even so, it is a machine-readable plan meter that a headless driver gets for free. The `/usage` meter was not read (that needs the founder).

## Terms check (fetched 2026-11-07)

1. **https://code.claude.com/docs/en/headless** ("Run Claude Code programmatically")
   - "The [Agent SDK] gives you the same tools, agent loop, and context management that power Claude Code. It's available as a CLI for scripts and CI/CD, or as Python and TypeScript packages for full programmatic control."
   - "Set `ANTHROPIC_API_KEY` before running it, because bare mode doesn't use your subscription login". By implication, a non-bare `-p` run does use the subscription login.
   - "`--bare` is the recommended mode for scripted and SDK calls, and will become the default for `-p` in a future release." **Risk:** when bare becomes the `-p` default, `-p` will stop using the subscription login unless `--bare` is turned off.
   - "With `--output-format json`, the response payload includes `total_cost_usd` and a per-model cost breakdown … Both figures are client-side estimates and can differ from your actual bill."

2. **https://code.claude.com/docs/en/legal-and-compliance**
   - "Consumer Terms of Service - for Free, Pro, Max, and Ultra users"
   - "Advertised usage limits for Pro, Max, and Ultra plans assume ordinary, individual usage of Claude Code and the Agent SDK."
   - "OAuth authentication is intended exclusively for purchasers of Claude Free, Pro, Max, Ultra, Team, and Enterprise subscription plans and is designed to support ordinary use of Claude Code and other native Anthropic applications."
   - "Developers building products or services that interact with Claude's capabilities, including those using the Agent SDK, should use API key authentication through Claude Console or a supported cloud provider. Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, Max, or Ultra plan credentials on behalf of their users. Moreover, developers may not collect, store, or intermediate Claude.ai credentials or session tokens — sign-in to a Claude account must complete through Anthropic's own flow."
   - "Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription, including where a platform hosts Claude Code as described under *Can customers offer Claude Code in their products?* above."
   - The "offer Claude Code in their products" conditions: "The Claude Code binary must not be modified." and "Each end user must authenticate with their own Anthropic API key, Claude subscription plan credentials, or 3P inference provider credential". Offering it "requires agreeing to our Commercial Terms of Service".

3. **https://www.anthropic.com/legal/consumer-terms** (effective October 8, 2025), §3 item 7. Users may not:
   - "Except when you are accessing our Services via an Anthropic API Key or where we otherwise explicitly permit it, to access the Services through automated or non-human means, whether through a bot, script, or otherwise."

**Reading: partly inconclusive.**

What the terms allow:
- The Claude Code docs present `claude -p` with subscription login as a supported, documented feature ("for scripts and CI/CD"). That plausibly counts as "where we otherwise explicitly permit it".
- A local app that runs the **unmodified** binary, signed in through Anthropic's own flow with the user's **own** subscription, matches the carve-out the legal page states.

What they forbid:
- The app must never handle or store credentials.
- It must not route other people's usage through one subscription.

What stays open:
- The limits assume "ordinary, individual usage". Heavy automated orchestration (many parallel `-p` agents) may fall outside that.
- No page says outright that unattended, programmatic `-p` on a Pro/Max plan is allowed.
- This needs founder/legal confirmation, or a question to Anthropic sales.
