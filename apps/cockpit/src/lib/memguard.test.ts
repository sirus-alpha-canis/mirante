import { describe, expect, it } from "vitest";
import { systemMemory } from "./memguard";

const status = { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 };
const mem = (level: string) => ({ level, total: 1, free: 1, swap_used: 0, swap_total: 0, paused: false });

describe("systemMemory", () => {
  it("keeps known levels and never guesses an unknown one as normal", () => {
    expect(systemMemory({ ...status, memory: mem("critical") })?.level).toBe("critical");
    expect(systemMemory({ ...status, memory: mem("normal") })?.level).toBe("normal");
    expect(systemMemory({ ...status, memory: mem("bogus") })?.level).toBe("unknown");
    expect(systemMemory(null)).toBeNull();
    expect(systemMemory(status)).toBeNull();
  });
});
