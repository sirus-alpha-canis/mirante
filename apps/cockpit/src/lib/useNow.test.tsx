import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useNow } from "./useNow";

function Clock({ period }: { period: number }) {
  return <span>{useNow(period)}</span>;
}

describe("useNow", () => {
  afterEach(() => vi.restoreAllMocks());

  // A slow machine (a loaded CI runner) crosses a millisecond between two renders. With a
  // subscribe made anew on every render, each render resubscribed, the restarted clock read
  // a new Date.now(), and that re-rendered again: "Maximum update depth exceeded".
  it("settles when every read of the clock sees a later millisecond", () => {
    let t = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => ++t);
    render(<Clock period={7_001} />);
    expect(Number(screen.getByText(/^\d+$/).textContent)).toBeGreaterThan(1_000);
  });

  it("one clock per period, shared by its readers", () => {
    vi.spyOn(Date, "now").mockReturnValue(42);
    render(
      <>
        <Clock period={7_002} />
        <Clock period={7_002} />
      </>,
    );
    expect(screen.getAllByText("42")).toHaveLength(2);
  });
});
