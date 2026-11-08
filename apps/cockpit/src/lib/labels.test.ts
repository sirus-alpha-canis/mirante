import { describe, expect, it } from "vitest";
import { agentModeLabel, GRANT_ROOT_TOOL, permissionScope } from "./labels";

describe("labels", () => {
  it("only Codex carries the auto-edits label", () => {
    expect(agentModeLabel("codex")?.text).toBe("auto-edits");
    expect(agentModeLabel("claude")).toBeNull();
  });

  it("grantRoot requests are session-wide, everything else once", () => {
    expect(permissionScope(GRANT_ROOT_TOOL).text).toBe("session-wide grant");
    expect(permissionScope("Bash").text).toBe("once");
  });
});
