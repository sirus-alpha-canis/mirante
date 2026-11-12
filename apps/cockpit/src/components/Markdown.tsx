// The side pane's Markdown preview: `lib/markdown`'s whitelisted AST mapped to fixed React
// elements. Never HTML (no raw-HTML escape hatch): every string is a text node. Links
// go through the opener rule (github.com, localhost), open a worktree file in the pane, or
// are shown with "copy link". Images load only from the task's worktree (`task.file`, as a
// `data:` URL the CSP already allows); a remote image is its alt text.
import { memo, type ReactNode, useEffect, useMemo, useState } from "react";
import { useEnv } from "../env";
import { useT } from "../lib/i18n";
import { type Block, type Inline, linkTarget, MARKDOWN_MAX, parse, resolveRel } from "../lib/markdown";
import { type FileView, M } from "../lib/protocol";

/** What an image may be shown as (never SVG: it is a document that can run script). */
const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

interface Ctx {
  path: string;
  taskId?: string;
  onOpenPath?(path: string, line?: number): void;
}

function CopyLink({ href, children }: { href: string; children: ReactNode }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(href).then(
      () => setCopied(true),
      () => {},
    );
  };
  return (
    <span className="md-link-copy" title={t("md.linkCopyTitle", { href })}>
      {children}{" "}
      <button type="button" className="btn btn-small btn-ghost" onClick={copy}>
        {copied ? t("md.copied") : t("md.copyLink")}
      </button>
    </span>
  );
}

function Link({ href, c, ctx, k }: { href: string; c: Inline[]; ctx: Ctx; k: string }) {
  const { openUrl } = useEnv();
  const target = linkTarget(href, ctx.path);
  const kids = renderInlines(c, ctx, k);
  if (target.kind === "copy" || (target.kind === "pane" && !ctx.onOpenPath)) return <CopyLink href={href}>{kids}</CopyLink>;
  const go = (e: React.MouseEvent) => {
    e.preventDefault();
    if (target.kind === "open") void openUrl(target.url).catch(() => {});
    else ctx.onOpenPath?.(target.path, target.line);
  };
  // An href the webview never follows on its own ("#"): the click handler decides.
  return (
    <a href="#" className="md-link" title={target.kind === "open" ? target.url : target.path} onClick={go}>
      {kids}
    </a>
  );
}

/** A worktree image, read through `task.file`; anything else stays its alt text. */
function WorktreeImage({ taskId, path, alt }: { taskId: string; path: string; alt: string }) {
  const { rpc } = useEnv();
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    rpc<FileView>(M.taskFile, { task: taskId, path })
      .then((f) => {
        if (live && f.kind === "image" && f.image && IMAGE_MIMES.has(f.image.mime) && /^[A-Za-z0-9+/=]*$/.test(f.image.b64)) {
          setSrc(`data:${f.image.mime};base64,${f.image.b64}`);
        }
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [rpc, taskId, path]);
  return src ? <img className="md-img" src={src} alt={alt} /> : <span className="md-img-alt">{alt}</span>;
}

function Image({ src, alt, ctx }: { src: string; alt: string; ctx: Ctx }) {
  const t = useT();
  const remote = /^[a-z][a-z0-9+.-]*:/i.test(src.trim()) || src.trim().startsWith("//");
  const rel = remote ? null : resolveRel(ctx.path, decodeSafe(src.split(/[?#]/)[0]));
  if (!rel || !ctx.taskId) {
    return (
      <span className="md-img-alt">
        {alt} <span className="muted small">{t("md.remoteImage")}</span>
      </span>
    );
  }
  return <WorktreeImage taskId={ctx.taskId} path={rel} alt={alt} />;
}

function decodeSafe(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

function renderInlines(list: Inline[], ctx: Ctx, key: string): ReactNode[] {
  return list.map((n, i) => {
    const k = `${key}.${i}`;
    switch (n.t) {
      case "text":
        return n.v;
      case "strong":
        return <strong key={k}>{renderInlines(n.c, ctx, k)}</strong>;
      case "em":
        return <em key={k}>{renderInlines(n.c, ctx, k)}</em>;
      case "code":
        return <code key={k}>{n.v}</code>;
      case "link":
        return <Link key={k} href={n.href} c={n.c} ctx={ctx} k={k} />;
      case "img":
        return <Image key={k} src={n.src} alt={n.alt} ctx={ctx} />;
    }
  });
}

function renderBlocks(list: Block[], ctx: Ctx, key: string): ReactNode[] {
  return list.map((b, i) => {
    const k = `${key}-${i}`;
    switch (b.type) {
      case "heading": {
        const H = `h${b.level}` as "h1";
        return <H key={k}>{renderInlines(b.inl, ctx, k)}</H>;
      }
      case "para":
        return <p key={k}>{renderInlines(b.inl, ctx, k)}</p>;
      case "list": {
        const L = b.ordered ? "ol" : "ul";
        return (
          <L key={k}>
            {b.items.map((item, n) => (
              // A one-paragraph item is its text (no <p> inside the <li>).
              <li key={n}>
                {item.length === 1 && item[0].type === "para" ? renderInlines(item[0].inl, ctx, `${k}.${n}`) : renderBlocks(item, ctx, `${k}.${n}`)}
              </li>
            ))}
          </L>
        );
      }
      case "code":
        return (
          <pre key={k} className="md-code" data-lang={b.lang ?? undefined}>
            <code>{b.text}</code>
          </pre>
        );
      case "quote":
        return <blockquote key={k}>{renderBlocks(b.blocks, ctx, k)}</blockquote>;
      case "hr":
        return <hr key={k} />;
      case "table":
        return (
          <table key={k} className="md-table">
            <thead>
              <tr>
                {b.head.map((c, n) => (
                  <th key={n}>{renderInlines(c, ctx, `${k}.h${n}`)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((r, n) => (
                <tr key={n}>
                  {r.map((c, m) => (
                    <td key={m}>{renderInlines(c, ctx, `${k}.${n}.${m}`)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        );
    }
  });
}

/** `src` (the text of `path`, a worktree-relative file) as a document. */
export const Markdown = memo(function Markdown({
  src,
  path,
  taskId,
  onOpenPath,
}: {
  src: string;
  path: string;
  taskId?: string;
  onOpenPath?(path: string, line?: number): void;
}) {
  const t = useT();
  const big = src.length > MARKDOWN_MAX;
  const blocks = useMemo(() => (big ? [] : parse(src)), [src, big]);
  if (big) {
    return (
      <div className="markdown">
        <p className="muted small">{t("md.tooLarge", { size: formatBytes(src.length) })}</p>
        <pre className="md-code">{src}</pre>
      </div>
    );
  }
  return <div className="markdown">{renderBlocks(blocks, { path, taskId, onOpenPath }, "b")}</div>;
});
