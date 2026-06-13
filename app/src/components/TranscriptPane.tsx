import { useEffect, useState } from "react";
import { api } from "../api";

export function TranscriptPane({ ticketId }: { ticketId: number }) {
  const [text, setText] = useState("");
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .sessionTranscript(ticketId)
      .then((t) => {
        if (!cancelled) setText(t);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [ticketId]);

  if (!text) return null;

  const prompts = text.split("\n❯ ").length - (text.startsWith("❯ ") ? 0 : 1);

  return (
    <div className="transcript-pane">
      <button className="transcript-toggle" onClick={() => setOpen((o) => !o)}>
        {open ? "▾" : "▸"} Conversation so far{prompts > 0 ? ` (${prompts} prompts)` : ""}
      </button>
      {open && <pre className="transcript">{text}</pre>}
    </div>
  );
}
