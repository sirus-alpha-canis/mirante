import { useEffect, useMemo, useRef, useState } from "react";
import { useT } from "../lib/i18n";

export interface Command {
  id: string;
  label: string;
  hint?: string;
  run(): void;
}

/**
 * ⌘K: filter commands by text; "@agent repo/branch goal" goes to Home's dispatch box (it is
 * not sent from here: the user reviews it there and presses Enter). A modal dialog
 * holding a combobox over a listbox: the input keeps focus, the arrows move the active
 * option, Enter runs it, Escape closes; focus returns to where it was on close.
 */
export function CommandBar(props: { commands: Command[]; onDispatch(text: string): void; onClose(): void }) {
  const t = useT();
  const [q, setQ] = useState("");
  const [i, setI] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const before = document.activeElement;
    input.current?.focus();
    return () => {
      if (before instanceof HTMLElement && before.isConnected) before.focus();
    };
  }, []);
  const items = useMemo(() => {
    const text = q.trim();
    const needle = text.toLowerCase();
    const hits = props.commands.filter((c) => c.label.toLowerCase().includes(needle));
    if (text.startsWith("@")) {
      return [{ id: "dispatch", label: t("cmdbar.dispatch", { text }), run: () => props.onDispatch(text) }, ...hits];
    }
    return hits;
  }, [q, props, t]);
  const run = (c: Command | undefined) => {
    if (!c) return;
    props.onClose();
    c.run();
  };
  // The list can shrink under the active option (a task archived while the bar is open).
  const idx = Math.max(0, Math.min(i, items.length - 1));
  const active = items[idx];
  return (
    <div className="overlay" onMouseDown={props.onClose}>
      <div className="palette" role="dialog" aria-modal="true" aria-label={t("cmdbar.label")} onMouseDown={(e) => e.stopPropagation()}>
        <input
          ref={input}
          role="combobox"
          aria-label={t("cmdbar.input")}
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls="palette-list"
          aria-activedescendant={active ? `cmd-${idx}` : undefined}
          placeholder={t("cmdbar.placeholder")}
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            setI(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              props.onClose();
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              setI(Math.min(idx + 1, items.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setI(Math.max(idx - 1, 0));
            } else if (e.key === "Enter") {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              e.preventDefault();
              run(active);
            } else if (e.key === "Tab") {
              // The dialog is modal and holds one control: keep focus inside.
              e.preventDefault();
            }
          }}
        />
        <ul id="palette-list" role="listbox" aria-label={t("cmdbar.list")}>
          {items.map((c, n) => (
            <li
              key={c.id}
              // Positional: a command id can hold a path, which is no valid id token.
              id={`cmd-${n}`}
              role="option"
              aria-selected={n === idx}
              className={n === idx ? "active" : ""}
              onMouseEnter={() => setI(n)}
              onClick={() => run(c)}
            >
              <span>{c.label}</span>
              {c.hint && <kbd>{c.hint}</kbd>}
            </li>
          ))}
        </ul>
        {items.length === 0 && (
          <p className="muted small palette-empty" role="status">
            {t("cmdbar.none")}
          </p>
        )}
      </div>
    </div>
  );
}
