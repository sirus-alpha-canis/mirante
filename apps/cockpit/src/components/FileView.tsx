// One worktree file in the side pane, as the daemon read it (`task.file`): highlighted text
// with line numbers, a Markdown preview (with its source a click away), an image, or what it
// is when it cannot be shown (binary, too large, maybe secret until revealed). Every string
// is a React text node.
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useEnv } from "../env";
import { highlight } from "../lib/highlight";
import { useT } from "../lib/i18n";
import { type FileView as File, M } from "../lib/protocol";
import { EditorButton } from "./EditorButton";
import { formatBytes, Markdown } from "./Markdown";

const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

type Loaded = { state: "loading" } | { state: "ready"; file: File } | { state: "error"; error: string };

const Code = memo(function Code({ text, lang, line, path }: { text: string; lang?: string; line?: number; path: string }) {
  const t = useT();
  const lines = useMemo(() => highlight(text, lang), [text, lang]);
  const marked = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    marked.current?.scrollIntoView?.({ block: "center" });
  }, [line, text]);
  return (
    <pre className="code-view" aria-label={t("file.lines", { path })}>
      {lines.map((toks, i) => {
        const n = i + 1;
        const mark = n === line;
        return (
          <div key={n} className={`code-line${mark ? " code-line-mark" : ""}`} data-line={n} ref={mark ? marked : undefined}>
            <span className="code-ln" aria-hidden="true">
              {n}
            </span>
            <span className="code-text">
              {toks.map((tk, j) =>
                tk.k === "plain" ? (
                  tk.v
                ) : (
                  <span key={j} className={`tk-${tk.k}`}>
                    {tk.v}
                  </span>
                ),
              )}
            </span>
          </div>
        );
      })}
    </pre>
  );
});

/** `path` (worktree-relative) of task `taskId`, at `line` when given. */
export function FileView({
  taskId,
  path,
  line,
  onOpenPath,
}: {
  taskId: string;
  path: string;
  line?: number;
  onOpenPath(path: string, line?: number): void;
}) {
  const { rpc } = useEnv();
  const t = useT();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [reveal, setReveal] = useState(false);
  const [source, setSource] = useState(false);
  useEffect(() => {
    setReveal(false);
    setSource(false);
  }, [taskId, path]);
  useEffect(() => {
    let live = true;
    setLoaded({ state: "loading" });
    rpc<File>(M.taskFile, reveal ? { task: taskId, path, reveal: true } : { task: taskId, path })
      .then((file) => live && setLoaded({ state: "ready", file }))
      .catch((e: unknown) => live && setLoaded({ state: "error", error: e instanceof Error ? e.message : String(e) }));
    return () => {
      live = false;
    };
  }, [rpc, taskId, path, reveal]);

  if (loaded.state === "loading") return <p className="muted small">{t("file.loading")}</p>;
  if (loaded.state === "error") {
    return (
      <p className="error small" role="alert">
        {t("file.failed", { path, error: loaded.error })}
      </p>
    );
  }
  const f = loaded.file;
  const editor = <EditorButton taskId={taskId} path={path} line={line} />;
  switch (f.kind) {
    case "secret_hidden":
      return (
        <div className="file-note">
          <p>{t("file.secret")}</p>
          <button type="button" className="btn btn-small" onClick={() => setReveal(true)}>
            {t("file.reveal")}
          </button>
        </div>
      );
    case "binary":
      return (
        <div className="file-note">
          <p>{t("file.binary", { size: formatBytes(f.size) })}</p>
          {editor}
        </div>
      );
    case "too_large":
      return (
        <div className="file-note">
          <p>{t("file.tooLarge", { size: formatBytes(f.size) })}</p>
          {editor}
        </div>
      );
    case "image":
      return f.image && IMAGE_MIMES.has(f.image.mime) && /^[A-Za-z0-9+/=]*$/.test(f.image.b64) ? (
        <div className="file-image">
          <img src={`data:${f.image.mime};base64,${f.image.b64}`} alt={path} />
        </div>
      ) : (
        <p className="muted small">{t("file.binary", { size: formatBytes(f.size) })}</p>
      );
    case "text": {
      const text = f.text ?? "";
      const md = f.lang === "markdown";
      return (
        <div className="file-text">
          {(md || f.truncated) && (
            <div className="file-tools">
              {md && (
                <span role="group" aria-label={t("file.view")}>
                  <button type="button" className="token" aria-pressed={!source} onClick={() => setSource(false)}>
                    {t("file.preview")}
                  </button>
                  <button type="button" className="token" aria-pressed={source} onClick={() => setSource(true)}>
                    {t("file.source")}
                  </button>
                </span>
              )}
              {f.truncated && <span className="muted small">{t("file.truncated")}</span>}
            </div>
          )}
          {md && !source ? (
            <Markdown src={text} path={path} taskId={taskId} onOpenPath={onOpenPath} />
          ) : (
            <Code text={text} lang={f.lang} line={line} path={path} />
          )}
        </div>
      );
    }
  }
}
