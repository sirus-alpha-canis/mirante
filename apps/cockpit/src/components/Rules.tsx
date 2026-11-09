// "Rules": every instruction layer an agent session reads for a repo or a task (managed,
// your global files, the project's, local, nested, and what Mirante adds), in the order the
// agent loads them. Read-only. File contents are shown as plain text in <pre> (React text
// nodes; never Markdown rendered as HTML). Places where rules could live but do not are
// listed greyed, so a person learns where to look.
import { useEffect, useState } from "react";
import { useBoot, useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { type Key, useT } from "../lib/i18n";
import { type AgentKind, M, type RuleImport, type RuleLayer, type RuleScope, type RulesView, RpcCode } from "../lib/protocol";
import { ErrorRetry } from "./ErrorRetry";
import { Unsupported } from "./Restart";

export type RulesAt = { repo: string } | { task: string };

const SCOPES: RuleScope[] = ["managed", "user", "project", "local", "nested", "mirante"];
const SCOPE_KEY: Record<RuleScope, Key> = {
  managed: "rules.scope.managed",
  user: "rules.scope.user",
  project: "rules.scope.project",
  local: "rules.scope.local",
  nested: "rules.scope.nested",
  mirante: "rules.scope.mirante",
};
const KIND_KEY: Record<string, Key> = {
  instructions: "rules.kind.instructions",
  settings: "rules.kind.settings",
  skills: "rules.kind.skills",
  memory: "rules.kind.memory",
  mode: "rules.kind.mode",
};

/** "812 B", "4.2 KB". */
export function bytes(n: number): string {
  return n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
}

type Loaded = { state: "loading" } | { state: "unavailable" } | { state: "error"; error: string } | { state: "ready"; view: RulesView };

function Imports({ list, depth }: { list: RuleImport[]; depth: number }) {
  const t = useT();
  return (
    <ul className="rule-imports" aria-label={t("rules.importsList")}>
      {list.map((i, n) => (
        <ImportItem key={`${depth}-${n}-${i.path}`} imp={i} depth={depth} />
      ))}
    </ul>
  );
}

function ImportItem({ imp, depth }: { imp: RuleImport; depth: number }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const sub = imp.imports ?? [];
  return (
    <li className={imp.content === undefined ? "rule-off" : undefined}>
      <code className="rule-path">@{imp.spec}</code> <span className="muted small">→ {imp.path}</span>
      {imp.size !== undefined && <span className="muted small"> · {bytes(imp.size)}</span>}
      {!imp.exists && <span className="tag tag-muted">{t("rules.missing")}</span>}
      {imp.error && <p className="small rule-error">{imp.error}</p>}
      {(imp.content !== undefined || sub.length > 0) && (
        <button className="btn btn-small" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          {open ? t("rules.hide") : t("rules.show")}
        </button>
      )}
      {open && (
        <>
          {imp.content !== undefined && <pre className="rule-text">{imp.content}</pre>}
          {imp.truncated && <p className="muted small">{t("rules.truncated")}</p>}
          {sub.length > 0 && <Imports list={sub} depth={depth + 1} />}
        </>
      )}
    </li>
  );
}

function Layer({ l }: { l: RuleLayer }) {
  const t = useT();
  const [imports, setImports] = useState(false);
  const list = l.imports ?? [];
  const cls = ["rule", l.exists ? "" : "rule-missing", l.loaded ? "" : "rule-off"].filter(Boolean).join(" ");
  return (
    <li className={cls} aria-label={`${t(SCOPE_KEY[l.scope] ?? "rules.scope.project")} ${l.path}`}>
      <div className="rule-head">
        <span className={`tag rule-scope rule-scope-${l.scope}`}>{t(SCOPE_KEY[l.scope] ?? "rules.scope.project")}</span>
        <span className="tag tag-muted">{t(KIND_KEY[l.kind] ?? "rules.kind.instructions")}</span>
        <code className="rule-path">{l.path}</code>
        {l.size !== undefined && <span className="muted small">{bytes(l.size)}</span>}
        {l.kind === "skills" && l.exists && <span className="muted small">{t("rules.skillCount", { n: l.skills })}</span>}
        {!l.exists && <span className="tag tag-muted">{t("rules.missing")}</span>}
        {l.exists && !l.loaded && !l.error && <span className="tag tag-muted">{t("rules.notLoaded")}</span>}
      </div>
      {l.note && <p className="muted small rule-note">{l.note}</p>}
      {l.error && <p className="small rule-error">{l.error}</p>}
      {l.content !== undefined && l.content !== "" && (
        <details className="rule-body">
          <summary>{t("rules.content")}</summary>
          <pre className="rule-text">{l.content}</pre>
          {l.truncated && <p className="muted small">{t("rules.truncated")}</p>}
        </details>
      )}
      {list.length > 0 && (
        <>
          <button className="btn btn-small" aria-expanded={imports} onClick={() => setImports((o) => !o)}>
            {t("rules.imports", { n: list.length })}
          </button>
          {imports && <Imports list={list} depth={1} />}
        </>
      )}
    </li>
  );
}

/** The layers of `at` for one agent, with a Claude / Codex switch. */
export function Rules({ at, agent: initial }: { at: RulesAt; agent?: AgentKind }) {
  const { rpc } = useEnv();
  const t = useT();
  const boot = useBoot();
  const [agent, setAgent] = useState<AgentKind>(initial ?? "claude");
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const key = "repo" in at ? `r:${at.repo}` : `t:${at.task}`;
  useEffect(() => {
    let live = true;
    setLoaded({ state: "loading" });
    rpc<RulesView>(M.rulesLayers, { ...at, agent })
      .then((view) => live && setLoaded({ state: "ready", view }))
      .catch((e) => {
        if (!live) return;
        if (e instanceof DaemonError && e.code === RpcCode.METHOD_NOT_FOUND) setLoaded({ state: "unavailable" });
        else setLoaded({ state: "error", error: e instanceof Error ? e.message : String(e) });
      });
    return () => {
      live = false;
    };
    // `at` is named by `key` (a new object each render must not refetch).
  }, [rpc, key, agent, nonce, boot]);

  let body: React.ReactNode;
  if (loaded.state === "loading") body = <p className="muted">{t("rules.loading")}</p>;
  else if (loaded.state === "unavailable") body = <Unsupported text={t("rules.unavailable")} />;
  else if (loaded.state === "error") body = <ErrorRetry text={t("rules.error", { error: loaded.error })} onRetry={() => setNonce((n) => n + 1)} />;
  else {
    const { view } = loaded;
    const s = view.summary;
    const perTask = !view.task;
    body = (
      <>
        <p className="rules-summary" role="status">
          {t(view.agent === "codex" ? "rules.summary.codex" : "rules.summary.claude", {
            files: s.instruction_files,
            size: bytes(s.instruction_bytes),
            settings: s.settings_files,
            skills: s.skills,
          })}
        </p>
        <p className="muted small">{t("rules.researched", { version: view.researched, root: view.root })}</p>
        {SCOPES.map((scope) => {
          const ls = view.layers.filter((l) => l.scope === scope);
          if (ls.length === 0) return null;
          return (
            <section key={scope} className="dna-section">
              <h3>{scope === "mirante" && perTask ? t("rules.mirantePerTask") : t(SCOPE_KEY[scope])}</h3>
              <ul className="rule-list">
                {ls.map((l, i) => (
                  <Layer key={`${scope}-${i}-${l.path}`} l={l} />
                ))}
              </ul>
            </section>
          );
        })}
      </>
    );
  }
  return (
    <div className="rules">
      <div className="brain-bar">
        <span role="group" aria-label={t("rules.agent")} className="brain-pills">
          {(["claude", "codex"] as AgentKind[]).map((a) => (
            <button key={a} className="btn btn-small" aria-pressed={agent === a} onClick={() => setAgent(a)}>
              {t(a === "claude" ? "rules.agent.claude" : "rules.agent.codex")}
            </button>
          ))}
        </span>
        <span className="muted small">{t("rules.readOnly")}</span>
      </div>
      {body}
    </div>
  );
}
