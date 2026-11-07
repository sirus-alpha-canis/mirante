# Task 10 raw outputs (spike-codex: S1 Codex + S4 Codex)

Run on 2026-11-07. Codex CLI 0.167.0, logged in with ChatGPT (`account/updated` → `authMode: chatgpt`, `planType: plus`). Model reported by the server: `gpt-6.1-sol`.
The binary was `~/.nvm/versions/node/v22.14.0/bin/codex`, set through `MIRANTE_CODEX_BIN`. That skips the cmux shim that PATH `codex` resolves to (`~/.cmuxterm/cmux-cli-shims/<id>/codex`).
The env was scrubbed with `util::codex_env_to_scrub()`: the 13 Claude nested-session vars plus every `CMUX_*` var. No `CODEX_*` / `OPENAI_*` var was set or scrubbed. Auth lives in `~/.codex/auth.json`.
Every run used a fresh marker-guarded sandbox from `scripts/make-sandbox.sh`. No "dangerously bypass" flag was used.

## Step 1: schema and real protocol names

```
codex app-server generate-json-schema --out ~/.mirante-spike/codex-schema
```
39 files were written: `ClientRequest.json`, `ServerNotification.json`, `ServerRequest.json`, `v2/*Params.json`, and others. The names the plan guessed are all correct.

| Purpose | Real name | Notes |
|---|---|---|
| handshake | `initialize` (request) + `initialized` (client notification) | `initialize.params.clientInfo` is required |
| start a thread | `thread/start` | params: `cwd`, `approvalPolicy`, `sandbox`, `model`, `ephemeral`, `developerInstructions`, … (all optional). Response `result.thread.id`; notification `thread/started` → `params.thread.id` |
| start a turn | `turn/start` | required `threadId`, `input: UserInput[]` (`{"type":"text","text":…}`). Response `result.turn.id`. Per-turn overrides: `approvalPolicy`, `sandboxPolicy`, `cwd`, `model`, `effort` |
| steer the running turn | `turn/steer` | required `threadId`, `expectedTurnId` ("fails when it does not match the currently active turn"), `input`. Response `result.turnId` |
| interrupt | `turn/interrupt` | required `threadId`, `turnId` |
| resume | `thread/resume` | required `threadId`; optional `cwd`, `approvalPolicy`, `sandbox`, `excludeTurns`, … |
| other thread ops | `thread/fork`, `thread/read`, `thread/turns/list`, `thread/items/list`, `thread/inject_items`, `thread/list`, `thread/archive`, … | not exercised |
| turn lifecycle | `turn/started`, `turn/completed` (`params.turn.{id,status,error}`) | plus `thread/status/changed` (`active` / `idle`) |
| items | `item/started`, `item/completed` (`params.item`), deltas such as `item/agentMessage/delta` and `item/commandExecution/outputDelta` | `agentMessage` = `{id, text, phase: "commentary" or "final_answer", delivery, …}`; `userMessage` = `{id, content[]}`; `commandExecution` = `{command, status, exitCode, aggregatedOutput, durationMs}` |
| approvals (server → client requests) | `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput` (and legacy `execCommandApproval`, `applyPatchApproval`) | none were raised in these runs |
| `approvalPolicy` | `"untrusted"`, `"on-request"`, `"never"`, or `{"granular":{…}}` | |
| `sandbox` (thread) | `"read-only"`, `"workspace-write"`, `"danger-full-access"` | per-turn `sandboxPolicy`: `{"type":"workspaceWrite","networkAccess":false,"writableRoots":[]}` and others; `networkAccess` defaults to false |

The id pointers in `codex::THREAD_POINTERS` / `TURN_POINTERS` already match the schema (`/line/result/thread/id`, `/line/params/thread/id`, `/line/result/turn/id`, `/line/params/turn/id`, `/line/result/turnId`), so `codex.rs` is unchanged.

## Step 2: final script (`scripts/codex-steer.jsonl`), unchanged from the plan

```
{"delay_ms":0,"msg":{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"mirante-spike","version":"0.1.0"}}}}
{"delay_ms":500,"msg":{"jsonrpc":"2.0","method":"initialized"}}
{"delay_ms":500,"msg":{"jsonrpc":"2.0","id":2,"method":"thread/start","params":{"cwd":"$CWD","approvalPolicy":"never","sandbox":"workspace-write"}}}
{"delay_ms":2000,"msg":{"jsonrpc":"2.0","id":3,"method":"turn/start","params":{"threadId":"$THREAD","input":[{"type":"text","text":"Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM."}]}}}
{"delay_ms":8000,"msg":{"jsonrpc":"2.0","id":4,"method":"turn/steer","params":{"threadId":"$THREAD","expectedTurnId":"$TURN","input":[{"type":"text","text":"Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA."}]}}}
```

The `approvalPolicy: "never"` + `sandbox: "workspace-write"` setting is **spike-only**. With `never`, a command that the sandbox blocks fails and is not escalated to an approval prompt, so nothing can run outside the throwaway sandbox repo. Network is off by default in `workspace-write`. The task ran only `sleep`/`echo`. The product never auto-approves.

## S1 Codex: steering, 2 runs, first attempt (no script fixes needed)

Command:
```
SB=$(scripts/make-sandbox.sh "$HOME/.mirante-spike/codex-sandbox")
MIRANTE_CODEX_BIN=~/.nvm/versions/node/v22.14.0/bin/codex \
  ./target/debug/spike-codex "$SB" scripts/codex-steer.jsonl ~/.mirante-spike/codex.jsonl 60
```

### Run 1: verbatim output
```
thread id: 01a1b72b-b899-79a3-bdb9-ee4a2b4d8223
turn/completed seen: 1
agentMessage @1794074463498: "Vou executar as cinco chamadas em sequência."
agentMessage @1794074471500: "BANANA"
mid-run delivery (codex): Steered
```
Timeline (seconds from `initialize`, noise filtered):
```
  3.02 in  turn/start
  3.06 out turn/started            id 01a1b72b-c059-… inProgress
  6.27 out item/completed agentMessage "Vou executar as cinco chamadas em sequência." (phase commentary)
  8.11 out item/started   commandExecution  sleep 3 && echo passo-1
 10.99 out item/completed commandExecution  passo-1, exit 0
 11.02 in  turn/steer      expectedTurnId 01a1b72b-c059-…
 11.02 out result {"turnId":"01a1b72b-c059-…"}        <- same turn, accepted
 12.48 out item/completed userMessage "Mudança de planos: … BANANA."
 14.27 out item/completed agentMessage "BANANA" (phase final_answer)
 14.29 out turn/completed  status completed            <- only one turn
```
Codex stopped after step 1. `passo-2`…`passo-5` never ran, and it never said FIM.

### Run 2: verbatim output
```
thread id: 01a1b72e-244f-7743-a7b4-841da53db758
turn/completed seen: 1
agentMessage @1794074621561: "Vou executar as cinco chamadas em sequência."
agentMessage @1794074628702: "BANANA"
mid-run delivery (codex): Steered
```
Timeline:
```
  6.29 out item/started   commandExecution passo-1
  9.18 out item/completed commandExecution passo-1, exit 0
 11.03 in  turn/steer
 11.04 out result {"turnId":"01a1b72e-2c27-…"}
 11.07 out item/completed userMessage (the steer)
 11.22 out item/started   commandExecution passo-2     <- already issued before the steer was seen
 12.80 out item/completed agentMessage "BANANA"
 12.85 out thread/status/changed idle
 12.85 out turn/completed status completed
 14.12 out item/completed commandExecution passo-2, exit 0   <- AFTER turn/completed
```
Codex stopped after step 2. The `passo-2` call was already in flight when the steer landed. The model answered BANANA and closed the turn **while `sleep 3` was still running**. That command's `item/completed` arrived 1.3 s after `turn/completed`. `passo-3`…`passo-5` never ran.

## S4 Codex: resume across app-server processes, PASS

Scripts:
- `scripts/codex-resume-1.jsonl`: `initialize` → `initialized` → `thread/start` (`approvalPolicy: never`, `sandbox: read-only`) → `turn/start` "Memorize: a palavra secreta é JABUTICABA. Responda apenas OK."
- `scripts/codex-resume-2.jsonl`: `initialize` → `initialized` → `thread/resume {threadId:$THREAD, cwd, approvalPolicy: never, sandbox: read-only, excludeTurns: true}` → `turn/start` "Qual é a palavra secreta que eu pedi para você memorizar? Responda apenas com ela."

`spike-codex` takes an optional 5th argument, a preset thread id, which fills `$THREAD`. Between the two invocations the first app-server was reaped (stdin closed → exit), and `pgrep -fl "codex app-server"` returned nothing.

```
SB=$(scripts/make-sandbox.sh "$HOME/.mirante-spike/codex-resume-sandbox")
spike-codex "$SB" scripts/codex-resume-1.jsonl ~/.mirante-spike/codex-resume-1.jsonl 25
  thread id: 01a1b72d-149d-7a23-a4e7-551d525bc2f9
  turn/completed seen: 1
  agentMessage @1794074551845: "OK"
  mid-run delivery (codex): Never          (expected: no BANANA in this script)
# app-server process gone
spike-codex "$SB" scripts/codex-resume-2.jsonl ~/.mirante-spike/codex-resume-2.jsonl 25 01a1b72d-149d-7a23-a4e7-551d525bc2f9
  thread id: 01a1b72d-149d-7a23-a4e7-551d525bc2f9
  turn/completed seen: 1
  agentMessage @1794074582070: "JABUTICABA"
  mid-run delivery (codex): Never          (expected)
```
`thread/resume` returned a normal result for the same thread id, with no JSON-RPC error. The new process recalled the secret, so S4 Codex is a **PASS**.

Raw logs (not committed): `~/.mirante-spike/codex-run1.jsonl`, `~/.mirante-spike/codex-run2.jsonl` (= `codex.jsonl`), `~/.mirante-spike/codex-resume-{1,2}.jsonl`.
