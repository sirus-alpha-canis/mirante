import { describe, expect, it } from "vitest";
import { makeEnv, rpcError } from "../test/env";
import {
  dnaErrorText,
  dnaGet,
  dnaStatus,
  linkParts,
  noteAction,
  proposals,
  review,
  shortCommit,
  stateLabel,
  taskContext,
} from "./dna";
import { DaemonError } from "./daemon";
import type { NoteSummary } from "./protocol";
import { RpcCode } from "./protocol";

describe("dna helpers", () => {
  it("splits [[links]] into text and link parts without HTML", () => {
    expect(linkParts("See [[hook-spool|the spool]] and [[x#h]].")).toEqual([
      { text: "See " },
      { link: "hook-spool", label: "the spool" },
      { text: " and " },
      { link: "x", label: "x#h" },
      { text: "." },
    ]);
  });

  it("keeps markup as text and leaves unclosed or empty brackets alone", () => {
    expect(linkParts("<img src=x onerror=alert(1)> [[São Paulo]]")).toEqual([
      { text: "<img src=x onerror=alert(1)> " },
      { link: "sao-paulo", label: "São Paulo" },
    ]);
    expect(linkParts("a [[ ]] b [[open")).toEqual([{ text: "a [[ ]] b [[open" }]);
    expect(linkParts("")).toEqual([]);
  });

  it("labels states for people", () => {
    expect(stateLabel({ state: "stale", reviewed: true } as NoteSummary)).toBe("stale: cited code changed");
    expect(stateLabel({ state: "fresh", reviewed: false } as NoteSummary)).toBe("unreviewed");
    expect(stateLabel({ state: "fresh", reviewed: true } as NoteSummary)).toBe("fresh");
    expect(stateLabel({ state: "unverifiable", reviewed: true } as NoteSummary)).toBe("unverifiable: cited commit not found");
    expect(stateLabel({ state: "fresh", reviewed: true, status: "retired" } as NoteSummary)).toBe("retired");
    expect(shortCommit("50ea8cc255941e739b13cdbe2c937abca0319834")).toBe("50ea8cc");
  });

  it("treats METHOD_NOT_FOUND as an older daemon", async () => {
    const env = makeEnv({ "dna.status": rpcError(RpcCode.METHOD_NOT_FOUND) });
    expect(await dnaStatus(env.env.rpc, "/r")).toEqual({ available: false });
    const ok = makeEnv({ "dna.status": { repo: "/r", enabled: true, pending: 2 } });
    expect(await dnaStatus(ok.env.rpc, "/r")).toMatchObject({ available: true, status: { pending: 2 } });
    const down = makeEnv({ "dna.status": rpcError(RpcCode.INVALID_PARAMS, "not a git repo") });
    await expect(dnaStatus(down.env.rpc, "/r")).rejects.toThrow("not a git repo");
  });

  it("sends the wire params the daemon expects", async () => {
    const env = makeEnv({});
    await taskContext(env.env.rpc, "t1");
    await dnaGet(env.env.rpc, { repo: "/r" }, "a");
    await proposals(env.env.rpc, { repo: "/r" });
    await review(env.env.rpc, { id: "p", accept: true, title: "T" });
    await noteAction(env.env.rpc, "/r", "a", "confirm", "h1");
    expect(env.calls).toEqual([
      ["dna.task_context", { task: "t1" }],
      ["dna.get", { repo: "/r", slug: "a" }],
      ["dna.proposals", { repo: "/r" }],
      ["dna.review", { id: "p", accept: true, title: "T" }],
      ["dna.note_action", { repo: "/r", slug: "a", action: "confirm", expect_hash: "h1" }],
    ]);
  });

  it("explains refusals in a person's words", () => {
    const err = (code: number, message: string) => new DaemonError(rpcError(code, message));
    expect(dnaErrorText(err(RpcCode.FORBIDDEN, "x"))).toMatch(/cannot be made from an agent session/);
    expect(dnaErrorText(err(RpcCode.CONFLICT, "the note changed on disk since it was read; reload and try again"))).toBe(
      "The note changed since it was shown. Reload it and try again.",
    );
    expect(dnaErrorText(err(RpcCode.CONFLICT, "anchor 1 is stale: re-cite it or retire the note"))).toBe(
      "anchor 1 is stale: re-cite it or retire the note",
    );
  });
});
