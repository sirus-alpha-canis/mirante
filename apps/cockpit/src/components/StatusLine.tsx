import { type Key, tn, useT } from "../lib/i18n";
import { size, systemMemory } from "../lib/memguard";
import type { DaemonStatus } from "../lib/protocol";
import type { ShortcutHints } from "../lib/shortcuts";
import type { Connection } from "../store/state";

export type StatusView = "home" | "detail" | "grid" | "brain" | "skills" | "rules";

/** The keys worth knowing in each view: [key, what it does]. */
export function viewHints(view: StatusView, hints: ShortcutHints, mac: boolean): [string, Key][] {
  const queue = mac ? "⌥↵" : "Alt+↵";
  switch (view) {
    case "home":
      return [
        [hints.next, "hint.next"],
        [hints.palette, "hint.commands"],
        [hints.send, "hint.dispatch"],
      ];
    case "detail":
      return [
        [hints.send, "hint.steer"],
        [queue, "hint.queue"],
        ["Esc", "hint.back"],
        [hints.palette, "hint.commands"],
      ];
    case "grid":
      return [
        ["↵", "hint.steer"],
        [queue, "hint.queue"],
        [hints.next, "hint.next"],
        [hints.palette, "hint.commands"],
      ];
    default:
      return [
        [hints.home, "hint.home"],
        [hints.next, "hint.next"],
        [hints.palette, "hint.commands"],
      ];
  }
}

/** The daemon's state as a dot class and its words (top bar and status line). */
export function daemonState(c: Connection): { dot: string; top: Key; line: Key } {
  if (c.state === "live") return { dot: "dot-ready", top: "top.daemonLive", line: "status.daemonLive" };
  if (c.state === "connecting") return { dot: "dot-work", top: "top.daemonConnecting", line: "status.daemonConnecting" };
  return { dot: "dot-needs", top: "top.daemonDown", line: "status.daemonDown" };
}

/**
 * The 24px line at the window's foot: daemon · system memory (level, swap, available) ·
 * what needs you · the keys of this view. Quiet text; never a live region (it changes every
 * few seconds; the banner and the inbox announce what matters).
 */
export function StatusLine(props: { connection: Connection; status: DaemonStatus | null; needing: number; view: StatusView; hints: ShortcutHints; mac: boolean }) {
  const t = useT();
  const d = daemonState(props.connection);
  const mem = systemMemory(props.status);
  const level =
    mem && (mem.level === "normal" || mem.level === "elevated" || mem.level === "critical") ? t(`mem.level.${mem.level}` as Key) : null;
  const memory = !mem
    ? t("status.memoryUnknown")
    : [
        t("status.memory", { level: level ?? t("mem.level.unknown") }),
        mem.swapTotal > 0 ? t("mem.swap", { pct: Math.round((mem.swapUsed * 100) / mem.swapTotal) }) : null,
        t("status.available", { size: size(mem.available ?? mem.free) }),
        mem.paused ? t("status.paused") : null,
      ]
        .filter(Boolean)
        .join(" · ");
  const pressured = mem?.level === "elevated" || mem?.level === "critical";
  return (
    <footer className="statusline" aria-label={t("status.label")}>
      <span>
        <span className={`dot ${d.dot}`} aria-hidden="true" />
        {t(d.line)}
      </span>
      <span className={pressured ? "status-needs" : undefined}>{memory}</span>
      {props.needing > 0 && <span className="status-needs">{tn(props.needing, "top.needsOne", "top.needsMany", { n: props.needing })}</span>}
      <span className="status-hints" aria-label={t("status.hints")}>
        {viewHints(props.view, props.hints, props.mac).map(([k, what], i) => (
          <span key={what}>
            {i > 0 && " · "}
            <kbd>{k}</kbd> {t(what)}
          </span>
        ))}
      </span>
    </footer>
  );
}
