// A line diff for showing a skill edit: common prefix and suffix are cut first (linear),
// then a longest-common-subsequence table runs on the changed middle only. When the middle
// is too large for the table (more than DIFF_MAX_LINES lines on a side, or more than
// DIFF_MAX_CELLS cells), it is shown as one replaced block: every line still appears, only
// the pairing is coarser. Returns data; the caller renders each line as a text node.

export type DiffOp = { same: string } | { del: string } | { add: string };

export const DIFF_MAX_LINES = 2000;
/** Cells of the middle's LCS table (one byte each). */
export const DIFF_MAX_CELLS = 1_000_000;

/** Lines of `text`, without the empty line after a final newline. */
export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split(/\r?\n/);
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}

export function lineDiff(before: string, after: string): DiffOp[] {
  const a = splitLines(before);
  const b = splitLines(after);
  let pre = 0;
  while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (suf < a.length - pre && suf < b.length - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
  const out: DiffOp[] = a.slice(0, pre).map((same) => ({ same }));
  const am = a.slice(pre, a.length - suf);
  const bm = b.slice(pre, b.length - suf);
  out.push(...middle(am, bm));
  for (const same of a.slice(a.length - suf)) out.push({ same });
  return out;
}

function block(a: string[], b: string[]): DiffOp[] {
  return [...a.map((del) => ({ del })), ...b.map((add) => ({ add }))];
}

function middle(a: string[], b: string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  if (n === 0 || m === 0) return block(a, b);
  if (n > DIFF_MAX_LINES || m > DIFF_MAX_LINES || (n + 1) * (m + 1) > DIFF_MAX_CELLS) return block(a, b);
  // len[i][j]: LCS length of a[i..] and b[j..], capped storage (Uint16 suffices: ≤2000).
  const w = m + 1;
  const len = new Uint16Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      len[i * w + j] = a[i] === b[j] ? len[(i + 1) * w + j + 1] + 1 : Math.max(len[(i + 1) * w + j], len[i * w + j + 1]);
    }
  }
  const out: DiffOp[] = [];
  let dels: DiffOp[] = [];
  let adds: DiffOp[] = [];
  const flush = () => {
    out.push(...dels, ...adds);
    dels = [];
    adds = [];
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      flush();
      out.push({ same: a[i] });
      i++;
      j++;
    } else if (len[(i + 1) * w + j] >= len[i * w + j + 1]) {
      dels.push({ del: a[i++] });
    } else {
      adds.push({ add: b[j++] });
    }
  }
  while (i < n) dels.push({ del: a[i++] });
  while (j < m) adds.push({ add: b[j++] });
  flush();
  return out;
}
