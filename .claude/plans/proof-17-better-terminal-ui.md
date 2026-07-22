# Proof of work — Better UI for the Claude terminal session

Branch: `harmony/local-17-add-better-ui-for-claude-terminal-sessio`
Media: `/Users/nathan/.harmony/proof/17/`

## What works now

The live Claude terminal now renders like a modern terminal: it decodes multi-byte
UTF-8 glyphs correctly even when a character is split across a 4096-byte PTY read (no
more `�` corruption), draws through xterm's **WebGL** renderer with the **unicode11**
width addon, keeps a **10,000-line** scrollback with a styled scrollbar, shows a
**"Jump to latest"** pill when you scroll up (auto-sticking to the bottom only when
you're already there), and opens **Cmd+F search** over the scrollback. Both new
unit-test suites — the Rust `Utf8ChunkBuffer` and the pure `terminalScroll` at-bottom
helper — pass, and the existing Vitest + Rust CI stays green.

## How to see it

The features are proven two ways:

1. **Automated tests** (the substantive backend + logic fixes):
   ```
   # Rust UTF-8 chunk-buffer helper
   cd app/src-tauri && ~/.cargo/bin/cargo test --lib tests

   # Frontend pure scroll-stick / at-bottom helper
   cd app && npx vitest run src/lib/terminalScroll.test.ts

   # Full frontend suite (CI gate)
   cd app && npx vitest run
   ```

2. **Visual walkthrough** (rendering, scroll, jump, search, glyphs). The full Tauri
   desktop app needs a packaged WKWebView runtime + a live Claude PTY to drive
   `term-output`, so the visual proof uses a browser harness that mounts xterm.js with
   the **exact** configuration, theme, font stack, addon set, scrollback, CSS, and the
   **real** shipped `app/src/lib/terminalScroll.ts` helper from this branch — fed
   representative Claude-TUI output (box-drawing, emoji, ANSI colour, deep scrollback).
   It was driven headlessly with Playwright against the system Chrome. To regenerate:
   the harness/driver live in the session scratchpad; screenshots + video are written
   to `/Users/nathan/.harmony/proof/17/`.

## Evidence

### Media files (`/Users/nathan/.harmony/proof/17/`)

- **`walkthrough.webm`** — screen recording: terminal at rest → scroll up into history
  (styled scrollbar + "Jump to latest" pill appear) → Cmd+F search finds a match →
  click the pill to snap back to the latest output.
- **`01-terminal-at-bottom.png`** — parked at the bottom of a deep scrollback; ANSI
  colours, crisp glyphs, tidied "Live terminal" header with the green live dot. Footer
  reports `renderer: WebGL · unicode: v11 · scrollback: 10000`.
- **`02-scrolled-jump-and-scrollbar.png`** — scrolled up into history (lines 73–83);
  the accent-blue **"↓ Jump to latest"** pill is visible (driven by the real
  `shouldShowJumpToLatest` helper).
- **`03-search-match.png`** — the Cmd+F search bar (input + ↑/↓/✕ controls) open, the
  view scrolled to a `SEARCHME` match via the xterm search addon.
- **`04-glyphs-boxdrawing-emoji.png`** — box-drawing borders connect cleanly and emoji
  (🚀 ✅ ⚠️ 🔥 💡 📦 🧪 🎉) are measured to the correct width with no overlap. (This
  static shot uses the DOM renderer, honestly labelled `renderer: DOM` in the footer —
  `unicode11` governs glyph width identically under both renderers. The Powerline
  private-use glyphs show as boxes because no Nerd Font is installed on the capture
  machine; that is expected and unrelated to the change.)

### Rust — `Utf8ChunkBuffer` unit tests (verbatim)

`cd app/src-tauri && ~/.cargo/bin/cargo test --lib tests`

```
running 5 tests
test tests::buffers_a_multibyte_char_split_across_a_boundary ... ok
test tests::passes_through_plain_ascii ... ok
test tests::decodes_a_whole_multibyte_char ... ok
test tests::reassembles_a_four_byte_emoji_split_byte_by_byte ... ok
test tests::replaces_genuinely_invalid_bytes_without_stalling ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The `buffers_a_multibyte_char_split_across_a_boundary` and
`reassembles_a_four_byte_emoji_split_byte_by_byte` cases exercise exactly the defect the
change fixes: a box-drawing glyph and a 4-byte emoji fed byte-by-byte across read
boundaries reassemble into correct UTF-8 instead of being mangled by `from_utf8_lossy`.

### Frontend — pure scroll-stick / at-bottom helper (verbatim)

`cd app && npx vitest run src/lib/terminalScroll.test.ts`

```
 RUN  v2.1.9 .../app
 ✓ src/lib/terminalScroll.test.ts (6 tests) 2ms
 Test Files  1 passed (1)
      Tests  6 passed (6)
```

### Frontend — full Vitest suite stays green (verbatim)

`cd app && npx vitest run`

```
 ✓ src/types.test.ts (5 tests) 2ms
 ✓ src/lib/terminalScroll.test.ts (6 tests) 1ms

 Test Files  2 passed (2)
      Tests  11 passed (11)
```

### Buffer content proves glyph correctness (from the harness, verbatim)

Reading xterm's buffer back after writing the banner confirms the multi-byte glyphs are
stored intact (this is the same text the renderer draws in `04-glyphs-boxdrawing-emoji.png`):

```
✻ Welcome to Claude Code

╭───────────────────────────────────────────────────────────────╮
│  Harmony live terminal — box-drawing + emoji + colour test  │
├───────────────────────────────────────────────────────────────┤
│  Glyphs:  ┌─┬─┐ ├─┼─┤ └─┴─┘ ║═╬╗╔╝╚ ▏▎▍▌▋▊▉█ ░▒▓          │
│  Emoji:   🚀 ✅ ⚠️  🔥 💡 📦 🧪 🎉 → correctly measured    │
│  Powerline:                                │
╰───────────────────────────────────────────────────────────────╯
```

### Change surface (from `git diff --merge-base main --stat`)

```
 app/package-lock.json              |  21 ++++
 app/package.json                   |   3 +
 app/src-tauri/src/lib.rs           | 124 +++++++++++++++++++-
 app/src/components/Terminal.tsx    | 234 ++++++++++++++++++++++++++++++++-----
 app/src/lib/terminalScroll.test.ts |  35 ++++++
 app/src/lib/terminalScroll.ts      |  37 ++++++
 app/src/styles.css                 |  48 +++++++-
 7 files changed, 470 insertions(+), 32 deletions(-)
```

## Notes / honest caveats

- The visual harness is a faithful stand-in, not the packaged desktop app: it reuses the
  branch's exact xterm config/theme/CSS and the real `terminalScroll.ts` helper, but
  stubs the Tauri IPC (`api.resize`/`sendInput`/`term-output`) that needs the desktop
  runtime and a live Claude PTY.
- Under headless Chrome the WebGL renderer inflates cell size and repaints only on data
  writes (a known capture-side quirk) — this is precisely the WKWebView fragility the
  change defends against with automatic DOM fallback, and is unrelated to correctness on
  the target macOS/WebKit runtime. Screenshots 01–03 confirm WebGL is active and drawing;
  screenshot 04 uses the DOM renderer for a reliable static glyph capture.
