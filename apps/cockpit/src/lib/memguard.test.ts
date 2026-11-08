import { describe, expect, it } from "vitest";
import { pressureText, systemMemory } from "./memguard";

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

describe("pressureText", () => {
  const GB = 2 ** 30;
  const sys = (over: Record<string, unknown>) =>
    systemMemory({
      ...status,
      memory: { level: "elevated", total: 16 * GB, free: 894 * 2 ** 20, swap_used: 92, swap_total: 100, paused: true, ...over },
    })!;

  it("paused by macOS's own pressure measure: says so, with swap; free memory is secondary", () => {
    const t = pressureText(sys({ kernel_level: "warn", kernel_source: "macos" }));
    expect(t.main).toBe("macOS reported memory pressure · swap 92% · new sessions paused until it eases");
    expect(t.secondary).toBe("894 MB free of 16.0 GB");
    expect(pressureText(sys({ level: "critical", kernel_level: "critical", kernel_source: "macos" })).main).toBe(
      "macOS reported critical memory pressure · swap 92% · new sessions paused until it eases",
    );
    expect(pressureText(sys({ kernel_level: "warn", kernel_source: "linux-psi" })).main).toBe(
      "Linux (PSI) reported memory pressure · swap 92% · new sessions paused until it eases",
    );
  });

  it("still paused after the kernel is back to normal: easing, not invented", () => {
    expect(pressureText(sys({ kernel_level: "normal", kernel_source: "macos" })).main).toBe(
      "macOS reports normal memory pressure again · swap 92% · new sessions paused until it settles",
    );
  });

  it("judged from swap and free memory (no kernel measure): names those", () => {
    expect(pressureText(sys({})).main).toBe("Memory tight (swap 92% used, little free) · new sessions paused until it eases");
  });

  it("not paused: the level, swap only when there is swap", () => {
    expect(pressureText(sys({ level: "normal", paused: false, kernel_level: "normal", kernel_source: "macos" })).main).toBe(
      "Memory pressure normal (macOS) · swap 92%",
    );
    expect(pressureText(sys({ level: "normal", paused: false, swap_used: 0, swap_total: 0 })).main).toBe("Memory pressure normal");
  });
});
