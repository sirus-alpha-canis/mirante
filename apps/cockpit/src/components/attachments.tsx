// What the composers share for attachments: the tray of chips above the field, the paperclip,
// the drop zone, and `useAttachments`, which uploads as soon as there is a task (a message,
// a group child's steer) or holds the files until one exists (a goal: uploaded after
// `task.create`, before `session.start`).
import { type ClipboardEvent, type RefObject, useCallback, useEffect, useRef, useState } from "react";
import { useBoot, useEnv } from "../env";
import {
  type AttachmentSource,
  type AttachmentView,
  baseName,
  guessKind,
  INLINE_TEXT_MAX,
  isStaleAttachment,
  MAX_PER_MESSAGE,
  pastedName,
  previewOf,
  PUT_MAX,
  sizeText,
  toBase64,
  upload,
} from "../lib/attachments";
import { addDropTarget, type DropTarget, focusedTarget } from "../lib/drop";
import { t, tn, useT } from "../lib/i18n";
import { M } from "../lib/protocol";

export interface AttItem {
  key: string;
  name: string;
  kind: "image" | "file";
  /** Unknown for a dropped file until the daemon read it. */
  size: number | null;
  /** A `data:` thumbnail for a pasted raster image. */
  preview: string | null;
  /** held: kept until a task exists (a goal). */
  status: "held" | "uploading" | "ready";
  src: AttachmentSource;
  view?: AttachmentView;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
let seq = 0;
const nextKey = () => `att-${++seq}`;

/** Reads a clipboard file; null (with a note) when it is too big for one request line. */
async function fromFile(file: File, onNote: (m: string) => void): Promise<AttItem | null> {
  const name = pastedName(file.name, file.type);
  if (file.size > PUT_MAX) {
    onNote(t("att.tooBig", { name }));
    return null;
  }
  const b64 = toBase64(new Uint8Array(await file.arrayBuffer()));
  const preview = previewOf(file.type, b64);
  return { key: nextKey(), name, kind: file.type.startsWith("image/") ? "image" : "file", size: file.size, preview, status: "held", src: { kind: "blob", name, b64, size: file.size } };
}

export interface Attachments {
  items: AttItem[];
  /** Ids the daemon holds, ready to send. */
  ids: string[];
  uploading: boolean;
  addFiles(files: File[]): Promise<void>;
  addPaths(paths: string[]): void;
  addText(text: string): void;
  /** A paste into the field: files (a screenshot) and long text become attachments; true
   * when it took the paste (the caller then prevents the default). */
  paste(e: ClipboardEvent<HTMLElement>): boolean;
  pick(): void;
  remove(key: string): void;
  /** Forgets `keys` (sent, or uploaded elsewhere); all when none given. */
  clear(keys?: string[]): void;
  /** A send refused for an id the daemon no longer knows (it restarted): the tray is
   * cleared and the person told to attach again. True when that was the refusal. */
  stale(e: unknown): boolean;
}

/** `task`: upload at once (null: hold until `uploadHeld`). `onNote`: where refusals show. */
export function useAttachments(task: string | null, onNote: (m: string) => void): Attachments {
  const { rpc, pickFiles } = useEnv();
  const [items, setItems] = useState<AttItem[]>([]);
  const itemsRef = useRef(items);
  itemsRef.current = items;
  const noteRef = useRef(onNote);
  noteRef.current = onNote;
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const note = useCallback((m: string) => alive.current && noteRef.current(m), []);

  const add = useCallback(
    (fresh: AttItem[]) => {
      const room = Math.max(0, MAX_PER_MESSAGE - itemsRef.current.length);
      if (fresh.length > room) {
        note(t("att.tooMany", { max: MAX_PER_MESSAGE }));
        fresh = fresh.slice(0, room);
      }
      if (fresh.length === 0) return;
      const placed = fresh.map((i) => ({ ...i, status: task ? ("uploading" as const) : ("held" as const) }));
      itemsRef.current = [...itemsRef.current, ...placed];
      setItems(itemsRef.current);
      if (!task) return;
      for (const it of placed) {
        upload(rpc, task, it.src).then(
          (view) => {
            if (!alive.current) return;
            setItems((cur) => cur.map((i) => (i.key === it.key ? { ...i, status: "ready", view, name: view.name, kind: view.kind, size: view.bytes } : i)));
          },
          (e) => {
            if (!alive.current) return;
            setItems((cur) => cur.filter((i) => i.key !== it.key));
            note(t("att.failed", { name: it.name, error: message(e) }));
          },
        );
      }
    },
    [rpc, task, note],
  );

  const addFiles = useCallback(
    async (files: File[]) => {
      const read = await Promise.all(files.map((f) => fromFile(f, note)));
      add(read.filter((x): x is AttItem => x !== null));
    },
    [add, note],
  );
  const addPaths = useCallback(
    (paths: string[]) =>
      add(paths.map((path) => ({ key: nextKey(), name: baseName(path), kind: guessKind(path), size: null, preview: null, status: "held", src: { kind: "path", path } }))),
    [add],
  );
  const addText = useCallback(
    (text: string) => {
      const bytes = new TextEncoder().encode(text);
      const name = "pasted-text.txt";
      add([{ key: nextKey(), name, kind: "file", size: bytes.length, preview: null, status: "held", src: { kind: "blob", name, b64: toBase64(bytes), size: bytes.length } }]);
    },
    [add],
  );

  const paste = (e: ClipboardEvent<HTMLElement>) => {
    const dt = e.clipboardData;
    if (!dt) return false;
    const files = Array.from(dt.files ?? []);
    if (files.length) {
      void addFiles(files);
      return true;
    }
    const text = dt.getData("text/plain");
    if (new TextEncoder().encode(text).length > INLINE_TEXT_MAX) {
      addText(text);
      return true;
    }
    return false;
  };

  const pick = () => {
    pickFiles().then(
      (paths) => paths.length && addPaths(paths),
      (e) => note(message(e)),
    );
  };

  const remove = (key: string) => {
    const it = itemsRef.current.find((i) => i.key === key);
    if (!it) return;
    setItems((cur) => cur.filter((i) => i.key !== key));
    // Already gone (sent, or the daemon restarted): nothing to undo there.
    if (task && it.view) rpc(M.attachmentRemove, { task, id: it.view.id }).catch(() => {});
  };

  const clear = useCallback((keys?: string[]) => {
    const drop = keys ? new Set(keys) : null;
    itemsRef.current = drop ? itemsRef.current.filter((i) => !drop.has(i.key)) : [];
    setItems(itemsRef.current);
  }, []);

  const clearUploaded = useCallback(() => {
    const gone = itemsRef.current.filter((i) => i.view);
    if (gone.length === 0) return false;
    clear(gone.map((i) => i.key));
    note(t("att.reattach"));
    return true;
  }, [clear, note]);

  // A restarted daemon forgot every id it gave: attach again rather than send in vain.
  const boot = useBoot();
  const bootWas = useRef(boot);
  useEffect(() => {
    const before = bootWas.current;
    bootWas.current = boot;
    if (before !== null && boot !== null && before !== boot) clearUploaded();
  }, [boot, clearUploaded]);

  const stale = (e: unknown) => {
    if (!isStaleAttachment(e)) return false;
    clearUploaded();
    return true;
  };

  return {
    items,
    ids: items.flatMap((i) => (i.status === "ready" && i.view ? [i.view.id] : [])),
    uploading: items.some((i) => i.status === "uploading"),
    addFiles,
    addPaths,
    addText,
    paste,
    pick,
    remove,
    clear,
    stale,
  };
}

/** A goal's held attachments, uploaded to the task just created (one at a time, in order).
 * A refusal names the file; the others still go. */
export async function uploadHeld(
  rpc: <T>(method: string, params?: object) => Promise<T>,
  task: string,
  held: AttItem[],
): Promise<{ ids: string[]; problems: string[] }> {
  const ids: string[] = [];
  const problems: string[] = [];
  for (const it of held) {
    try {
      ids.push((await upload(rpc, task, it.src)).id);
    } catch (e) {
      problems.push(t("att.failed", { name: it.name, error: message(e) }));
    }
  }
  return { ids, problems };
}

/** Registers `ref` as a place a Finder drop can land; true while a drag hovers it. */
export function useDropZone(ref: RefObject<HTMLElement | null>, onPaths: (paths: string[]) => void, enabled = true): boolean {
  const [over, setOver] = useState(false);
  const cb = useRef(onPaths);
  cb.current = onPaths;
  useEffect(() => {
    const el = ref.current;
    if (!enabled || !el) return;
    const target: DropTarget = { el: () => ref.current, drop: (p) => cb.current(p), hover: setOver };
    const off = addDropTarget(target);
    const focus = () => focusedTarget(target);
    el.addEventListener("focusin", focus);
    return () => {
      off();
      el.removeEventListener("focusin", focus);
      setOver(false);
    };
  }, [ref, enabled]);
  return over;
}

/** The paperclip: the native picker, several files. */
export function AttachButton({ att, disabled, className = "token token-icon" }: { att: Attachments; disabled?: boolean; className?: string }) {
  const tt = useT();
  return (
    <button type="button" className={`${className} att-pick`} aria-label={tt("att.pick")} title={tt("att.pick")} onClick={() => att.pick()} disabled={disabled}>
      <svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        <path d="M10.5 4.5 5.8 9.2a1.4 1.4 0 0 0 2 2l5-5a2.8 2.8 0 0 0-4-4l-5 5a4.2 4.2 0 0 0 6 6l4.2-4.2" />
      </svg>
    </button>
  );
}

/** A chip per attachment: thumbnail (or a glyph), name, size, secrets removed, ×. */
export function AttachmentTray({ att }: { att: Attachments }) {
  const tt = useT();
  if (att.items.length === 0) return null;
  return (
    <ul className="att-tray" aria-label={tt("att.tray")}>
      {att.items.map((i) => (
        <li key={i.key} className={`att-chip att-${i.status}`} title={i.view?.path ?? i.name}>
          {i.preview ? <img className="att-thumb" src={i.preview} alt="" /> : <span className="att-glyph" aria-hidden="true">{i.kind === "image" ? "▣" : "▤"}</span>}
          <span className="att-name">{i.name}</span>
          {i.size !== null && <span className="att-size">{sizeText(i.size)}</span>}
          {!!i.view?.redacted && <span className="att-redacted">{tn(i.view.redacted, "att.redactedOne", "att.redactedMany")}</span>}
          {i.status === "uploading" && <span className="att-spin" role="progressbar" aria-label={tt("att.uploading", { name: i.name })} />}
          <button type="button" className="att-x" aria-label={tt("att.remove", { name: i.name })} disabled={i.status === "uploading"} onClick={() => att.remove(i.key)}>
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}
