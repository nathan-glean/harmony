import { useState } from "react";
import { api } from "../api";
import type { PendingQuestion } from "../types";

/// Renders Claude's pending AskUserQuestion as an answerable card, so the user can click an
/// option (or type a custom answer) without dropping into the embedded terminal. The answer
/// is relayed to the live session over the PTY via `answer_question`.
///
/// v1 answers the FIRST question robustly; multi-question prompts show the rest as context
/// and fall back to the terminal for those (see plan risks).
export function QuestionCard({
  pq,
  onAnswered,
}: {
  pq: PendingQuestion;
  onAnswered: () => void;
}) {
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [custom, setCustom] = useState("");
  const [sending, setSending] = useState(false);

  const q = pq.questions[0];
  if (!q) return null;
  const extra = pq.questions.length - 1;

  const toggle = (i: number) => {
    setPicked((prev) => {
      const next = new Set(q.multiSelect ? prev : []);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  };

  const relay = async (selected: number[], customText: string | null) => {
    if (sending) return;
    setSending(true);
    try {
      await api.answerQuestion(
        pq.session_id,
        q.options.length,
        selected,
        customText,
        q.multiSelect
      );
      onAnswered(); // optimistic; backend also clears on PostToolUse
    } catch (e) {
      console.error(e);
    } finally {
      setSending(false);
    }
  };

  const submitCustom = () => {
    const text = custom.trim();
    if (text) relay([], text);
  };

  return (
    <div className="question-card">
      <div className="question-head">
        Claude is asking{q.header ? ` — ${q.header}` : ""}
      </div>
      <div className="question-text">{q.question}</div>

      <div className="question-options">
        {q.options.map((o, i) => (
          <button
            key={i}
            className={"question-option" + (picked.has(i) ? " picked" : "")}
            disabled={sending}
            onClick={() => (q.multiSelect ? toggle(i) : relay([i], null))}
          >
            <span className="question-option-label">{o.label}</span>
            {o.description && (
              <span className="question-option-desc">{o.description}</span>
            )}
          </button>
        ))}
      </div>

      {q.multiSelect && (
        <button
          className="question-confirm"
          disabled={sending || picked.size === 0}
          onClick={() => relay([...picked].sort((a, b) => a - b), null)}
        >
          Send {picked.size} selected
        </button>
      )}

      <div className="question-custom">
        <input
          type="text"
          placeholder="…or type your own answer"
          value={custom}
          disabled={sending}
          onChange={(e) => setCustom(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submitCustom();
          }}
        />
        <button disabled={sending || !custom.trim()} onClick={submitCustom}>
          Send
        </button>
      </div>

      {extra > 0 && (
        <div className="question-extra muted">
          +{extra} more question{extra > 1 ? "s" : ""} in this prompt — answer the rest in the
          terminal below.
        </div>
      )}
    </div>
  );
}
