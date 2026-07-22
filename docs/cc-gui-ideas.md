# Ideas from CC-GUI

A review of [`genio-learn/CC-GUI`](https://github.com/genio-learn/CC-GUI) (all 76 merged PRs + the
codebase) for things worth adopting in harmony. Written 2026-07-22.

## What CC-GUI is, and how it differs from harmony

CC-GUI is a sibling **Tauri 2 desktop app** for Claude Code — same stack shape as harmony (Rust
backend + Vite/TS frontend, xterm terminals over a PTY). But the philosophy is the mirror image:

- **CC-GUI is a manual, terminal-first session manager.** It embeds
  [`claude-commander`](https://github.com/sizeak/claude-commander) as a library and drives
  tmux-backed sessions; the human runs and steers everything. Its investment is overwhelmingly in
  **UI/UX, interaction, terminal ergonomics, theming, and distribution**.
- **harmony is an autonomous board** (grill → implement → review → PR → merge) with an orchestrator,
  proof-of-work, auto CI-fix / review-loop / conflict-resolve, and a stuck-session watchdog. harmony
  is well **ahead on autonomy** — CC-GUI has none of that.

So the useful transfer is almost entirely **presentation, interaction, ops, and distribution** — not
the engine. Two caveats when reading the pointers below:

1. **Frontend language差**: CC-GUI is **plain-TS / direct-DOM** (no framework); harmony is **React**.
   The *concepts and architecture* port cleanly, but code can't be copy-pasted — it needs a React
   translation (hooks/components instead of DOM builders).
2. **Model fit**: a few CC-GUI features exist to juggle *many concurrent manual sessions* (split-pane
   terminals, `@path` file explorer). harmony's model is one autonomous session per ticket, so those
   rank lower — flagged below.

## Already in harmony (don't re-suggest)

Login-shell `PATH` fix (PR #10) · Vitest + CI (PR #9) · semver release script (`task release`) ·
Tauri auto-updater · activity pills · diff/review pane with inline comments · worktree-delete confirm
dialog · Jira sync. The suggestions below are gaps or clear upgrades on these.

---

## Tier 1 — high value, low effort, directly portable

### 1.1 Release via PR, not a push to `main`
CC-GUI's `scripts/release.sh` (PR #26) bumps versions on a `release-vX.Y.Z` branch, opens+merges a PR
with `gh`, fast-forwards main, then tags the merged commit (the tag triggers the build). harmony's
`scripts/release.mjs` commits to the current branch and expects you to push — which **breaks against a
branch-protected `main`**. Adopt the PR-based landing.
- Also copy its **loud preconditions** (on main, clean tree, `gh` present, not behind origin) and the
  **merge-retry loop** (PR mergeability lags after creation).
- **Lockfile trap (PR #8):** CI's `npm ci` validates `package-lock.json` strictly; a bump that edits
  `package.json` but not the lockfile drifts and fails CI. `release.mjs` already touches
  `package-lock.json`, but verify it stays in lockstep (regenerate via `npm install
  --package-lock-only`). — *~1 hr.*

### 1.2 Split CI into parallel jobs + a Husky pre-commit hook
CC-GUI's `ci.yml` runs `frontend` (typecheck+build), `rustfmt`, and `clippy` as **three independent
jobs** (faster feedback than harmony's single serial `task ci`). Its `.husky/pre-commit` runs
`cargo fmt --check` only when `.rs` is staged and `tsc` only when `.ts` is staged — catching the
90% case locally while leaving clippy to CI. Non-obvious gotcha it documents: clippy must `npm run
build` first so `dist/` exists for the tauri build script (harmony shares this). — *~1 hr total.*

### 1.3 UTF-8 locale fix (companion to the PATH fix harmony already has)
`main.rs`'s `ensure_utf8_locale()` defaults `LANG` from `defaults read -g AppleLocale` before spawning
children, so CLI tools emit real Nerd-Font glyphs instead of ASCII placeholders — the exact sibling
of the login-shell `PATH` fix harmony shipped in #10. Small, macOS-only, verbatim-portable. — *~20 min.*

### 1.4 Status chip: shape + color + word (accessibility upgrade to activity pills)
CC-GUI's `src/status.ts` encodes each state by **shape + color + word**, so one hue can serve two
states disambiguated by glyph (e.g. "Done" = warning + dot vs "Waiting" = warning + "?") — colorblind-
safe. harmony's activity pills are color+word only. Adopt the shape dimension (a dot vs a glyph) and
reserve one color (`--danger`) exclusively for "blocked". Pure presentation. — *~1–2 hrs.*

### 1.5 Itemized consequence checklist for destructive actions
CC-GUI's delete dialog (PR #70) replaces prose with a scannable checklist — `✕ Kills the running
agent`, `✕ Removes the worktree`, `✓ Keeps the branch <name>` — cut/keep glyphs + tones. harmony's
worktree-delete / ticket-delete confirmations would read far clearer this way. — *~1 hr.*

---

## Tier 2 — high value, moderate effort

### 2.1 GUI theming with a no-flash boot (highest-polish win)
CC-GUI (PR #1, refined in #60) has a genuinely nice theming architecture, all **portable**:
- A **semantic token contract** (~21 color keys + shape/elevation) defined **once** in a TS theme
  registry, with the CSS `:root` defaults *generated from it at build time* (via a Vite plugin) so
  they can't drift. Derived tokens use `color-mix()` so everything reskins together; consumers never
  write raw hex.
- **No-flash boot plugin** (`vite.config.ts`): injects a pre-paint script that reads the saved theme
  from `localStorage` and replays cached CSS vars before first paint — no theme flash on launch.
- **Drop-in custom JSON themes** (validated in the frontend, read raw in the backend), a live-preview
  picker, and one `Theme` object that themes chrome + xterm (`ITheme`) + Shiki syntax in lockstep.

harmony has a single hard-coded dark palette in `styles.css`. Even without user themes, adopting the
**token layer + `color-mix` derivations** is a strong maintainability/polish upgrade; full theming is
the stretch. — *token layer ~half day; full theming ~2–3 days.*

### 2.2 Command palette (`Cmd/Ctrl+K`)
CC-GUI's `palette.ts` is a **provider-based** fuzzy palette that unifies "jump to a session" and "run
a command" in one list (subsequence scorer with streak/position weighting, grouped rendering). For
harmony this maps to **jump-to-ticket + run-any-command** (open ticket, grill, request review, open
PR, toggle a setting, switch tab). High utility, fully portable (as a React component). — *~1 day.*

### 2.3 Help overlay (`?`) from a single action registry
CC-GUI centralizes keybindings as one `{action: {label, run, keys}}` registry that feeds **three**
consumers: the key dispatcher, the palette's shortcut glyphs, and the `?` help overlay (auto-generated
so it never drifts). harmony has ad-hoc key handling and no discoverability surface. Adopt the single-
registry pattern + a `?` overlay. Pairs naturally with 2.2. — *~half day (more if refactoring existing keys).*

### 2.4 Homebrew cask + Linux packaging
CC-GUI ships via `brew install --cask genio-learn/tap/cc-gui` and builds **AppImage + `.deb`** for
Linux in its matrix `release.yml`. Two lessons: the cask's `postflight` **auto-clears the quarantine
flag** (Homebrew removed `--no-quarantine` in 5.x — don't use it), and the `update-cask` job hashes
the built DMG and bumps the tap via a PAT (default `GITHUB_TOKEN` can't push cross-repo). harmony is
unsigned-DMG-only today; a cask makes install one command and Linux widens reach. — *~2–4 hrs (needs a
tap repo + PAT secret).*

### 2.5 First-run onboarding hero
CC-GUI's onboarding (PRs #63/#72) is a **flag-free**, derived hero ("Add a project → Start a session →
…") shown whenever the board is empty — no persisted "seen" state, so it self-heals. harmony's
first-run (no repos / no tickets) is currently blank. A derived hero ("Add a repo → New ticket → Grill
it") would smooth first use. — *~half day.*

### 2.6 Whole-frontend tests with a faked Tauri backend (`.iwft`)
Beyond unit tests (which harmony has), CC-GUI (PR #2, `plans/testing.md`) boots the real frontend in
headless Chromium and **fakes the entire Tauri IPC seam** with a stateful `TauriSimulator` (built on
`@tauri-apps/api/mocks` `mockIPC`, injected via `addInitScript`) — seed state, drive the UI, assert.
Their `plans/testing.md` is a near-complete blueprint and documents the sharp edges (mock the
`listen`/`emit` channel + PTY `Channel`; `mockWindows("main")`; emit the update event after every
mutating command). harmony has the identical IPC seam, so this ports directly and would give real
integration coverage of the board/flow. Note: real Tauri E2E is a dead end (no macOS WebKit
WebDriver) — the fake-backend layer is the top tier. — *days, but blueprint provided.*

### 2.7 Settings: single searchable nav
CC-GUI (PR #68) replaced settings tabs with one schema-driven, **searchable** list (query matches
category or any field label/desc). As harmony's settings toggles grow (auto-review, review-loop,
proof, auto-merge, conflict-resolve, orchestrator, …), a searchable schema-driven pane scales better
than a flat checkbox column. — *~half day.*

---

## Tier 3 — roadmap / needs adaptation

### 3.1 Multi-harness "programs list" (on-theme for harmony)
`docs/ideas/programs-list.md` (captured, not built): let a session launch under any harness —
`claude`, `codex`, `opencode`, or a bare shell — via a config list of `{label, command}` entries.
harmony already spawns the `claude` CLI directly, so a "programs list" is a natural fit and needs no
claude-commander. The doc flags the design tension to resolve up front: a model baked into the command
(`claude --model opus`) overlaps any separate per-session model field. — *moderate feature; spec ready.*

### 3.2 Terminal ergonomics (if the live terminal stays central)
Portable xterm upgrades from PRs #3/#4/#5/#44: **OSC-52 copy** via `ClipboardAddon` routed through the
Tauri clipboard plugin (makes drag-select inside Claude's TUI actually copy in WKWebView), a
**copy-on-select last-char fix**, **Cmd+Click to open links**, and **Shift+Enter → newline**. harmony
just fixed terminal *fit*; these are the next ergonomics layer. — *~half day; portable.*

### 3.3 Review-mode robustness
From PRs #10/#27/#35/#37: keep **orphaned inline comments reachable** when their file leaves the diff
(a two-tier fallback so a drifted comment can't become undeletable), **image diffs** (incl. a
self-contained juxtapose slider), **keyboard file nav** (↑/↓, Ctrl-P/N), and a **progress ring**
(`conic-gradient`). harmony's review/diff pane could adopt the orphaned-comment reachability + keyboard
nav (both portable); anchoring/apply-to-agent would need harmony's own backend equivalent. — *moderate.*

### 3.4 Design refresh (IBM Plex + refined phases)
CC-GUI's "Refined" phases (#59–#73) sharpen a Catppuccin-Mocha look with IBM Plex type and consistent
shape/elevation. If harmony does a visual pass, the bundled-font technique is worth copying: register
the terminal font under a distinct family name (so it doesn't shadow a user's installed copy) and
**inline the terminal woff2 as a `data:` URI** (WKWebView refuses `@font-face` over Tauri's asset
protocol in packaged builds — a real gotcha). — *presentation project.*

### 3.5 Session hibernation / wake
CC-GUI (PRs #47/#49) stops idle sessions and offers "Wake" (resume via `--resume`). harmony could add
its own idle-policy loop to free resources on long-lived sessions and resume on demand. Concept-
portable, but harmony would build the loop itself. — *moderate–high; only if many long-lived sessions.*

### 3.6 Ops playbook skills + opt-out telemetry
- **Skills**: CC-GUI ships project-scoped Claude Code skills — `update-claude-commander` (a repeatable
  engine-bump playbook with a module→UI impact table) and `run-app` (how to launch/verify the native
  app; notes Playwright can't drive the WKWebView — use `.iwft`). harmony could add an
  `/update-claude-cli` skill (review the CLI changelog between pinned/latest, check spawn/parse code,
  surface new flags) and a `run-app` skill. — *~1–2 hrs each.*
- **Telemetry**: CC-GUI has opt-out feature-usage telemetry with a strict schema (feature name + coarse
  env + config snapshot + resettable install id; **never** text/prompts/paths/args) honoring
  `DO_NOT_TRACK`. Building the pipeline is high-effort; the **schema + never-send list + `DO_NOT_TRACK`
  honoring** are the reusable, low-risk parts if harmony ever wants usage signal.

---

## Lower fit for harmony's model (noted, not recommended now)
- **Multi-pane split-screen terminals + terminal tabs** (PRs #43/#6) and the **keyboard file explorer
  that drops `@path` into the terminal** (PR #50) are built for juggling many concurrent *manual*
  sessions. harmony's autonomy model (one session per ticket, driven by the flow) gets less from them.
  One thing worth lifting even so: CC-GUI's **`draggable()` pointer-drag primitive** (`main.ts`) — it
  powers tab/card/column drag *without* HTML5 DnD, which Tauri's OS drag-drop handler swallows. If
  harmony adds any drag interaction beyond the board's current one, this is the robust approach.

## Suggested adoption order
1. Tier 1 wholesale (release-via-PR, split CI + Husky, UTF-8 locale, status-chip shape, delete
   checklist) — cheap, mostly ops, near-zero risk.
2. Command palette + help overlay + single action registry (2.2/2.3) — big daily-use UX win.
3. Theming token layer, then optionally full theming (2.1).
4. Homebrew cask + Linux (2.4) when ready to widen distribution.
5. `.iwft` integration tests (2.6) to lock in the board/flow behavior.
6. Roadmap items (multi-harness, terminal ergonomics, review robustness) as they become relevant.
