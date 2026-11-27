// Attachments: images and files a person pastes, drops or picks. The daemon stores them in
// the task's own folder (redacting HAR and text files) and hands the agent their paths; this
// module only shapes what goes over the wire. No Tauri here (lib/daemon.ts is the one).
import { DaemonError } from "./daemon";
import { M, RpcCode } from "./protocol";

/** `attachment.put` / `attachment.import`'s answer. */
export interface AttachmentView {
  id: string;
  /** Where the daemon stored it (absolute, canonical). */
  path: string;
  name: string;
  kind: "image" | "file";
  bytes: number;
  /** Secrets the daemon removed before storing it. */
  redacted: number;
}

/** What is attached before (or while) it is uploaded: bytes from the clipboard, or a path
 * from a drop or the picker (the daemon copies the file). */
export type AttachmentSource = { kind: "blob"; name: string; b64: string; size: number } | { kind: "path"; path: string };

/** A text paste longer than this becomes `pasted-text.txt` instead of going inline. */
export const INLINE_TEXT_MAX = 32 * 1024;
/** A pasted blob travels as base64 in one request line (16 MB): about 12 MB decoded. A
 * bigger file is dropped or picked instead (the daemon reads it from disk). */
export const PUT_MAX = 12 * 1024 * 1024;
/** The daemon takes at most this many per message. */
export const MAX_PER_MESSAGE = 20;

/** Standard base64 (a JSON array of numbers would be about four times larger). */
export function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** `path` as one shell word: single-quoted, each `'` closed, escaped and reopened. */
export function shellQuote(path: string): string {
  return `'${path.replace(/'/g, "'\\''")}'`;
}

/** What a terminal paste of stored `paths` types into the PTY: each path one shell word
 * (control characters stripped, so a name cannot end the paste or press a key), separated by
 * spaces, inside the bracketed-paste markers only when the program turned that mode on. */
export function pathPaste(paths: string[], bracketed: boolean): string {
  // eslint-disable-next-line no-control-regex
  const words = paths.map((p) => shellQuote(p.replace(/[\u0000-\u001f\u007f-\u009f]/g, ""))).join(" ");
  return bracketed ? `\x1b[200~${words}\x1b[201~` : words;
}

export function sizeText(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1).replace(/\.0$/, "")} MB`;
}

const RASTER = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

/** A thumbnail for a raster image the webview holds the bytes of (`img-src data:`); never
 * SVG (a document, not a picture). */
export function previewOf(mime: string, b64: string): string | null {
  return RASTER.has(mime) ? `data:${mime};base64,${b64}` : null;
}

/** A dropped or picked file's likely kind, until the daemon (which reads its bytes) says. */
export function guessKind(path: string): "image" | "file" {
  return /\.(png|jpe?g|gif|webp)$/i.test(path) ? "image" : "file";
}

export function baseName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

/** A clipboard blob's name (screenshots often come without one). */
export function pastedName(name: string, mime: string): string {
  if (name) return name;
  const ext = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" }[mime];
  return ext ? `pasted-image.${ext}` : "pasted-file";
}

/** The daemon refused an id it does not know: it restarted since the upload (ids live in
 * its memory), so the files must be attached again. */
export function isStaleAttachment(e: unknown): boolean {
  return e instanceof DaemonError && e.code === RpcCode.INVALID_PARAMS && /unknown attachment/.test(e.message);
}

type Rpc = <T>(method: string, params?: object) => Promise<T>;

/** Uploads one source to `task` (`attachment.put` or `attachment.import`). */
export function upload(rpc: Rpc, task: string, src: AttachmentSource): Promise<AttachmentView> {
  return src.kind === "blob"
    ? rpc<AttachmentView>(M.attachmentPut, { task, name: src.name, b64: src.b64 })
    : rpc<AttachmentView>(M.attachmentImport, { task, path: src.path });
}
