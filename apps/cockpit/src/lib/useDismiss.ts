import { type RefObject, useEffect, useRef } from "react";

/** Why an open popover closed: the caller decides where focus goes (back to the trigger after
 * Escape; nowhere new after a click elsewhere, which already put it where the person meant). */
export type DismissReason = "outside" | "escape" | "blur" | "focusout";

/**
 * Closes an open popover or menu the way people expect, the same everywhere in the cockpit.
 * `ref` wraps the trigger and what it opened (a press on the trigger is not "outside", so the
 * trigger's own click can toggle it closed). While `enabled`:
 *
 * - a pointerdown outside closes it (pointerdown, not click: nothing can open between the
 *   press and the release, and WebKit moves no focus on a click on a button or plain text);
 * - Escape anywhere closes it first (window, capture phase), before the app's own Escape;
 * - the window losing focus (another app, another window) closes it;
 * - focus moving to an element outside closes it. Focus going nowhere (`relatedTarget` null)
 *   does not: WebKit drops focus on a click on a button inside, and a press outside is the
 *   pointer's call above.
 */
export function useDismiss(ref: RefObject<HTMLElement | null>, onClose: (reason: DismissReason) => void, { enabled }: { enabled: boolean }): void {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    if (!enabled) return;
    const inside = (n: EventTarget | null) => n instanceof Node && !!ref.current?.contains(n);
    const onPointer = (e: PointerEvent) => {
      if (!inside(e.target)) close.current("outside");
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      close.current("escape");
    };
    // An element's blur does not bubble: a listener on the window (not capturing) hears only its own.
    const onBlur = () => close.current("blur");
    const onFocusOut = (e: FocusEvent) => {
      if (inside(e.target) && e.relatedTarget instanceof Node && !inside(e.relatedTarget)) close.current("focusout");
    };
    document.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    document.addEventListener("focusout", onFocusOut, true);
    return () => {
      document.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("focusout", onFocusOut, true);
    };
  }, [enabled, ref]);
}
