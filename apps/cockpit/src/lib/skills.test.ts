import { describe, expect, it } from "vitest";
import { makeEnv, rpcError } from "../test/env";
import { DaemonError } from "./daemon";
import { RpcCode } from "./protocol";
import {
  activationLabel,
  activationShort,
  hasLintErrors,
  scopeKind,
  scopeLabel,
  skillBody,
  skillErrorText,
  skillTenants,
  taskSkills,
} from "./skills";

describe("skills helpers", () => {
  it("labels scopes for a person", () => {
    expect(scopeLabel("personal")).toBe("Personal (every repo)");
    expect(scopeLabel("org:github.com/acme")).toBe("Org github.com/acme");
    expect(scopeLabel("repo:github.com/acme/api")).toBe("Repo github.com/acme/api");
    expect(scopeKind("org:github.com/acme")).toBe("org");
    expect(scopeKind("weird")).toBe("personal");
  });

  it("says where an always-on skill goes", () => {
    expect(activationLabel("always")).toMatch(/system prompt/);
    expect(activationLabel("on_demand")).toMatch(/when relevant/);
    expect([activationShort("always"), activationShort("on_demand")]).toEqual(["always on", "on demand"]);
  });

  it("takes the body out of a rendered SKILL.md", () => {
    expect(skillBody('---\nname: a\ndescription: "d"\n---\nBody\nmore\n')).toBe("Body\nmore\n");
    expect(skillBody("no frontmatter\n")).toBe("no frontmatter\n");
  });

  it("lint errors block, warnings do not", () => {
    const f = (severity: "error" | "warn") => ({ rule: "r", severity, file: "SKILL.md", line: 1, message: "m" });
    expect(hasLintErrors([f("warn")])).toBe(false);
    expect(hasLintErrors([f("warn"), f("error")])).toBe(true);
  });

  it("METHOD_NOT_FOUND means an older daemon without skills", async () => {
    const old = makeEnv({ "skill.tenants": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    expect(await skillTenants(old.env.rpc)).toEqual({ available: false });
    const now = makeEnv({ "skill.tenants": [{ tenant: "github.com/acme", learning: true, tasks: 2, skills: 1 }] });
    expect(await skillTenants(now.env.rpc)).toEqual({
      available: true,
      tenants: [{ tenant: "github.com/acme", learning: true, tasks: 2, skills: 1 }],
    });
    const other = makeEnv({ "skill.tenants": rpcError(RpcCode.FORBIDDEN) });
    await expect(skillTenants(other.env.rpc)).rejects.toBeInstanceOf(DaemonError);
    const t = makeEnv({ "skill.task_skills": rpcError(RpcCode.METHOD_NOT_FOUND) });
    expect(await taskSkills(t.env.rpc, "t1")).toBeNull();
    expect(t.calls).toEqual([["skill.task_skills", { task: "t1" }]]);
  });

  it("explains a refusal in a person's words", () => {
    expect(skillErrorText(new DaemonError(rpcError(RpcCode.FORBIDDEN)))).toMatch(/person's decision/);
    expect(skillErrorText(new DaemonError(rpcError(RpcCode.CONFLICT, "moved on")))).toBe("moved on");
  });
});
