import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import type {
  PendingQuestion,
  SessionProgress,
  SessionViewState,
  TranscriptBlock,
  TranscriptMessage,
} from "../types";
import { MarkdownView } from "./MarkdownView";
import { QuestionCard } from "./QuestionCard";
import { Tasks } from "./Tasks";
import { ProgressLine } from "./ProgressLine";

type TermOutput = { session_id: number; data: string };

// Carriage return submits a line to the TUI (exactly like pressing Enter in the terminal).
const KEY_ENTER = "\r";
// Escape interrupts Claude's current turn.
const KEY_ESC = "\x1b";

// A small, recognisable glyph per common tool; everything else gets the generic 🔧.
const TOOL_ICONS: Record<string, string> = {
  Read: "📖",
  Edit: "✏️",
  MultiEdit: "✏️",
  Write: "📝",
  NotebookEdit: "📝",
  Bash: "❯_",
  Grep: "🔍",
  Glob: "🔍",
  WebFetch: "🌐",
  WebSearch: "🌐",
  Task: "🤖",
  TodoWrite: "✅",
  AskUserQuestion: "❓",
  ExitPlanMode: "📋",
};
const toolIcon = (name: string) => TOOL_ICONS[name] ?? "🔧";

/// A collapsed one-line tool card (icon + name + target). Clicking reveals its captured output,
/// which is hidden by default so the conversation stays skimmable.
function ToolCard({
  name,
  summary,
  result,
}: {
  name: string;
  summary: string;
  result: { output: string; is_error: boolean } | undefined;
}) {
  const [open, setOpen] = useState(false);
  const hasResult = !!result && result.output.trim().length > 0;
  return (
    <div className={"tool-card" + (result?.is_error ? " tool-error" : "")}>
      <button
        className="tool-card-head"
        onClick={() => hasResult && setOpen((o) => !o)}
        title={hasResult ? "Show output" : undefined}
        style={{ cursor: hasResult ? "pointer" : "default" }}
      >
        <span className="tool-icon">{toolIcon(name)}</span>
        <span className="tool-name">{name}</span>
        {summary && <span className="tool-summary">{summary}</span>}
        {hasResult && <span className="tool-caret">{open ? "▾" : "▸"}</span>}
      </button>
      {open && hasResult && <pre className="tool-card-result">{result!.output}</pre>}
    </div>
  );
}

/// A user message. The first prompt of a session is huge (the whole task spec), so long user
/// text collapses behind a "show more" toggle to keep the top of the log tidy.
function UserMessage({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const long = text.length > 600;
  const shown = long && !open ? text.slice(0, 600).trimEnd() + " …" : text;
  return (
    <div className="friendly-user">
      <pre className="friendly-user-text">{shown}</pre>
      {long && (
        <button className="friendly-more" onClick={() => setOpen((o) => !o)}>
          {open ? "Show less" : "Show more"}
        </button>
      )}
    </div>
  );
}

/// The beginner-friendly, chat-style view of a live Claude session. Renders the JSONL transcript
/// as markdown + compact tool cards, and gives normal GUI affordances (steer box, interrupt, stop,
/// question cards) over the SAME PTY the raw terminal drives. It never parses ANSI — the
/// high-frequency `term-output` event is used purely as a debounced "transcript changed" trigger to
/// re-fetch the structured records.
export function FriendlySession({
  sessionId,
  ticketId,
  pendingQuestion,
  progress,
  todosJson,
  onAnswered,
  onNeedTerminal,
}: {
  sessionId: number;
  ticketId: number;
  pendingQuestion: PendingQuestion | null;
  progress: SessionProgress | undefined;
  todosJson: string;
  onAnswered: () => void;
  onNeedTerminal: () => void;
}) {
  const [messages, setMessages] = useState<TranscriptMessage[]>([]);
  const [viewState, setViewState] = useState<SessionViewState>("working");
  const [input, setInput] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  // Whether the log is scrolled to (near) the bottom, so we only auto-scroll when the user hasn't
  // deliberately scrolled up into history.
  const atBottomRef = useRef(true);

  const load = useCallback(async () => {
    try {
      const [msgs, vs] = await Promise.all([
        api.sessionMessages(ticketId),
        api.sessionViewState(ticketId),
      ]);
      setMessages(msgs);
      setViewState(vs);
    } catch {
      /* transcript not ready yet — a later dirty-signal retries */
    }
  }, [ticketId]);

  // Initial load + near-real-time refresh: re-fetch (debounced) whenever this session emits PTY
  // output. We deliberately ignore the payload bytes — it's only a "something changed" tick.
  useEffect(() => {
    load();
    let t: ReturnType<typeof setTimeout> | undefined;
    const un = listen<TermOutput>("term-output", (e) => {
      if (e.payload.session_id !== sessionId) return;
      clearTimeout(t);
      t = setTimeout(load, 350);
    });
    return () => {
      clearTimeout(t);
      un.then((u) => u());
    };
  }, [sessionId, load]);

  // Escape hatch: a plan-approval / permission-style prompt the friendly view can't model — hand
  // off to the raw terminal so the user can respond. A pending AskUserQuestion is modelled (the
  // QuestionCard handles it) so it must not trigger the switch.
  useEffect(() => {
    if (viewState === "exit_plan" && !pendingQuestion) onNeedTerminal();
  }, [viewState, pendingQuestion, onNeedTerminal]);

  // Map every tool_result back to its tool_use id so each card can reveal its own output.
  const resultsById = useMemo(() => {
    const m = new Map<string, { output: string; is_error: boolean }>();
    for (const msg of messages) {
      for (const b of msg.blocks) {
        if (b.type === "tool_result") {
          m.set(b.tool_use_id, { output: b.output, is_error: b.is_error });
        }
      }
    }
    return m;
  }, [messages]);

  // Keep the log pinned to the latest as it grows, unless the user scrolled up.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && atBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [messages, viewState]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  const send = () => {
    const text = input.trim();
    if (!text) return;
    api.sendInput(sessionId, text + KEY_ENTER).catch(() => {});
    setInput("");
  };

  const interrupt = () => api.sendInput(sessionId, KEY_ESC).catch(() => {});
  const stop = () => api.stopSession(sessionId).catch(() => {});

  const working = viewState === "working";

  return (
    <div className="friendly-session">
      <div className="friendly-messages" ref={scrollRef} onScroll={onScroll}>
        {messages.length === 0 && (
          <p className="empty">Waiting for Claude to start the conversation…</p>
        )}
        {messages.map((msg, mi) => {
          // A user message that is purely tool_result carriers is Claude's own machinery, not a
          // human turn — its results are shown inside the tool cards, so don't render a bubble.
          const isToolCarrier =
            msg.role === "user" && msg.blocks.every((b) => b.type === "tool_result");
          if (isToolCarrier) return null;
          return (
            <div key={mi} className={"friendly-msg " + msg.role}>
              {msg.blocks.map((b: TranscriptBlock, bi) => {
                if (b.type === "text") {
                  return msg.role === "assistant" ? (
                    <div key={bi} className="friendly-assistant">
                      <MarkdownView markdown={b.text} />
                    </div>
                  ) : (
                    <UserMessage key={bi} text={b.text} />
                  );
                }
                if (b.type === "tool_use") {
                  return (
                    <ToolCard
                      key={bi}
                      name={b.name}
                      summary={b.summary}
                      result={resultsById.get(b.id)}
                    />
                  );
                }
                return null; // tool_result blocks are surfaced via their tool card
              })}
            </div>
          );
        })}
      </div>

      {working && (
        <div className="friendly-working">
          <span className="friendly-spinner" />
          {progress ? <ProgressLine p={progress} /> : <span>Claude is working…</span>}
        </div>
      )}

      {pendingQuestion && <QuestionCard pq={pendingQuestion} onAnswered={onAnswered} />}

      {todosJson && <Tasks todosJson={todosJson} />}

      <div className="friendly-composer">
        <textarea
          className="friendly-input"
          placeholder="Message Claude — type to steer the session, ↵ to send (⇧↵ for a new line)"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        <div className="friendly-actions">
          <button className="friendly-send" disabled={!input.trim()} onClick={send}>
            Send
          </button>
          <button
            className="friendly-interrupt"
            title="Interrupt the current turn (sends Esc)"
            onClick={interrupt}
          >
            Interrupt
          </button>
          <button className="friendly-stop" title="End this session" onClick={stop}>
            Stop
          </button>
        </div>
      </div>
    </div>
  );
}
