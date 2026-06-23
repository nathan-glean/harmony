import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api";
import { MarkdownView } from "./MarkdownView";
import type { CommentTarget, DiffComment } from "../types";

const TARGET_LABEL: Record<CommentTarget, string> = {
  general: "General",
  diff: "Diff",
  review: "On review",
  pr_comment: "PR comment",
};

// Cap a highlighted snippet stored as the comment anchor (keeps the feedback prompt tidy).
const MAX_ANCHOR = 280;

type ReviewSel = { text: string; x: number; y: number };

/** Feedback hub in the Review tab: renders Claude's /review (with highlight-to-comment), takes
 * general comments, lists all pending feedback, and sends it to Claude to address. */
export function ReviewFeedback({
  ticketId,
  reviewText,
  version,
  onChanged,
}: {
  ticketId: number;
  reviewText: string;
  version: number;
  onChanged: () => void;
}) {
  const [comments, setComments] = useState<DiffComment[]>([]);
  const [draft, setDraft] = useState("");
  const [reviewSel, setReviewSel] = useState<ReviewSel | null>(null);
  const [reviewAnchor, setReviewAnchor] = useState<string | null>(null);
  const [reviewDraft, setReviewDraft] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const reviewRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    try {
      setComments(await api.listDiffComments(ticketId));
    } catch {
      /* best-effort */
    }
  }, [ticketId]);

  useEffect(() => {
    load();
  }, [load, version]);

  const open = comments.filter((c) => c.status === "open");

  const add = async (target: CommentTarget, anchor: string, body: string, clear: () => void) => {
    const text = body.trim();
    if (!text) return;
    setErr(null);
    try {
      await api.addComment(ticketId, target, anchor, text);
      clear();
      await load();
      onChanged();
    } catch (e) {
      setErr(String(e));
    }
  };

  const remove = async (id: number) => {
    await api.deleteDiffComment(id);
    await load();
    onChanged();
  };

  // Capture a highlight inside the rendered review → show a floating "Comment" button.
  const onReviewMouseUp = () => {
    const sel = window.getSelection();
    const text = sel?.toString().trim() ?? "";
    if (!text || !sel || sel.rangeCount === 0 || !reviewRef.current?.contains(sel.anchorNode)) {
      setReviewSel(null);
      return;
    }
    const rect = sel.getRangeAt(0).getBoundingClientRect();
    setReviewSel({ text: text.slice(0, MAX_ANCHOR), x: rect.right, y: rect.top });
  };

  const startReviewComment = () => {
    if (!reviewSel) return;
    setReviewAnchor(reviewSel.text);
    setReviewDraft("");
    setReviewSel(null);
  };

  return (
    <div className="review-feedback">
      {reviewText ? (
        <div className="review-text" ref={reviewRef} onMouseUp={onReviewMouseUp}>
          <MarkdownView markdown={reviewText} />
        </div>
      ) : (
        <p className="empty">
          No review yet — press “Request review” (or move the ticket through review) to have Claude
          run <code>/review</code> and generate one.
        </p>
      )}

      {reviewSel && (
        <button
          className="review-comment-btn"
          style={{ position: "fixed", top: reviewSel.y - 34, left: reviewSel.x }}
          onMouseDown={(e) => e.preventDefault()} // keep the selection while clicking
          onClick={startReviewComment}
        >
          💬 Comment
        </button>
      )}

      <div className="pr-comments-head">
        <span>Feedback for Claude</span>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {reviewAnchor !== null && (
        <div className="comment-composer">
          <div className="review-anchor-quote">Commenting on: “{reviewAnchor}”</div>
          <textarea
            autoFocus
            placeholder="Your comment on this part of the review…"
            value={reviewDraft}
            onChange={(e) => setReviewDraft(e.target.value)}
          />
          <div className="comment-composer-actions">
            <button
              className="primary"
              disabled={!reviewDraft.trim()}
              onClick={() =>
                add("review", reviewAnchor, reviewDraft, () => {
                  setReviewDraft("");
                  setReviewAnchor(null);
                })
              }
            >
              Add review comment
            </button>
            <button onClick={() => { setReviewAnchor(null); setReviewDraft(""); }}>Cancel</button>
          </div>
        </div>
      )}

      <div className="comment-composer">
        <textarea
          placeholder="Add a general comment or suggestion for Claude…"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
        <div className="comment-composer-actions">
          <button className="primary" disabled={!draft.trim()} onClick={() => add("general", "", draft, () => setDraft(""))}>
            Add comment
          </button>
          {reviewText && <span className="muted review-hint">…or highlight text in the review above to comment on it</span>}
        </div>
      </div>

      {open.length > 0 && (
        <div className="diff-comments">
          {open.map((c) => (
            <div key={c.id} className="comment-card">
              <div className="comment-head">
                <span className="pr-comment-kind">{TARGET_LABEL[c.target]}</span>
                {c.target === "diff" && (
                  <span className="comment-range">
                    {c.file_path}:{c.line}
                  </span>
                )}
                {c.anchor && c.target !== "diff" && <span className="comment-range">{c.anchor}</span>}
                <div className="comment-actions">
                  <button onClick={() => remove(c.id)}>Delete</button>
                </div>
              </div>
              <div className="comment-body">{c.body}</div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
