import { useCallback, useEffect, useState } from "react";
import { api } from "../api";

// gh's check buckets: pass | fail | pending | skipping | cancel
function checkClass(c: any): string {
  const b = String(c.bucket ?? c.state ?? "").toLowerCase();
  if (b.includes("pass") || b.includes("success")) return "ok";
  if (b.includes("fail") || b.includes("error")) return "bad";
  if (b.includes("pend")) return "pending";
  return "";
}

// One-line summary of CI check states for the collapsed checks header, e.g. "3 passed · 1 failed".
function checksSummary(checks: any[]): string {
  const counts = { ok: 0, bad: 0, pending: 0, other: 0 };
  for (const c of checks) {
    const k = checkClass(c);
    if (k === "ok") counts.ok++;
    else if (k === "bad") counts.bad++;
    else if (k === "pending") counts.pending++;
    else counts.other++;
  }
  const parts: string[] = [];
  if (counts.ok) parts.push(`${counts.ok} passed`);
  if (counts.bad) parts.push(`${counts.bad} failed`);
  if (counts.pending) parts.push(`${counts.pending} pending`);
  if (counts.other) parts.push(`${counts.other} other`);
  return `(${parts.join(" · ") || `${checks.length}`})`;
}

/** The Proof tab: the PR's CI check status (moved out of the Diff tab). Fetches the PR/checks for
 * the ticket's branch and renders them as a collapsible list. */
export function ProofPane({ ticketId }: { ticketId: number }) {
  const [checks, setChecks] = useState<any[]>([]);
  const [open, setOpen] = useState(true);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const p = await api.ticketPr(ticketId);
      setChecks(p.checks ?? []);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, [ticketId]);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="proofpane">
      <div className="diffpane-head">
        <span>CI status</span>
        <div className="diffpane-actions">
          <button onClick={load} disabled={loading}>
            {loading ? "Loading…" : "Refresh"}
          </button>
        </div>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {checks.length > 0 ? (
        <div className="checks-block">
          <button className="checks-toggle" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
            {open ? "▾" : "▸"} CI checks {checksSummary(checks)}
          </button>
          {open && (
            <ul className="checks">
              {checks.map((c, i) => (
                <li key={i} className={"chk " + checkClass(c)}>
                  <span className="chk-dot" />
                  {c.link ? (
                    <a href={c.link} target="_blank" rel="noreferrer">
                      {c.name}
                    </a>
                  ) : (
                    c.name
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        !loading && (
          <p className="empty">
            No CI checks yet — they appear once a PR is open and CI starts running.
          </p>
        )
      )}
    </div>
  );
}
