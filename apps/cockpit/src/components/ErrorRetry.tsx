import { useT } from "../lib/i18n";

/** A failed load, said, with the way to try it again. */
export function ErrorRetry({ text, onRetry, small }: { text: string; onRetry(): void; small?: boolean }) {
  const t = useT();
  return (
    <p className={`error${small ? " small" : ""}`} role="alert">
      {text}{" "}
      <button type="button" className="btn btn-small" onClick={onRetry}>
        {t("common.retry")}
      </button>
    </p>
  );
}
