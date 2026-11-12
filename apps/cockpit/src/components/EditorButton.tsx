// "Open in editor" for a worktree file: the editors installed here (asked once per app run),
// the one the person last picked first. The shell resolves the file through the daemon
// (`task.file` with `stat`), so nothing here names an absolute path.
import { useEffect, useState } from "react";
import { useEnv } from "../env";
import type { EditorId } from "../lib/daemon";
import { type Key, useT } from "../lib/i18n";

export const EDITOR_KEY = "mirante.editor";
const ORDER: EditorId[] = ["cursor", "vscode", "zed", "sublime", "xcode", "default"];
export const EDITOR_LABEL: Record<EditorId, Key> = {
  vscode: "editor.vscode",
  cursor: "editor.cursor",
  zed: "editor.zed",
  sublime: "editor.sublime",
  xcode: "editor.xcode",
  default: "editor.default",
};

let cached: Promise<EditorId[]> | null = null;
/** For tests: forget what the shell said. */
export function resetEditorsCache() {
  cached = null;
}

/** The installed editors in a stable order (always at least the default text editor). */
export function useEditors(): EditorId[] {
  const { editors } = useEnv();
  const [list, setList] = useState<EditorId[]>(["default"]);
  useEffect(() => {
    let live = true;
    cached ??= editors().catch(() => ["default"] as EditorId[]);
    void cached.then((l) => {
      if (!live) return;
      const known = ORDER.filter((e) => l.includes(e));
      setList(known.length ? known : ["default"]);
    });
    return () => {
      live = false;
    };
  }, [editors]);
  return list;
}

/** The editor to use: the remembered one when still installed, else the first installed. */
export function preferredEditor(list: EditorId[], saved: string | null): EditorId {
  return list.find((e) => e === saved) ?? list[0] ?? "default";
}

/** Opens `path` of `taskId` in the person's editor; says why when it could not. */
export function useOpenInEditor(taskId: string) {
  const env = useEnv();
  const list = useEditors();
  const t = useT();
  const [error, setError] = useState<string | null>(null);
  const editor = preferredEditor(list, env.storage.getItem(EDITOR_KEY));
  const open = (path: string, line?: number | null, which: EditorId = editor) => {
    setError(null);
    env.openInEditor(taskId, path, line ?? null, which).catch((e: unknown) =>
      setError(t("file.editorFailed", { editor: t(EDITOR_LABEL[which]), error: e instanceof Error ? e.message : String(e) })),
    );
  };
  return { list, editor, open, error };
}

export function EditorButton({ taskId, path, line }: { taskId: string; path: string; line?: number }) {
  const { storage } = useEnv();
  const t = useT();
  const { list, editor, open, error } = useOpenInEditor(taskId);
  const [which, setWhich] = useState<EditorId | null>(null);
  const chosen = which && list.includes(which) ? which : editor;
  return (
    <span className="editor-button">
      <button type="button" className="btn btn-small" onClick={() => open(path, line, chosen)}>
        {t("file.openIn", { editor: t(EDITOR_LABEL[chosen]) })}
      </button>
      {list.length > 1 && (
        <select
          className="editor-select"
          aria-label={t("file.openInEditor")}
          value={chosen}
          onChange={(e) => {
            const v = e.target.value as EditorId;
            setWhich(v);
            storage.setItem(EDITOR_KEY, v);
          }}
        >
          {list.map((e) => (
            <option key={e} value={e}>
              {t(EDITOR_LABEL[e])}
            </option>
          ))}
        </select>
      )}
      {error && (
        <span className="error small" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}
