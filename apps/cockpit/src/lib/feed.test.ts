import { describe, expect, it } from "vitest";
import type { FeedItem } from "../store/state";
import { ev } from "../test/fixtures";
import { feedItem, feedText } from "../store/state";
import { collapseRepeats, describeRun, lastAnswer, plainStep, summarizeSteps } from "./feed";
import { setLang } from "./i18n";

let seq = 0;
const line = (kind: FeedItem["kind"], text: string, over: Partial<FeedItem> = {}): FeedItem => ({ seq: ++seq, ts: seq, kind, text, ...over });
const tool = (name: string, what: string) => line("tool", `${name} ${what}`, { ok: true });

describe("collapseRepeats", () => {
  it("folds consecutive identical lines into one with a count; a different line in between breaks the run", () => {
    const hib = () => line("system", "hibernate suggested: memory pressure elevated");
    const out = collapseRepeats([hib(), hib(), hib(), line("turn", "turn ended"), hib(), tool("Read", "a.ts")]);
    expect(out.map((r) => [r.item.text, r.count])).toEqual([
      ["hibernate suggested: memory pressure elevated", 3],
      ["turn ended", 1],
      ["hibernate suggested: memory pressure elevated", 1],
      ["Read a.ts", 1],
    ]);
    // The newest line of a run is kept (its seq keys the row).
    const a = hib();
    const b = hib();
    expect(collapseRepeats([a, b])[0].item).toBe(b);
    expect(collapseRepeats([])).toEqual([]);
  });

  it("the same text from another kind, or a failed and an ok line, are not the same line", () => {
    const out = collapseRepeats([line("system", "x"), line("message", "x"), tool("Bash", "ls"), { ...tool("Bash", "ls"), ok: false }]);
    expect(out.map((r) => r.count)).toEqual([1, 1, 1, 1]);
  });
});

describe("summarizeSteps", () => {
  it("a run of tool steps becomes one summary in plain words, expandable to its lines", () => {
    const reads = Array.from({ length: 14 }, (_, i) => tool("Read", `src/f${i}.ts`));
    const items = [line("message", "do it", { mode: "steer" }), ...reads, line("command", "$ cargo test", { ok: true }), tool("Bash", "ls"), tool("Bash", "pwd"), line("agent_message", "done")];
    const out = summarizeSteps(items);
    expect(out.map((e) => e.type)).toEqual(["line", "run", "line"]);
    const run = out[1];
    if (run.type !== "run") throw new Error("not a run");
    expect(run.text).toBe("read 14 files, ran 3 commands");
    expect(run.items).toHaveLength(17);
  });

  it("names edits, searches and other tools; a single step stays a line", () => {
    const out = summarizeSteps([tool("Edit", "a.ts"), tool("Write", "b.ts"), tool("Edit", "a.ts"), tool("Grep", "TODO"), tool("Glob", "*.ts"), tool("WebFetch", "x")]);
    const run = out[0];
    if (run.type !== "run") throw new Error("not a run");
    expect(run.text).toBe("edited 2 files, searched 2 times, used 1 tool");
    expect(summarizeSteps([tool("Read", "a.ts")]).map((e) => e.type)).toEqual(["line"]);
    expect(summarizeSteps([tool("Read", "a.ts"), tool("Read", "b.ts")])[0]).toMatchObject({ type: "run", text: "read 2 files" });
    expect(summarizeSteps([tool("Read", "a.ts"), tool("Read", "a.ts")])[0]).toMatchObject({ type: "run", text: "read 1 file" });
  });

  it("a failed step is counted and said", () => {
    const run = summarizeSteps([tool("Read", "a"), { ...tool("Bash", "x"), ok: false }])[0];
    expect(run).toMatchObject({ type: "run", text: "read 1 file, ran 1 command (1 failed)" });
  });

  it("identical system lines still fold with ×N", () => {
    const hib = () => line("system", "hibernate suggested");
    const out = summarizeSteps([hib(), hib()]);
    expect(out).toEqual([{ type: "line", item: expect.objectContaining({ text: "hibernate suggested" }), count: 2 }]);
  });
});

describe("plainStep", () => {
  it("says what the agent is doing without raw commands", () => {
    expect(plainStep(tool("Read", "/w/src/app/login.ts"))).toBe("reading login.ts");
    expect(plainStep(tool("Edit", "src/a.rs"))).toBe("editing a.rs");
    expect(plainStep(tool("Bash", "rm -rf node_modules && npm ci"))).toBe("running a command");
    expect(plainStep(line("command", "$ cargo test"))).toBe("running a command");
    expect(plainStep(tool("Grep", "TODO"))).toBe("searching the code");
    expect(plainStep(line("agent_message", "Here is what I found"))).toBe("writing");
    expect(plainStep(tool("Task", "explore"))).toBe("using Task");
    expect(plainStep(line("turn", "turn started", { phase: "started" }))).toBe("started a turn");
    expect(plainStep(undefined)).toBe("starting");
  });
});

describe("lastAnswer", () => {
  it("is the newest agent message, or nothing", () => {
    expect(lastAnswer([line("agent_message", "first"), tool("Read", "a"), line("agent_message", "the answer"), line("turn", "turn ended")])).toBe("the answer");
    expect(lastAnswer([tool("Read", "a")])).toBeNull();
    expect(lastAnswer(undefined)).toBeNull();
  });
});

describe("language", () => {
  it("a feed line built in English reads in Portuguese once the language switches; agent text stays", () => {
    const ended = feedItem(ev(1, "t", "turn_ended"))!;
    const asked = feedItem(ev(2, "t", "permission_requested", { tool: "Bash", detail: "ls" }))!;
    const said = feedItem(ev(3, "t", "step", { type: "agent_message", text: "All done" }))!;
    expect([feedText(ended), feedText(asked), feedText(said)]).toEqual(["turn ended", "asks: Bash ls", "All done"]);
    setLang("pt");
    expect([feedText(ended), feedText(asked), feedText(said)]).toEqual(["turno terminou", "pede: Bash ls", "All done"]);
    expect(describeRun([tool("Read", "a"), tool("Read", "b"), line("command", "$ ls", { ok: true })])).toBe("leu 2 arquivos, rodou 1 comando");
    expect(plainStep(tool("Read", "src/a.ts"))).toBe("lendo a.ts");
  });
});
