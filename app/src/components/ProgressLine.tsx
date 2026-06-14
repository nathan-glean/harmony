import type { SessionProgress } from "../types";

// A live "latest progress" line sourced from the session transcript: the current tool as a
// chip plus the latest assistant message. Used on board cards (compact) and in the detail
// panel. Renders nothing when there's no progress yet.
export function ProgressLine({ p, className }: { p: SessionProgress; className?: string }) {
  const text = p.message?.trim();
  if (!text && !p.tool) return null;
  return (
    <div className={className ?? "progress-line"}>
      {p.tool && <span className="progress-tool">⏺ {p.tool}</span>}
      {text && <span className="progress-msg">{text}</span>}
    </div>
  );
}
