// The launch splash lives in index.html (it paints before any script runs). Once the app has
// mounted, it fades out and goes; with reduced motion it goes at once.

/** How long the fade lasts (the `.splash-out` transition in index.html), then it is removed. */
export const SPLASH_FADE_MS = 200;

export function prefersReducedMotion(): boolean {
  try {
    return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  } catch {
    return false;
  }
}

export function dismissSplash(doc: Document = document, reduced: boolean = prefersReducedMotion()): void {
  const el = doc.getElementById("splash");
  if (!el) return;
  if (reduced) {
    el.remove();
    return;
  }
  el.classList.add("splash-out");
  setTimeout(() => el.remove(), SPLASH_FADE_MS);
}
