import { useEffect, useRef, useState } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";
import { shouldShowJumpToLatest } from "../lib/terminalScroll";

type TermOutput = { session_id: number; data: string };

// A comfortable cross-platform monospace stack: prefer the crisp system mono, fall back through
// popular dev fonts, then the classic Menlo/Monaco, then the generic keyword.
const FONT_STACK =
  "'SF Mono', 'JetBrains Mono', 'Fira Code', 'Cascadia Code', Menlo, Monaco, 'Courier New', monospace";

// Deep, low-contrast theme tuned to the app's dark chrome; ANSI colours from the Tokyo-Night
// palette already used elsewhere in the UI (see styles.css accent/task colours).
const THEME = {
  background: "#0b0e14",
  foreground: "#cdd6f4",
  cursor: "#7aa2f7",
  cursorAccent: "#0b0e14",
  selectionBackground: "#283457",
  black: "#15161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#a9b1d6",
  brightBlack: "#414868",
  brightRed: "#f7768e",
  brightGreen: "#9ece6a",
  brightYellow: "#e0af68",
  brightBlue: "#7aa2f7",
  brightMagenta: "#bb9af7",
  brightCyan: "#7dcfff",
  brightWhite: "#c0caf5",
};

export function TerminalView({ sessionId }: { sessionId: number }) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const screenRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Xterm | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  // Latest searchOpen, read by the terminal's key handler (registered once) without re-mounting.
  const searchOpenRef = useRef(false);
  const [showJump, setShowJump] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  searchOpenRef.current = searchOpen;

  useEffect(() => {
    const screen = screenRef.current;
    const wrap = wrapRef.current;
    if (!screen || !wrap) return;

    const term = new Xterm({
      fontSize: 13,
      fontFamily: FONT_STACK,
      lineHeight: 1.2,
      theme: THEME,
      cursorBlink: true,
      // Deep history so long Claude runs stay scrollable.
      scrollback: 10000,
      // Required by the unicode11 addon (it uses xterm's proposed unicode API).
      allowProposedApi: true,
    });
    termRef.current = term;

    const fit = new FitAddon();
    const search = new SearchAddon();
    searchRef.current = search;
    term.loadAddon(fit);
    term.loadAddon(search);

    // Wide-char / emoji width correctness — must be loaded then activated.
    const unicode11 = new Unicode11Addon();
    term.loadAddon(unicode11);
    term.unicode.activeVersion = "11";

    term.open(screen);

    // The Session tabpanel mounts while hidden (`display:none`) — child effects run before the
    // parent effect that switches to the Session tab, so at mount the element is 0×0. The WebGL
    // renderer and `fit()` both throw against a zero-sized canvas, which (with no error boundary
    // above) blanked the whole app. So defer all sizing-dependent setup until the terminal is
    // actually measurable, and run it exactly once via `activate()`. The ResizeObserver below
    // fires when the tab becomes visible, driving activation then.
    let webgl: WebglAddon | null = null;
    let activated = false;
    const measurable = () => screen.clientWidth > 0 && screen.clientHeight > 0;

    const activate = () => {
      if (activated || !measurable()) return;
      activated = true;

      // WebGL renderer: smooth, flicker-free GPU rendering, with graceful fallback to the default
      // DOM renderer if WebGL is unavailable or its context is lost in the WKWebView.
      try {
        webgl = new WebglAddon();
        webgl.onContextLoss(() => {
          webgl?.dispose();
          webgl = null;
        });
        term.loadAddon(webgl);
      } catch {
        webgl = null; // WebGL unsupported — DOM renderer stays active.
      }

      try {
        fit.fit();
      } catch {
        /* not measurable yet — a later resize will fit */
      }
      term.focus(); // ready to steer immediately
      api.resize(sessionId, term.cols, term.rows).catch(() => {});

      // Reattaching to an idle Claude TUI shows blank until something changes (it only repaints on
      // demand). Force a single SIGWINCH-driven repaint by briefly shrinking a row and restoring it.
      // This fires as soon as the terminal is visible and shrinks rather than grows, so there's no
      // visible size bounce.
      api
        .resize(sessionId, term.cols, Math.max(1, term.rows - 1))
        .then(() => api.resize(sessionId, term.cols, term.rows))
        .catch(() => {});
    };

    const pushSize = () => {
      if (!measurable()) return;
      if (!activated) {
        activate();
        return;
      }
      try {
        fit.fit();
        api.resize(sessionId, term.cols, term.rows).catch(() => {});
      } catch {
        /* element not measurable yet */
      }
    };

    // If the tab is already active at mount, activate now; otherwise the ResizeObserver activates
    // on the first show.
    activate();

    const updateJump = () => {
      const b = term.buffer.active;
      setShowJump(shouldShowJumpToLatest({ baseY: b.baseY, viewportY: b.viewportY }, 1));
    };

    const onData = term.onData((d) => api.sendInput(sessionId, d).catch(() => {}));
    const onScroll = term.onScroll(updateJump);

    const unlisten = listen<TermOutput>("term-output", (e) => {
      if (e.payload.session_id === sessionId) {
        term.write(e.payload.data, updateJump);
      }
    });

    // Cmd+F opens search; Esc closes it (only when open). Returning false stops xterm from also
    // handling the key. searchOpen is read via a ref so this handler is registered exactly once.
    term.attachCustomKeyEventHandler((ev) => {
      if (ev.type === "keydown" && (ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "f") {
        ev.preventDefault();
        setSearchOpen(true);
        return false;
      }
      if (ev.type === "keydown" && ev.key === "Escape" && searchOpenRef.current) {
        setSearchOpen(false);
        term.focus();
        return false;
      }
      return true;
    });

    // Re-fit whenever the container changes size — window resize, sidebar toggle, tab switch,
    // transcript expand/collapse, question-card appear/disappear — not just window.resize.
    let debounce: ReturnType<typeof setTimeout> | undefined;
    const ro = new ResizeObserver(() => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        pushSize();
        updateJump();
      }, 50);
    });
    ro.observe(wrap);
    window.addEventListener("resize", pushSize);

    return () => {
      clearTimeout(debounce);
      ro.disconnect();
      onData.dispose();
      onScroll.dispose();
      unlisten.then((u) => u());
      window.removeEventListener("resize", pushSize);
      term.dispose();
      termRef.current = null;
      searchRef.current = null;
    };
  }, [sessionId]);

  // Focus the search field when it opens.
  useEffect(() => {
    if (searchOpen) inputRef.current?.focus();
  }, [searchOpen]);

  const runSearch = (query: string, opts?: { back?: boolean }) => {
    if (!query) return;
    const search = searchRef.current;
    if (!search) return;
    if (opts?.back) search.findPrevious(query);
    else search.findNext(query);
  };

  const jumpToLatest = () => {
    termRef.current?.scrollToBottom();
    setShowJump(false);
  };

  return (
    <div className="terminal" ref={wrapRef}>
      <div className="term-screen" ref={screenRef} />
      {showJump && (
        <button className="term-jump" onClick={jumpToLatest} title="Scroll to latest output">
          ↓ Jump to latest
        </button>
      )}
      {searchOpen && (
        <div className="term-search">
          <input
            ref={inputRef}
            className="term-search-input"
            placeholder="Search…"
            onKeyDown={(e) => {
              if (e.key === "Enter") runSearch(e.currentTarget.value, { back: e.shiftKey });
              else if (e.key === "Escape") {
                setSearchOpen(false);
                termRef.current?.focus();
              }
            }}
          />
          <button
            className="term-search-btn"
            title="Previous match (Shift+Enter)"
            onClick={() => inputRef.current && runSearch(inputRef.current.value, { back: true })}
          >
            ↑
          </button>
          <button
            className="term-search-btn"
            title="Next match (Enter)"
            onClick={() => inputRef.current && runSearch(inputRef.current.value)}
          >
            ↓
          </button>
          <button
            className="term-search-btn"
            title="Close (Esc)"
            onClick={() => {
              setSearchOpen(false);
              termRef.current?.focus();
            }}
          >
            ✕
          </button>
        </div>
      )}
    </div>
  );
}
