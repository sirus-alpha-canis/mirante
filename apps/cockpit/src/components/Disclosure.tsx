import { type ReactNode, useId, useState } from "react";
import { useEnv } from "../env";

/** Where a rail disclosure remembers whether it is open (per viewer, this machine). */
export const disclosureKey = (id: string) => `mirante.rail.${id}.open`;

/** Open when this viewer left it open; closed otherwise (and when storage refuses). */
function loadOpen(store: Pick<Storage, "getItem">, id: string): boolean {
  try {
    return store.getItem(disclosureKey(id)) === "1";
  } catch {
    return false;
  }
}

/**
 * A rail section that folds to one line: its title and a summary that stays live. Closed
 * by default; the header is a button (`aria-expanded`, `aria-controls`). It never opens on
 * its own (a change of state updates the summary, never the layout). The body stays
 * mounted while closed, so a half-done action inside keeps its state.
 */
export function Disclosure(props: {
  /** Names its remembered state (`disclosureKey`). */
  id: string;
  title: string;
  /** The region's name (defaults to the title). */
  label?: string;
  /** The one line shown while closed. */
  summary: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  const { storage } = useEnv();
  const [open, setOpen] = useState(() => loadOpen(storage, props.id));
  const body = useId();
  const toggle = () => {
    const next = !open;
    setOpen(next);
    try {
      storage.setItem(disclosureKey(props.id), next ? "1" : "0");
    } catch {
      // Not remembered: the next start shows it closed.
    }
  };
  return (
    <section className={`disclosure ${props.className ?? ""}`.trim()} aria-label={props.label ?? props.title}>
      <h2>
        <button type="button" className="disclosure-head" aria-expanded={open} aria-controls={body} onClick={toggle}>
          <span className="disclosure-chev" aria-hidden="true" />
          <span className="disclosure-title">{props.title}</span>
          {!open && props.summary ? (
            <>
              {" "}
              <span className="disclosure-summary">{props.summary}</span>
            </>
          ) : null}
        </button>
      </h2>
      <div id={body} className="disclosure-body" hidden={!open}>
        {props.children}
      </div>
    </section>
  );
}
