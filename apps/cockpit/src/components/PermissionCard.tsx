import { useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { permissionScope } from "../lib/labels";
import { type AgentKind, M, RpcCode } from "../lib/protocol";
import type { Perm } from "../store/state";

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
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refused, setRefused] = useState(false);
  const scope = permissionScope(perm.tool);
  const inTerminal = agent === "claude" && (!inlineAnswers || refused);
  const answer = async (allow: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await rpc(M.permissionAnswer, { id: perm.id, allow });
    } catch (e) {
      const code = e instanceof DaemonError ? e.code : null;
      if (code === RpcCode.ANSWER_IN_TERMINAL) setRefused(true);
      setError(code === RpcCode.CONFLICT ? "already answered" : String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={`perm ${compact ? "perm-compact" : ""}`} role="group" aria-label={`Permission request: ${perm.tool}`}>
      <div className="perm-what">
        <strong>{perm.tool}</strong>
        <span className={`tag ${scope.text === "once" ? "" : "tag-warn"}`} title={scope.title}>
          {scope.text}
        </span>
        <code className="perm-detail">{perm.detail}</code>
      </div>
      <div className="perm-actions">
        {inTerminal ? (
          <button
            className="btn btn-primary"
            title="Mirante does not type this answer: answer the dialog in Claude's own terminal."
            onClick={onAnswerInTerminal}
          >
            Answer in terminal
          </button>
        ) : (
          <>
            <button className="btn btn-primary" disabled={busy} onClick={() => answer(true)}>
              Allow once
            </button>
            <button className="btn" disabled={busy} onClick={() => answer(false)}>
              Deny
            </button>
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
