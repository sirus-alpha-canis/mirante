import { act, fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { type DismissReason, useDismiss } from "./useDismiss";

function Harness({ onClose, enabled = true }: { onClose(r: DismissReason): void; enabled?: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  useDismiss(ref, onClose, { enabled });
  return (
    <>
      <button>outside</button>
      <p>plain text outside</p>
      <div ref={ref}>
        <button>inside</button>
        <input aria-label="field" />
      </div>
    </>
  );
}

function setup(enabled = true) {
  const onClose = vi.fn();
  render(<Harness onClose={onClose} enabled={enabled} />);
  return onClose;
}

describe("useDismiss", () => {
  it("closes on a pointerdown outside, not on one inside", () => {
    const onClose = setup();
    fireEvent.pointerDown(screen.getByRole("button", { name: "inside" }));
    expect(onClose).not.toHaveBeenCalled();
    // A non-focusable spot (WebKit moves no focus there): the pointer alone closes it.
    fireEvent.pointerDown(screen.getByText("plain text outside"));
    expect(onClose).toHaveBeenCalledWith("outside");
  });

  it("closes on Escape anywhere, and keeps the key from the app (capture)", () => {
    const onClose = setup();
    const app = vi.fn();
    window.addEventListener("keydown", app);
    const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(esc);
    });
    window.removeEventListener("keydown", app);
    expect(onClose).toHaveBeenCalledWith("escape");
    expect(esc.defaultPrevented).toBe(true);
    expect(app).not.toHaveBeenCalled();
  });

  it("other keys are left alone", () => {
    const onClose = setup();
    fireEvent.keyDown(document.body, { key: "a" });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes when the window loses focus", () => {
    const onClose = setup();
    fireEvent.blur(window);
    expect(onClose).toHaveBeenCalledWith("blur");
  });

  it("closes when focus moves to an element outside, not inside nor to nowhere", () => {
    const onClose = setup();
    const inside = screen.getByRole("button", { name: "inside" });
    const field = screen.getByLabelText("field");
    // Focus moving within stays open.
    fireEvent.focusOut(inside, { relatedTarget: field });
    // Focus to nowhere (WebKit, a click on a button that takes no focus) is the pointer's call.
    fireEvent.focusOut(field, { relatedTarget: null });
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.focusOut(field, { relatedTarget: screen.getByRole("button", { name: "outside" }) });
    expect(onClose).toHaveBeenCalledWith("focusout");
  });

  it("does nothing while disabled", () => {
    const onClose = setup(false);
    fireEvent.pointerDown(document.body);
    fireEvent.keyDown(document.body, { key: "Escape" });
    fireEvent.blur(window);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("calls the latest onClose without re-subscribing on every render", () => {
    const first = vi.fn();
    const second = vi.fn();
    function Swap() {
      const [n, setN] = useState(0);
      const ref = useRef<HTMLDivElement>(null);
      useDismiss(ref, n === 0 ? first : second, { enabled: true });
      return (
        <div ref={ref}>
          <button onClick={() => setN(1)}>swap</button>
        </div>
      );
    }
    render(<Swap />);
    fireEvent.click(screen.getByRole("button", { name: "swap" }));
    fireEvent.pointerDown(document.body);
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith("outside");
  });
});
