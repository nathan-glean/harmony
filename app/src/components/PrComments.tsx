import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import { MarkdownView } from "./MarkdownView";
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
// Trim a GitHub `diff_hunk` to just the commented line(s) plus a few lines of context. The hunk
// always ends at the commented line, so we drop the `@@` header and keep the tail.
const HUNK_CONTEXT = 4;
function hunkWindow(hunk: string): string[] {
  const lines = hunk.split("\n").filter((l) => !l.startsWith("@@"));
  while (lines.length && lines[lines.length - 1].trim() === "") lines.pop();
  if (lines.length <= HUNK_CONTEXT + 1) return lines;
  return ["⋯", ...lines.slice(lines.length - (HUNK_CONTEXT + 1))];
}

// Drop a leading unified-diff marker (+/-/space) from a hunk line.
function stripMarker(l: string): string {
  return l.length > 0 && (l[0] === "+" || l[0] === "-" || l[0] === " ") ? l.slice(1) : l;
}

// Extract ```suggestion blocks from a comment body → arrays of suggested replacement lines.
function parseSuggestions(body: string): string[][] {
  const out: string[][] = [];
  const re = /```suggestion[^\n]*\n([\s\S]*?)```/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body))) out.push(m[1].replace(/\n$/, "").split("\n"));
  return out;
}

// The comment prose with the ```suggestion blocks removed (rendered separately as a diff).
function stripSuggestions(body: string): string {
  return body.replace(/```suggestion[^\n]*\n[\s\S]*?```/g, "").trim();
}

// Rows for a "Suggested change" diff: the commented original line(s) removed, the suggestion added.
// The commented line(s) are the last `line - start_line + 1` lines of the diff_hunk.
function suggestionRows(c: PrComment, suggestion: string[]): { sign: string; text: string }[] {
  const hl = c.diff_hunk.split("\n").filter((l) => !l.startsWith("@@"));
  while (hl.length && hl[hl.length - 1].trim() === "") hl.pop();
  const n = c.start_line > 0 && c.start_line <= c.line ? c.line - c.start_line + 1 : 1;
  const original = hl.slice(Math.max(0, hl.length - n)).map(stripMarker);
  return [
    ...original.map((t) => ({ sign: "-", text: t })),
    ...suggestion.map((t) => ({ sign: "+", text: t })),
  ];
}

// The anchor string a flagged PR comment is stored under (must match on add + lookup).
function anchorFor(c: PrComment): string {
  const snippet = c.body.trim().replace(/\s+/g, " ").slice(0, 100);
  return `${c.author || "unknown"}: "${snippet}"`;
}

export function PrComments({
  ticketId,
  version,
  onCommentAdded,
}: {
  ticketId: number;
  version: number;
  onCommentAdded: () => void;
}) {
  const [comments, setComments] = useState<PrComment[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [flagging, setFlagging] = useState<number | null>(null);
  const [note, setNote] = useState("");
  // anchor → status of the matching local pr_comment feedback ("open" = queued, "sent" = sent).
  const [flagged, setFlagged] = useState<Record<string, string>>({});

  // `silent` background refreshes (the auto-poll) don't toggle the loading indicator or surface
  // transient errors, so the list updates without flicker.
  const load = useCallback(
    async (silent = false) => {
      if (!silent) {
        setLoading(true);
        setErr(null);
      }
      try {
        setComments(await api.ticketPrComments(ticketId));
      } catch (e) {
        if (!silent) setErr(String(e));
      } finally {
        if (!silent) setLoading(false);
      }
    },
    [ticketId]
  );

  // Which PR comments are flagged: match local pr_comment feedback by anchor. Open beats sent
  // beats resolved when an anchor has been flagged more than once.
  const loadFlagged = useCallback(async () => {
    try {
      const local = await api.listDiffComments(ticketId);
      const rank: Record<string, number> = { open: 3, sent: 2, resolved: 1 };
      const map: Record<string, string> = {};
      for (const c of local) {
        if (c.target !== "pr_comment" || !c.anchor) continue;
        if (!map[c.anchor] || (rank[c.status] ?? 0) > (rank[map[c.anchor]] ?? 0)) {
          map[c.anchor] = c.status;
        }
      }
      setFlagged(map);
    } catch {
      /* best-effort */
    }
  }, [ticketId]);

  useEffect(() => {
    load();
  }, [load]);

  // Auto-grab new PR comments: refresh quietly every 60s while the modal is open (skip when the
  // window is hidden to avoid pointless `gh` calls in the background).
  useEffect(() => {
    const id = setInterval(() => {
      if (!document.hidden) load(true);
    }, 60000);
    return () => clearInterval(id);
  }, [load]);

  useEffect(() => {
    loadFlagged();
  }, [loadFlagged, version]);

  // Add a local "pr_comment"-target note for Claude, anchored to this PR comment.
  const flagForClaude = async (c: PrComment) => {
    try {
      await api.addComment(ticketId, "pr_comment", anchorFor(c), note);
      setFlagging(null);
      setNote("");
      await loadFlagged();
      onCommentAdded();
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div className="pr-comments">
      <div className="pr-comments-head">
        <span>PR comments</span>
        <button onClick={() => load()} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </button>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {comments.length > 0 ? (
        comments.map((c, i) => {
          const flagStatus = flagged[anchorFor(c)];
          const isFlagged = flagStatus === "open" || flagStatus === "sent";
          const suggestions = parseSuggestions(c.body);
          const bodyProse = suggestions.length ? stripSuggestions(c.body) : c.body;
          return (
          <div key={i} className={"comment-card" + (isFlagged ? " flagged" : "")}>
            <div className="comment-head">
              <span className="comment-author">{c.author || "unknown"}</span>
              {c.priority && <span className={"prio-badge " + c.priority}>{c.priority}</span>}
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
              {flagStatus === "open" && <span className="comment-status queued">queued for Claude</span>}
              {flagStatus === "sent" && <span className="comment-status sent">sent to Claude</span>}
              <span className="comment-time muted">{timeAgo(c.created_at)}</span>
              {c.url && (
                <a className="comment-link" href={c.url} target="_blank" rel="noreferrer">
                  ↗
                </a>
              )}
            </div>
            {c.kind === "inline" && c.diff_hunk && (
              <pre className="diff-hunk">
                {hunkWindow(c.diff_hunk).map((ln, j) => (
                  <div
                    key={j}
                    className={
                      "dh-line " +
                      (ln === "⋯"
                        ? "hunk"
                        : ln.startsWith("+")
                        ? "add"
                        : ln.startsWith("-")
                        ? "del"
                        : "")
                    }
                  >
                    {ln || " "}
                  </div>
                ))}
              </pre>
            )}
            {bodyProse.trim() ? (
              <div className="comment-body comment-md">
                <MarkdownView markdown={bodyProse} />
              </div>
            ) : suggestions.length === 0 ? (
              <div className="comment-body muted">(no description)</div>
            ) : null}
            {suggestions.map((s, si) => (
              <div key={si} className="suggested-change">
                <div className="suggested-change-label">Suggested change</div>
                <pre className="diff-hunk">
                  {suggestionRows(c, s).map((r, j) => (
                    <div key={j} className={"dh-line " + (r.sign === "+" ? "add" : "del")}>
                      {r.sign}
                      {r.text || " "}
                    </div>
                  ))}
                </pre>
              </div>
            ))}
            {!isFlagged && (
              <div className="comment-actions">
                {flagging === i ? null : (
                  <button onClick={() => { setFlagging(i); setNote(""); }}>Flag for Claude</button>
                )}
              </div>
            )}
            {flagging === i && !isFlagged && (
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
          );
        })
      ) : (
        !loading && <p className="empty">No PR comments yet.</p>
      )}
    </div>
  );
}
