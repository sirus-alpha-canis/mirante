// The side pane's tab strip: one tab per thing shown (× closes it), the "+" menu that adds
// a surface (the person's own Terminal, a File through quick-open), and Split. ←/→ move
// between tabs (WAI-ARIA tabs); the menu is a WAI-ARIA menu (↑/↓, Enter, Escape closes it
// and focus goes back to "+").
import { type KeyboardEvent, type ReactNode, useRef, useState } from "react";
import { useT } from "../lib/i18n";
import { MAX_SHELLS, type PaneState } from "../lib/sidepane";
import { useDismiss } from "../lib/useDismiss";

export type AddSurface = "terminal" | "file";

export function PaneTabs({
  pane,
  label,
  name,
  canAddShell,
  onFocus,
  onCloseTab,
  onAdd,
  onSplit,
}: {
  pane: PaneState;
  /** Each tab's title. */
  label(id: string): ReactNode;
  /** Each tab's name as plain text (for its × button). */
  name(id: string): string;
  canAddShell: boolean;
  onFocus(id: string): void;
  onCloseTab(id: string): void;
  onAdd(what: AddSurface): void;
  onSplit(): void;
}) {
  const t = useT();
  const refs = useRef(new Map<string, HTMLButtonElement>());
  const onKey = (e: KeyboardEvent) => {
    const ids = pane.tabs.map((x) => x.id);
    const at = ids.indexOf(pane.active ?? "");
    const to = e.key === "ArrowRight" ? at + 1 : e.key === "ArrowLeft" ? at - 1 : e.key === "Home" ? 0 : e.key === "End" ? ids.length - 1 : null;
    if (to === null || ids.length === 0) return;
    e.preventDefault();
    const id = ids[(to + ids.length) % ids.length];
    onFocus(id);
    refs.current.get(id)?.focus();
  };
  return (
    <div className="pane-tabs">
      <div role="tablist" aria-label={t("pane.tabs")} className="pane-tablist" onKeyDown={onKey}>
        {pane.tabs.map((tab) => {
          const selected = tab.id === pane.active;
          const beside = tab.id === pane.split?.second;
          return (
            <span key={tab.id} className={`pane-tab${selected ? " pane-tab-on" : ""}${beside ? " pane-tab-beside" : ""}`}>
              <button
                type="button"
                role="tab"
                id={`pane-tab-${tab.id}`}
                aria-selected={selected}
                tabIndex={selected ? 0 : -1}
                ref={(el) => {
                  if (el) refs.current.set(tab.id, el);
                  else refs.current.delete(tab.id);
                }}
                onClick={() => onFocus(tab.id)}
              >
                {label(tab.id)}
              </button>
              <CloseTab title={name(tab.id)} onClose={() => onCloseTab(tab.id)} />
            </span>
          );
        })}
      </div>
      <AddMenu canAddShell={canAddShell} onAdd={onAdd} />
      {pane.tabs.length > 1 && (
        <button type="button" className="btn btn-small btn-ghost" aria-pressed={!!pane.split} title={t("pane.splitTitle")} onClick={onSplit}>
          {t(pane.split ? "pane.unsplit" : "pane.split")}
        </button>
      )}
    </div>
  );
}

function CloseTab({ title, onClose }: { title: string; onClose(): void }) {
  const t = useT();
  return (
    <button type="button" className="pane-tab-x" aria-label={t("pane.closeTab", { title })} title={t("pane.closeTab", { title })} onClick={onClose}>
      ×
    </button>
  );
}

function AddMenu({ canAddShell, onAdd }: { canAddShell: boolean; onAdd(what: AddSurface): void }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);
  useDismiss(
    wrap,
    (why) => {
      setOpen(false);
      if (why === "escape") button.current?.focus();
    },
    { enabled: open },
  );
  const choose = (what: AddSurface) => {
    setOpen(false);
    button.current?.focus();
    onAdd(what);
  };
  const onMenuKey = (e: KeyboardEvent) => {
    const live = items.current.filter((el): el is HTMLButtonElement => !!el && !el.disabled);
    if (live.length === 0) return;
    const at = live.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : live.length - 1;
      live[(Math.max(at, 0) + (at < 0 ? 0 : step)) % live.length].focus();
    }
  };
  const entries: { what: AddSurface; text: string; disabled: boolean }[] = [
    { what: "terminal", text: canAddShell ? t("pane.add.terminal") : t("pane.add.terminalFull", { n: MAX_SHELLS }), disabled: !canAddShell },
    { what: "file", text: t("pane.add.file"), disabled: false },
  ];
  return (
    <span className="popover-wrap" ref={wrap}>
      <button
        type="button"
        ref={button}
        className="btn btn-small btn-ghost"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("pane.add")}
        title={t("pane.add")}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        +
      </button>
      {open && (
        <div className="popover pane-add-menu">
          <ul role="menu" aria-label={t("pane.add")} className="menu" onKeyDown={onMenuKey}>
            {entries.map((x, i) => (
              <li key={x.what} role="none">
                <button
                  type="button"
                  role="menuitem"
                  className="menu-item"
                  disabled={x.disabled}
                  autoFocus={i === entries.findIndex((y) => !y.disabled)}
                  ref={(el) => {
                    items.current[i] = el;
                  }}
                  onClick={() => choose(x.what)}
                >
                  {x.text}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </span>
  );
}
