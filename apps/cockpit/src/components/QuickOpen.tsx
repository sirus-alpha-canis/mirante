// ⌘P inside a task: the task's files (`task.files`: tracked and untracked, not ignored, the
// daemon ranks them), filtered as the person types. ↵ opens one in the side pane, ⌘↵ in
// the person's editor. A modal dialog like the command bar: a combobox over a listbox, the
// input keeps focus, Escape closes, focus returns to where it was.
import { useEffect, useRef, useState } from "react";
import { useEnv } from "../env";
import { mayHoldSecrets } from "../lib/dispatch";
import { useT } from "../lib/i18n";
import { M } from "../lib/protocol";
import { hintsFor, onMac } from "../lib/shortcuts";

/** Typing settles this long before one `task.files`. */
export const QUICK_DEBOUNCE_MS = 120;
const LIMIT = 50;

export function QuickOpen({
  taskId,
  onOpen,
  onOpenEditor,
  onClose,
}: {
  taskId: string;
  onOpen(path: string): void;
  onOpenEditor(path: string): void;
  onClose(): void;
}) {
  const { rpc } = useEnv();
  const t = useT();
  const [q, setQ] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [i, setI] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const before = document.activeElement;
    input.current?.focus();
    return () => {
      if (before instanceof HTMLElement && before.isConnected) before.focus();
    };
  }, []);
  useEffect(() => {
    let live = true;
    const timer = setTimeout(() => {
      rpc<string[]>(M.taskFiles, { task: taskId, query: q.trim(), limit: LIMIT })
        .then((list) => {
          if (!live) return;
          setFiles(Array.isArray(list) ? list.filter((p) => typeof p === "string") : []);
          setError(null);
          setI(0);
        })
        .catch((e: unknown) => live && setError(e instanceof Error ? e.message : String(e)));
    }, QUICK_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [rpc, taskId, q]);
  const idx = Math.max(0, Math.min(i, files.length - 1));
  const active = files[idx];
  const pick = (path: string | undefined, editor: boolean) => {
    if (!path) return;
    onClose();
    if (editor) onOpenEditor(path);
    else onOpen(path);
  };
  const mod = hintsFor(onMac()).send.replace("↵", "");
  return (
    <div className="overlay" onMouseDown={onClose}>
      <div className="palette quick-open" role="dialog" aria-modal="true" aria-label={t("quick.label")} onMouseDown={(e) => e.stopPropagation()}>
        <input
          ref={input}
          role="combobox"
          aria-label={t("quick.input")}
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls="quick-list"
          aria-activedescendant={active ? `quick-${idx}` : undefined}
          placeholder={t("quick.placeholder")}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              setI(Math.min(idx + 1, files.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setI(Math.max(idx - 1, 0));
            } else if (e.key === "Enter") {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              e.preventDefault();
              pick(active, e.metaKey || e.ctrlKey);
            } else if (e.key === "Tab") {
              e.preventDefault();
            }
          }}
        />
        <ul id="quick-list" role="listbox" aria-label={t("quick.label")}>
          {files.map((p, n) => (
            <li key={p} id={`quick-${n}`} role="option" aria-selected={n === idx} className={n === idx ? "active" : ""} onMouseEnter={() => setI(n)} onClick={(e) => pick(p, e.metaKey || e.ctrlKey)}>
              <span className="quick-path">{p}</span>
              {mayHoldSecrets(p) && <span className="muted small">{t("context.secrets")}</span>}
            </li>
          ))}
        </ul>
        {error ? (
          <p className="error small" role="alert">
            {t("quick.failed", { error })}
          </p>
        ) : (
          files.length === 0 && (
            <p className="muted small palette-empty" role="status">
              {t("quick.none")}
            </p>
          )
        )}
        <p className="muted small quick-hint">{t("quick.hint", { mod })}</p>
      </div>
    </div>
  );
}
