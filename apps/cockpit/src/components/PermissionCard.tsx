import { useEffect, useRef, useState } from "react";
import { useEnv, useLive } from "../env";
import { DaemonError } from "../lib/daemon";
import { GRANT_ROOT_TOOL, permissionScope } from "../lib/labels";
import { type AgentKind, M, RpcCode } from "../lib/protocol";
import type { Perm } from "../store/state";
import { useT } from "../lib/i18n";

/**
 * A pending permission request. Answers are one-time; there is no "always allow". A Claude
 * card is answered in the Terminal tab unless the daemon types Claude answers
 * (`inlineAnswers`), and also once the daemon refused to (`ANSWER_IN_TERMINAL`).
 */
export function PermissionCard({
  perm,
  agent,
  inlineAnswers,
  onAnswerInTerminal,
  compact = false,
}: {
  perm: Perm;
  agent: AgentKind;
  inlineAnswers: boolean;
  onAnswerInTerminal(): void;
  compact?: boolean;
}) {
  const { rpc } = useEnv();
  const t = useT();
  const live = useLive();
  // "idle" | in flight | answered (sent, or already answered elsewhere): the card stays
  // until the closing event removes it, so a sent answer keeps its buttons off.
  const [phase, setPhase] = useState<"idle" | "busy" | "sent" | "done">("idle");
  const [error, setError] = useState<string | null>(null);
  const [refused, setRefused] = useState(false);
  const terminalButton = useRef<HTMLButtonElement>(null);
  const scope = permissionScope(perm.tool);
  const inTerminal = agent === "claude" && (!inlineAnswers || refused);
  useEffect(() => {
    // The buttons the user just used are gone: keep the keyboard on the card.
    if (refused) terminalButton.current?.focus();
  }, [refused]);
  const sending = useRef(false);
  const answer = async (allow: boolean) => {
    // A ref, not state: two clicks in one frame both see the old state.
    if (sending.current) return;
    sending.current = true;
    setPhase("busy");
    setError(null);
    try {
      await rpc(M.permissionAnswer, { id: perm.id, allow });
      setPhase("sent");
    } catch (e) {
      const code = e instanceof DaemonError ? e.code : null;
      if (code === RpcCode.ANSWER_IN_TERMINAL) setRefused(true);
      setError(code === RpcCode.CONFLICT ? t("ui.alreadyAnswered") : String(e instanceof Error ? e.message : e));
      setPhase(code === RpcCode.CONFLICT ? "done" : "idle");
      sending.current = code === RpcCode.CONFLICT;
    }
  };
  const off = phase !== "idle" || !live;
  return (
    <div className={`perm ${compact ? "perm-compact" : ""}`} role="group" aria-label={t("perm.label", { tool: perm.tool })}>
      <div className="perm-what">
        <strong>{perm.tool}</strong>
        <span className={`tag ${perm.tool === GRANT_ROOT_TOOL ? "tag-warn" : ""}`} title={scope.title}>
          {scope.text}
        </span>
        <code className="perm-detail">{perm.detail}</code>
      </div>
      <div className="perm-actions">
        {inTerminal ? (
          <button
            ref={terminalButton}
            className="btn btn-primary"
            title={t("perm.terminalTitle")}
            onClick={onAnswerInTerminal}
          >
            {t("perm.answerInTerminal")}
          </button>
        ) : (
          <>
            <button className="btn btn-primary" disabled={off} onClick={() => answer(true)}>
              {scope.allow}
            </button>
            <button className="btn" disabled={off} onClick={() => answer(false)}>
              {t("perm.deny")}
            </button>
            {phase === "sent" && (
              <span className="muted small" role="status">
                {t("perm.sent")}
              </span>
            )}
          </>
        )}
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
