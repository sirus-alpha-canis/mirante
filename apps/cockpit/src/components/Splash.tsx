/** The Orb-Dot mark with its orbiting satellites: the launch splash's (index.html, whose
 * <style> holds these classes and their animation; still with reduced motion). `still`:
 * the same mark without motion (the top bar's, styled in styles.css). */
export function OrbMark({ still = false }: { still?: boolean }) {
  return (
    <svg className={still ? "orb orb-static" : "orb"} viewBox="0 0 80 80" aria-hidden="true">
      <circle className="orb-ring" cx="40" cy="40" r="28" />
      <g className="orb-sats">
        <line x1="40" y1="40" x2="40" y2="12" opacity="0.25" />
        <line x1="40" y1="40" x2="68" y2="46" opacity="0.15" />
        <line x1="40" y1="40" x2="14" y2="58" opacity="0.12" />
        <circle cx="40" cy="12" r="3" stroke="none" />
        <circle cx="68" cy="46" r="2.5" stroke="none" opacity="0.7" />
        <circle cx="14" cy="58" r="2.5" stroke="none" opacity="0.5" />
      </g>
      <circle className="orb-halo" cx="40" cy="40" r="10" />
      <circle className="orb-core" cx="40" cy="40" r="5" />
    </svg>
  );
}

/** Waiting with the launch splash's mark and a line saying what for (the daemon's first answer). */
export function Splash({ text }: { text: string }) {
  return (
    <div className="splash splash-inline" role="status" aria-live="polite" aria-label={text}>
      <OrbMark />
      <span className="splash-word">Mirante</span>
      <p className="splash-note">{text}</p>
    </div>
  );
}
