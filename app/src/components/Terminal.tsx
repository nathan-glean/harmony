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

    // Fit the xterm to its container and push the fitted size to the PTY so Claude's alt-screen TUI
    // redraws to exactly the visible rows/cols — otherwise its bottom (the input line) is clipped.
    // Skip while the container is hidden/zero-size (e.g. the Session tab isn't active): fitting then
    // yields a bogus size. The ResizeObserver below refits once the box has a real size.
    const doFit = () => {
      if (!el.clientWidth || !el.clientHeight) return;
      try {
        fit.fit();
        api.resize(sessionId, term.cols, term.rows).catch(() => {});
      } catch {
        /* not measurable yet */
      }
    };

    doFit();
    term.focus(); // ready to steer immediately

    // Re-fit whenever the container's box changes — not just on window resize. During a live session
    // the siblings above the terminal (progress line, question card, task list, transcript) appear
    // and grow, shrinking the terminal with no window-resize event; without this the fit goes stale
    // and the TUI's bottom rows get clipped. Also covers the tab becoming visible (0 → real size).
    // Debounced via rAF to coalesce bursts and avoid the "ResizeObserver loop" warning.
    let raf = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(doFit);
    });
    ro.observe(el);

    // Reattaching to an idle Claude TUI shows blank until something changes (it only repaints on
    // demand). Nudge the PTY size (SIGWINCH) to force a full repaint into this fresh terminal. Only
    // when already visible — if still hidden, the observer's first real fit (0 → real size) is itself
    // a size change that triggers the repaint.
    const nudge = setTimeout(() => {
      if (!el.clientWidth || !el.clientHeight) return;
      api
        .resize(sessionId, term.cols, term.rows + 1)
        .then(() => api.resize(sessionId, term.cols, term.rows))
        .catch(() => {});
    }, 80);

    const onData = term.onData((d) => api.sendInput(sessionId, d).catch(() => {}));

    const unlisten = listen<TermOutput>("term-output", (e) => {
      if (e.payload.session_id === sessionId) term.write(e.payload.data);
    });

    window.addEventListener("resize", doFit);

    return () => {
      clearTimeout(nudge);
      cancelAnimationFrame(raf);
      ro.disconnect();
      onData.dispose();
      unlisten.then((u) => u());
      window.removeEventListener("resize", doFit);
      term.dispose();
    };
  }, [sessionId]);

  return <div className="terminal" ref={ref} />;
}
