import { useEffect, useRef } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";

type TermOutput = { session_id: number; data: string };

export function TerminalView({ sessionId }: { sessionId: number }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const term = new Xterm({
      fontSize: 13,
      fontFamily: "Menlo, monospace",
      theme: { background: "#0b0e14", foreground: "#cdd6f4" },
      cursorBlink: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    term.focus(); // ready to steer immediately
    api.resize(sessionId, term.cols, term.rows).catch(() => {});

    // Reattaching to an idle Claude TUI shows blank until something changes (it only
    // repaints on demand). Nudge the PTY size (SIGWINCH) to force a full repaint into
    // this fresh terminal.
    const nudge = setTimeout(() => {
      api
        .resize(sessionId, term.cols, term.rows + 1)
        .then(() => api.resize(sessionId, term.cols, term.rows))
        .catch(() => {});
    }, 80);

    const onData = term.onData((d) => api.sendInput(sessionId, d).catch(() => {}));

    const unlisten = listen<TermOutput>("term-output", (e) => {
      if (e.payload.session_id === sessionId) term.write(e.payload.data);
    });

    const onResize = () => {
      try {
        fit.fit();
        api.resize(sessionId, term.cols, term.rows).catch(() => {});
      } catch {
        /* element not measurable yet */
      }
    };
    window.addEventListener("resize", onResize);

    return () => {
      clearTimeout(nudge);
      onData.dispose();
      unlisten.then((u) => u());
      window.removeEventListener("resize", onResize);
      term.dispose();
    };
  }, [sessionId]);

  return <div className="terminal" ref={ref} />;
}
