import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { PrComment } from "../types";

const KIND_LABEL: Record<PrComment["kind"], string> = {
  conversation: "Comment",
  review: "Review",
  inline: "Inline",
};

const REVIEW_STATE_LABEL: Record<string, string> = {
  APPROVED: "approved",
  CHANGES_REQUESTED: "changes requested",
  COMMENTED: "commented",
};

// Best-effort relative time from an ISO8601 string (e.g. "3d ago"); falls back to the date.
function timeAgo(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const secs = Math.round((Date.now() - t) / 1000);
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  if (days < 7) return `${days}d ago`;
  return new Date(t).toLocaleDateString();
}

/** Read-only list of the GitHub PR's comments (conversation, review summaries, inline). Shown in
 * the Review tab below Claude's /review output. Each can be "flagged for Claude" — a local note
 * (target `pr_comment`) added to the feedback queue. */
export function PrComments({ ticketId, onCommentAdded }: { ticketId: number; onCommentAdded: () => void }) {
  const [comments, setComments] = useState<PrComment[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [flagging, setFlagging] = useState<number | null>(null);
  const [note, setNote] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      setComments(await api.ticketPrComments(ticketId));
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, [ticketId]);

  useEffect(() => {
    load();
  }, [load]);

  // Add a local "pr_comment"-target note for Claude, anchored to this PR comment.
  const flagForClaude = async (c: PrComment) => {
    const snippet = c.body.trim().replace(/\s+/g, " ").slice(0, 100);
    const anchor = `${c.author || "unknown"}: "${snippet}"`;
    try {
      await api.addComment(ticketId, "pr_comment", anchor, note);
      setFlagging(null);
      setNote("");
      onCommentAdded();
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div className="pr-comments">
      <div className="pr-comments-head">
        <span>PR comments</span>
        <button onClick={load} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </button>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {comments.length > 0 ? (
        comments.map((c, i) => (
          <div key={i} className="comment-card">
            <div className="comment-head">
              <span className="comment-author">{c.author || "unknown"}</span>
              <span className="pr-comment-kind">{KIND_LABEL[c.kind]}</span>
              {c.kind === "inline" && c.path && (
                <span className="comment-range">
                  {c.path}
                  {c.line ? `:${c.line}` : ""}
                </span>
              )}
              {c.kind === "review" && c.state && (
                <span className={"comment-status" + (c.state === "APPROVED" ? " sent" : "")}>
                  {REVIEW_STATE_LABEL[c.state] ?? c.state.toLowerCase()}
                </span>
              )}
              <span className="comment-time muted">{timeAgo(c.created_at)}</span>
              {c.url && (
                <a className="comment-link" href={c.url} target="_blank" rel="noreferrer">
                  ↗
                </a>
              )}
            </div>
            {c.body.trim() ? (
              <div className="comment-body">{c.body}</div>
            ) : (
              <div className="comment-body muted">(no description)</div>
            )}
            <div className="comment-actions">
              {flagging === i ? null : (
                <button onClick={() => { setFlagging(i); setNote(""); }}>Flag for Claude</button>
              )}
            </div>
            {flagging === i && (
              <div className="comment-composer">
                <textarea
                  placeholder={`Note for Claude about ${c.author || "this"}'s comment…`}
                  value={note}
                  onChange={(e) => setNote(e.target.value)}
                />
                <div className="comment-composer-actions">
                  <button
                    className="primary"
                    disabled={!note.trim()}
                    onClick={() => flagForClaude(c)}
                  >
                    Add to feedback
                  </button>
                  <button onClick={() => setFlagging(null)}>Cancel</button>
                </div>
              </div>
            )}
          </div>
        ))
      ) : (
        !loading && <p className="empty">No PR comments yet.</p>
      )}
    </div>
  );
}
