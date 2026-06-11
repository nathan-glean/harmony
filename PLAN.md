# harmony — Implementation Plan

Companion to [`DESIGN.md`](./DESIGN.md). `DESIGN.md` is the *what/why* (the 14 resolved
decisions); this is the *how / in what order*. Build is phased so the riskiest assumption
is proven before any UI is built.

## Stack (confirmed)
- **Shell**: Tauri 2 (Rust core + web frontend, single process).
- **Core**: Rust — `tokio` (async), `portable-pty` (PTY), `axum` (local hook server),
  `sqlx` + SQLite (store), `reqwest` (Jira Cloud REST v3), `git2` or shelling `git`
  (worktrees), shelling `gh` (PRs), `notify-rust` / Tauri notifications, `keyring` (secrets).
- **Frontend**: **React + TypeScript** (Vite), with `xterm.js` for embedded terminals.
- **Store**: **SQLite** at `~/.harmony/harmony.db`.
- **Jira**: **Cloud**, email + API token. **Forge**: **GitHub** via `gh`.

---

## Phase 0 — De-risking spike (DO THIS FIRST)

**Goal:** prove the single assumption the whole design leans on — that Claude Code's
**HTTP hooks fire for an interactive (PTY) session** and can both (a) report state and
(b) return a permission decision — *before* building anything else.

### Task 0.1 — Hook side-channel proof
**Steps**
1. Minimal Rust (or even a throwaway Node/Python) HTTP listener on `127.0.0.1:<port>`
   that logs every POST body.
2. In a scratch git repo, write `.claude/settings.json` with HTTP hooks for
   `SessionStart`, `PreToolUse`, `PermissionRequest`, `Stop`, `SessionEnd`, `Notification`,
   all pointing at the listener.
3. Spawn `claude` **interactively inside a PTY** (`portable-pty`), `cwd` = that repo.
4. Drive a prompt that triggers a tool use needing permission (e.g. "create and run a
   shell script") in **default** permission mode.

**Pass criteria**
- [ ] `SessionStart` POST arrives with a `session_id`.
- [ ] A `PreToolUse`/`PermissionRequest` POST arrives **before** the tool runs, carrying
      the tool name + input (the proposed command/diff).
- [ ] Returning `{"permissionDecision":"deny"}` (or `allow`) from the hook **actually
      controls** the interactive session without anyone typing in the TUI.
- [ ] `Stop` fires at end of turn; `SessionEnd` on exit.
- [ ] The same `session_id` matches a JSONL file under `~/.claude/projects/<hash>/`.

**If it fails** (hooks don't fire interactively, or can't return decisions): fall back to
the Q13 "notify + jump-to-terminal" path *without* programmatic decisions (badge from
`Stop`/`Notification` only, user always answers in the terminal), and shelve the triage-UI
north star. Decide this before Phase 3.

### Task 0.2 — Resume + transcript fidelity
- [ ] Confirm `claude --resume <id>` (cwd = same worktree) continues the session.
- [ ] Confirm the JSONL transcript can be parsed line-by-line into a readable
      conversation view (basis for rebuilding terminal scrollback after relaunch).

### Task 0.3 — Auth/quota sanity
- [ ] Run 3 concurrent interactive sessions under one `/login`; confirm they coexist and
      observe quota behavior. Note any rate-limit signal in events/transcript.

**Exit gate:** Phase 0 green → proceed. Any red → revisit the affected decision in
`DESIGN.md` before writing app code.

### Phase 0 findings (validated 2026-06-11, Claude Code v2.1.173) — GREEN
- ✅ **Interactive PTY hooks fire**: `PreToolUse`, `PostToolUse`, `Stop` all fire for a
  PTY-hosted interactive session using project `.claude/settings.json` HTTP hooks.
- ✅ **Programmatic control works**: a `PreToolUse` response of
  `{"permissionDecision":"allow"}` auto-approved a Write in `default` mode **with no TUI
  prompt** (file written + read back; verified on disk). → v2 triage UI is feasible.
- ✅ **Transcript**: `~/.claude/projects/<hash>/<session_id>.jsonl` written and tailable.
- 📌 **Payload field is `tool_name`, not `tool`** (and `tool_input` carries the args).
  The production hook parser must use `tool_name`.
- 📌 **`SessionStart`/`SessionEnd` hooks did NOT fire** for the externally-injected
  interactive session. **Not a blocker** — harmony owns PTY spawn (= session start) and
  detects child-process exit (= session end) directly, so it does not depend on those two
  hooks for lifecycle. Use `PreToolUse`/`PostToolUse`/`Stop`/`Notification` for in-session
  state and process spawn/exit for start/end.
- 📌 **`PermissionRequest` does not fire separately** once `PreToolUse` returns a decision
  — key the permission path off `PreToolUse`.

---

## Phase 1 — Core engine (headless, no UI)
Build and unit/integration-test the Rust core behind a thin CLI or tests.
- [ ] **Store**: SQLite schema — `tickets`, `repos`, `worktrees`, `sessions`, `settings`.
      Cardinality: ticket 1→N worktrees (default 1), worktree 1→1 branch, worktree 1→N
      sessions (resumes).
- [ ] **Repo registry**: register/list local repos; remember default repo per Jira project key.
- [ ] **Worktree manager**: create off fresh default branch at
      `~/.harmony/worktrees/<repo>/<branch>`, branch `harmony/<KEY|local-id>-<slug>`;
      list; remove. "Alternate attempt" = second worktree for same ticket.
- [ ] **Hook server** (`axum`): localhost, per-session shared-secret token; routes
      hook events → session state machine (Working/Waiting/Error/Done).
- [ ] **Settings injector**: write per-worktree `.claude/settings.json` with hook URLs+token.
- [ ] **Session manager**: spawn `claude` in PTY (cwd=worktree); first prompt = rendered
      spec template; resume via `--resume`; autonomy = elevated `--permission-mode`.
- [ ] **Transcript tailer**: tail JSONL → progress/state corroboration.

## Phase 2 — Integrations
- [ ] **Jira client**: Cloud REST v3; auth via keychain (email+token). Read
      assignee=currentUser() open issues → tickets. Writeback (opt-in, toggle each):
      transition (start→In Progress, PR→In Review) + PR-link comment. **Discover valid
      transition IDs per issue** (don't hardcode names).
- [ ] **PR/gh**: push branch; `gh pr create --draft` with body from spec + optional
      Claude-generated summary; capture PR URL → feed Jira writeback + board.
- [ ] **Spec "Draft from Jira"**: one-shot `claude -p` over Jira description (+ bounded,
      optional repo scan) → editable first-pass spec.

## Phase 3 — Desktop UI (Tauri + frontend)
- [ ] **Board**: native lifecycle columns Available → Ready → Working → Waiting →
      In Review → Done; Working/Waiting live from hook state.
- [ ] **Ticket detail + spec editor**: markdown body + fields (acceptance criteria,
      relevant paths, constraints); "Draft from Jira" button; repo picker (defaulted).
- [ ] **Embedded terminal**: xterm.js bound to the session PTY; resize/ANSI handling.
- [ ] **Attention**: card badges from hooks + OS notification when a backgrounded session
      starts Waiting; click → focus its terminal.
- [ ] **Diff / PR pane**: show worktree diff; PR status + link.
- [ ] **Resume-on-relaunch**: on startup, re-resume each active ticket's session and
      rebuild its terminal view from transcript.

## Phase 4 — Polish / hardening
- [ ] Soft "N running" concurrency indicator (no hard cap).
- [ ] Worktree GC (offer cleanup on PR merged/closed; manual "remove").
- [ ] Secret handling review (tokens in keychain, never logged).
- [ ] Error/edge states: session crash, `gh`/Jira failures, dirty worktree, network loss.

---

## Deferred to v2 (tracked, not built)
Native permission **triage UI** · tmux/daemon persistence for unattended runs · shared
**team backend** · JQL/board scope + pull-by-key · live **MCP tools** for the running
agent · cascade/auto-merge.

## Risk register
See `DESIGN.md` → "Sharp edges / risks". The dominant one (interactive hooks) is retired
by Phase 0; the rest (Jira transition discovery, hook injection robustness, resume
fidelity, draft-from-Jira cost) are addressed in their respective phases.

---

## Immediate next action
**Run Task 0.1.** Nothing else should be built until the hook side-channel is proven on
this machine with the installed Claude Code version.
