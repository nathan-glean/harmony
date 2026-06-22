import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { CiTriage, Ticket } from "../types";

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

const VERDICT_LABEL: Record<string, string> = {
  pr_caused: "PR-caused",
  unrelated_infra: "Unrelated / infra",
  flaky: "Flaky",
  undetermined: "Undetermined",
};

function parseTriage(json: string): CiTriage | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as CiTriage;
  } catch {
    return null;
  }
}

const MAX_CI_FIX_ATTEMPTS = 3;

/** The Proof tab: the PR's CI check status plus harmony's auto-fix triage verdict. */
export function ProofPane({ ticket }: { ticket: Ticket }) {
  const ticketId = ticket.id;
  const [checks, setChecks] = useState<any[]>([]);
  const [open, setOpen] = useState(true);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [fixing, setFixing] = useState(false);
  const [autofix, setAutofix] = useState(true);

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
    api.getCiAutofix().then(setAutofix).catch(() => {});
  }, [load]);

  const triage = parseTriage(ticket.ci_triage);
  const attempts = ticket.ci_fix_attempts;

  const requestFix = async () => {
    setFixing(true);
    setErr(null);
    try {
      await api.requestCiFix(ticketId);
    } catch (e) {
      setErr(String(e));
    } finally {
      setFixing(false);
    }
  };

  const toggleAutofix = async () => {
    const next = !autofix;
    setAutofix(next);
    try {
      await api.setCiAutofix(next);
    } catch (e) {
      setAutofix(!next); // revert on failure
      setErr(String(e));
    }
  };

  return (
    <div className="proofpane">
      <div className="diffpane-head">
        <span>CI status</span>
        <div className="diffpane-actions">
          <label className="autofix-toggle" title="Automatically fix CI failures caused by this PR">
            <input type="checkbox" checked={autofix} onChange={toggleAutofix} /> Auto-fix
          </label>
          <button
            onClick={requestFix}
            disabled={fixing}
            title="Triage the PR's CI now and fix any failing checks (manual backup to auto-fix)"
          >
            {fixing ? "Checking…" : "Check & fix CI"}
          </button>
          <button onClick={load} disabled={loading}>
            {loading ? "Loading…" : "Refresh"}
          </button>
        </div>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {triage && triage.failing_checks.length > 0 && (
        <div className={"triage-card " + (triage.verdict?.category ?? "")}>
          <div className="triage-head">
            <span className={"triage-badge " + (triage.verdict?.category ?? "")}>
              {triage.verdict ? VERDICT_LABEL[triage.verdict.category] : "Triaging…"}
            </span>
            {triage.verdict && (
              <span className="triage-conf">{Math.round(triage.verdict.confidence * 100)}% confident</span>
            )}
            <span className="triage-attempts">
              {attempts > 0 ? `${attempts}/${MAX_CI_FIX_ATTEMPTS} auto-fix attempts` : "no auto-fix yet"}
            </span>
            <button className="triage-fix" onClick={requestFix} disabled={fixing}>
              {fixing ? "Starting…" : "Fix CI"}
            </button>
          </div>
          {triage.verdict?.rationale && <p className="triage-rationale">{triage.verdict.rationale}</p>}
          <p className="triage-reason muted">{triage.reason}</p>
          {attempts >= MAX_CI_FIX_ATTEMPTS && (
            <p className="triage-reason muted">
              Auto-fix attempt cap reached — left for a human. Use “Fix CI” to try again.
            </p>
          )}
        </div>
      )}

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
