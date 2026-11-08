import { useId, useState } from "react";

/** The agent's last answer: two lines, expandable. Agent text: rendered as text only. */
export function LastAnswer({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const long = text.length > 160 || text.includes("\n");
  return (
    <div className="answer">
      <p id={id} className={`answer-text ${open ? "" : "answer-clamp"}`}>
        <span className="who">agent</span> {text}
      </p>
      {long && (
        <button type="button" className="btn btn-small btn-ghost" aria-expanded={open} aria-controls={id} onClick={() => setOpen((o) => !o)}>
          {open ? "less" : "more"}
        </button>
      )}
    </div>
  );
}
