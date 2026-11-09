// What the three composers share (the dispatch form on Home, the message box in a task's
// detail, a grid tile's line): the send arrow, the field that grows, the quiet memory hint.
import { type RefObject, useLayoutEffect } from "react";

/** The field's cap: past it, it scrolls. */
export const COMPOSER_MAX_PX = 200;

/** A textarea that starts at one line and grows with its text up to `COMPOSER_MAX_PX`. */
export function useAutoGrow(ref: RefObject<HTMLTextAreaElement | null>, value: string): void {
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "";
    // No layout (a test's DOM): the stylesheet's one line stays.
    const h = el.scrollHeight;
    if (h > 0) el.style.height = `${Math.min(h, COMPOSER_MAX_PX)}px`;
  }, [ref, value]);
}

/** The send glyph: an arrow up, inside the circular send button. */
export function SendArrow() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 13V3M3.5 7.5 8 3l4.5 4.5" />
    </svg>
  );
}

/** Memory pressure, as a small gauge icon with the full sentence in its tooltip (and for
 * screen readers). Dispatch is never held back for it. */
export function PressureHint({ text }: { text: string }) {
  return (
    <span className="pressure-hint" title={text}>
      <svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
        <rect x="2.5" y="4.5" width="11" height="7" rx="1.5" />
        <path d="M5 4.5v-2M8 4.5v-2M11 4.5v-2M5 11.5v2M8 11.5v2M11 11.5v2M5.5 8h1.5" />
      </svg>
      <span className="sr-only" role="status">
        {text}
      </span>
    </span>
  );
}
