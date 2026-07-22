import { useEffect, useState } from "react";
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

  const submitPicked = () => {
    if (picked.size === 0) return; // Enter with nothing selected is a no-op
    relay([...picked].sort((a, b) => a - b), null);
  };

  // Multi-select has no confirm button: clicking options toggles them, Enter submits the
  // whole selection. The custom-answer field owns Enter while it's focused (it submits the
  // typed text), so ignore Enter when that input is the active element.
  useEffect(() => {
    if (!q.multiSelect) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Enter") return;
      const el = document.activeElement as HTMLElement | null;
      if (el?.dataset.qcCustom === "true") return;
      if (sending || picked.size === 0) return;
      e.preventDefault();
      submitPicked();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // submitPicked is recreated each render; picked/sending are the values it closes over.
  }, [q.multiSelect, picked, sending]);

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
        <div className="question-hint muted">
          {picked.size > 0
            ? `↵ Enter to send ${picked.size} selected`
            : "Click options to select, then press ↵ Enter to send"}
        </div>
      )}

      <div className="question-custom">
        <input
          type="text"
          data-qc-custom="true"
          placeholder="…or type your own answer"
          value={custom}
          disabled={sending}
          onChange={(e) => setCustom(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.stopPropagation();
              submitCustom();
            }
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
