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
- [~] `SessionStart` POST arrives with a `session_id`. _(Did NOT fire for the injected
      interactive session — not a blocker; harmony owns PTY spawn. See findings.)_
- [x] A `PreToolUse`/`PermissionRequest` POST arrives **before** the tool runs, carrying
      the tool name + input (the proposed command/diff).
- [x] Returning `{"permissionDecision":"deny"}` (or `allow`) from the hook **actually
      controls** the interactive session without anyone typing in the TUI.
- [x] `Stop` fires at end of turn; ~~`SessionEnd` on exit~~ _(Stop ✅; SessionEnd did NOT
      fire — process-exit detection used instead. See findings.)_
- [x] The same `session_id` matches a JSONL file under `~/.claude/projects/<hash>/`.

**If it fails** (hooks don't fire interactively, or can't return decisions): fall back to
the Q13 "notify + jump-to-terminal" path *without* programmatic decisions (badge from
`Stop`/`Notification` only, user always answers in the terminal), and shelve the triage-UI
north star. Decide this before Phase 3.

### Task 0.2 — Resume + transcript fidelity — DONE (proven in Phase 1 + 3)
- [x] Confirm `claude --resume <id>` (cwd = same worktree) continues the session.
      _(Built + relied on in `session.rs` resume path and Phase 3 resume-on-relaunch.)_
- [x] Confirm the JSONL transcript can be parsed line-by-line into a readable
      conversation view (basis for rebuilding terminal scrollback after relaunch).
      _(Implemented as `session_transcript` → `TranscriptPane.tsx`.)_

### Task 0.3 — Auth/quota sanity — NOT DONE
- [ ] Run 3 concurrent interactive sessions under one `/login`; confirm they coexist and
      observe quota behavior. Note any rate-limit signal in events/transcript.
      _(Multi-session concurrency was later built — `live_sessions` map, several terminals
      at once — but the explicit quota/rate-limit observation spike was never recorded.)_

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

## Phase 1 — Core engine (headless, no UI)  — SCAFFOLDED ✅ (crate `core/`, builds + CRUD smoke-tested)
The Rust core behind a `harmony` CLI. Lives in `core/` (workspace member).
- [x] **Store** (`core/src/store.rs`): SQLite schema — `repos`, `tickets`, `worktrees`,
      `sessions`, `settings`; runtime sqlx queries (no DATABASE_URL needed at build).
      Cardinality: ticket 1→N worktrees (default 1, `is_alternate` for attempts),
      worktree 1→N sessions (resumes).
- [x] **Repo registry** (in store): register/list; `default_project_key` → default repo per
      Jira project key.
- [x] **Worktree manager** (`core/src/worktree.rs`): create off fresh default branch at
      `~/.harmony/worktrees/<repo>/<branch>`, branch `harmony/<KEY|local-id>-<slug>`;
      `create`/`remove`; reuse-or-create logic in the session manager. `create` prunes
      stale registrations and **reuses an existing branch** (so re-entering In Progress
      after a Done cleanup works — the branch/PR was kept).
- [x] **Hook server** (`core/src/hooks.rs`, `axum`): localhost; routes hook events,
      **correlates by `cwd`→worktree→session**, updates session + ticket state
      (Working/Waiting). Uses `tool_name` (Phase 0 finding). Supervised: returns no
      decision yet (autonomy = return `permissionDecision:allow`).
- [x] **Settings injector** (`core/src/settings.rs`): **merges** hooks into per-worktree
      `.claude/settings.local.json` (NOT `settings.json` — that's tracked; see finding).
      Idempotent; preserves repo + Claude-local entries.
- [x] **Session manager** (`core/src/session.rs`): spawn `claude` in PTY (cwd=worktree).
      **Fresh start** sends the rendered spec; **resume** uses `claude --resume <id>` to
      restore the real conversation and sends only a brief "Continue where you left off."
      nudge (no spec re-paste). Returns a handle exposing the PTY master; session-end =
      child process exit.
- [x] **CLI** (`core/src/main.rs`): `repo add/list`, `ticket add/list`, `start`, `serve`.

Deferred within Phase 1 (do alongside the UI / as needed):
- [ ] **Transcript tailer**: tail session JSONL → richer in-session progress.
- [ ] **Hook auth token** (shared secret in the injected settings; localhost-bind is the
      boundary for now) — Phase 4 hardening.
- [ ] **Structured spec fields** (acceptance criteria / paths / constraints as columns) —
      add when the UI editor lands; currently one `spec` markdown blob.
- [ ] Unit/integration tests around store + worktree + cwd-correlation.

**Try it:**
```bash
cargo build -p harmony-core
target/debug/harmony repo add <name> <path-to-a-git-repo> --project PROJ
target/debug/harmony ticket add --title "…" --key PROJ-1 --spec "…" --repo <name>
target/debug/harmony ticket list
target/debug/harmony start <ticket_id>   # creates worktree, injects hooks, spawns claude
```

## Phase 2 — Integrations  — SCAFFOLDED ✅ (builds + CLI/error paths smoke-tested; live calls untested)
- [x] **Jira via `acli`** (`core/src/jira.rs`): shell-outs to the official Atlassian CLI
      (like `gh`). `search_assigned` (JQL), `get_issue`, `transition`, `add_comment`;
      defensive JSON parsing (acli's `--json` schema isn't documented — verify mapping on
      first real run). Auth is acli's own (`acli jira auth login`); harmony stores no Jira
      creds. **Board columns drive Jira**: moving a Jira-linked ticket to Todo/In Progress/
      In PR Review/Done transitions the issue if that status exists in its workflow
      (`jira_apply_column` → `transition_to_any`, best-effort); PR open posts the PR-link
      comment. **Auto-sync**: while connected the app polls `jira_sync` every 60s (silent,
      non-overlapping) + once on connect/launch; manual "Sync Jira" still works. CLI:
      `jira login`/`logout`/`status`/`sync`. App: status/logout +
      a Connect panel that points to `acli jira auth login` and re-checks.
      _(Replaced the earlier OAuth-REST + keychain approach — see DESIGN.)_
- [x] **acli install affordance**: `cli_installed()` detection (PATH-augmented for macOS
      GUI apps that omit `/opt/homebrew/bin`) + `install_via_brew()`. CLI: `harmony jira
      install`. App: Connect panel detects "not installed" → shows brew commands + an
      "Install with Homebrew" button + manual-install link.
- [x] **PR/gh** (`core/src/github.rs`): `push_branch` + `gh pr create --draft`
      (body from spec), capture PR URL → ticket → In Review + Jira writeback.
- [x] **Draft from Jira** (`core/src/draft.rs`): one-shot `claude -p` over the Jira
      summary+description → editable spec; saved to `ticket.spec` (promotes → ready).
- [x] **CLI**: `jira config`, `jira sync`, `draft <id>`, `pr <id>`.

Deferred within Phase 2:
- [x] ~~Auth / token storage~~ — handled entirely by `acli` (no creds in harmony).
- [x] **Verify acli `--json` field mapping** on a real run; tighten `parse_issues`.
      _(Verified against acli 1.3.19 on 2026-06-15: `workitem search` → top-level array,
      `summary`/`status.name`/`description`(ADF) under `fields`; `comment list` → `{comments:[]}`
      with **plain-string** `author`+`body` and **no timestamp**. Reordered parse paths to
      verified-first, documented the schema, and added regression tests from the real fixtures.)_
- [x] **Pagination** beyond the first 50 issues (acli `--paginate`). _(Swapped `--limit 50`
      for `--paginate` in `search_assigned`; acli returns the full set as one top-level JSON
      array — verified 127 results in a single call vs. the old 50 cap. Verified 2026-06-15.)_
- [ ] **Claude-generated PR summary** in the body (currently the spec); **repo-aware** Draft.
- [ ] Tests / live-call validation against real Jira + `gh`.

**Try the full vertical (real Jira + GitHub):**
```bash
harmony jira install                          # brew tap+install acli (or do it manually)
acli jira auth login                          # browser login (no API key / app reg)
# (or: harmony jira login  — passthrough to the same)
harmony jira sync                             # assigned-to-me issues → board
harmony draft <ticket_id>                     # spec drafted from the Jira issue
harmony start <ticket_id>                     # → In Progress; worktree + live claude
harmony pr <ticket_id>                        # push + draft PR; → In Review + PR comment
```

## Phase 3 — Desktop UI (Tauri + React/TS)  — SCAFFOLDED ✅ (frontend + backend both build)
App lives in `app/` (frontend) + `app/src-tauri/` (Tauri 2 backend, workspace member).
- [x] **Backend bridge** (`app/src-tauri/src/lib.rs`): Tauri commands wrapping the core
      (list/get tickets+repos, add local ticket, set spec, jira sync/configured, draft,
      open PR, start session, send_input, resize). **PTY↔event bridge**: PTY output →
      `term-output` events; keystrokes → `send_input`; `session-exit` event on exit. Hook
      server started in `setup`.
- [x] **Board** (`app/src/components/Board.tsx`): columns Todo → In Progress → For Your
      Review → In PR Review → Done (new tickets land in Todo). **Drag-and-drop** between
      columns (`set_ticket_status`, optimistic); dropping into **In Progress auto-opens a
      live Claude session**, dropping **out of In Progress stops** any live session, and
      dropping into **Done removes the ticket's worktree(s)** (branch/PR untouched). Polls
      every 1.5s for live state.
- [x] **Ticket detail + spec editor** (`App.tsx`): spec textarea, "Draft from Jira",
      Save spec, Start session, Open PR; top-bar Sync. For Jira tickets, a read-only
      **Jira panel** (`JiraInfo.tsx`) shows the issue **description + comments**
      (`jira_detail` → `get_issue` + `acli comment list`), loaded on select.
- [x] **New (local) ticket** (`App.tsx`): top-bar "+ New ticket" → title + optional spec +
      optional repo → `add_local_ticket`. (CLI: `harmony ticket add --title … [--spec …]
      [--repo …]`, no `--key` = local.)
- [x] **Sessions view** (`app/src/components/Sessions.tsx`): Board/Sessions nav toggle;
      table of all sessions (ticket, state, last tool, branch, started/ended, claude id),
      live badge, click-a-row → opens its ticket. **Rows are consolidated per worktree** —
      all the start/resume runs of one conversation collapse into a single row with a
      "×N runs" count (grouped in `Sessions.tsx` by `worktree_id`; live detected via the set
      of live session ids). **"Clear ended (N)"** bulk button + per-group delete
      (`delete_worktree_sessions`). Backend `list_sessions` (now returns `worktree_id`),
      `clear_ended_sessions`, `delete_session`/`delete_worktree_sessions`. CLI: `harmony
      sessions`, `harmony sessions --clear-ended`.
- [x] **Worktrees view** (`app/src/components/Worktrees.tsx`): Board/Sessions/Worktrees
      nav; table (ticket, repo, branch, path, created); per-row **delete** (disabled while
      a session is live for that ticket) → `git worktree remove` + drop row/sessions.
      Backend `list_worktrees`, `delete_worktree`. CLI: `harmony worktrees [--delete <id>]`.
- [x] **Claude task checklist** (`Tasks.tsx`): the session's TodoWrite list is captured by
      the hook server (`tool_input.todos`) and stored on `ticket.todos`; the detail panel
      renders a live checklist that ticks off (completed ✓ / in-progress ▸) as Claude works,
      refreshed by the 1.5s poll.
- [x] **Embedded terminal** (`Terminal.tsx`): xterm.js + fit addon bound to the session
      PTY over Tauri events; resize + auto-focus. **Live, steerable, per ticket** — live
      sessions tracked in a `ticketId → sessionId` map so several can run at once and each
      ticket shows its own terminal; "Open terminal" starts/resumes; clicking a live row
      on the Sessions page attaches; **"Stop session"** kills the process (`clone_killer`).
      Live state is the **backend session map** (`live_sessions` cmd), so it survives
      webview reloads and never spawns duplicates; open sessions are reconciled to ended
      on startup (`end_all_open_sessions`) so dead-process zombies don't linger as "live".
      (Requires `dragDropEnabled:false` — also needed for DnD.)
- [x] Builds: `npm run build` (frontend) ✅ and `cargo build -p harmony-app` ✅.

Deferred within Phase 3 (next iterations):
- [x] **Attention**: OS notification (`tauri-plugin-notification`) fires when a live session
      enters "waiting" (Claude wants input); clicking it focuses the app and opens that
      ticket. Edge-triggered (only on entry into waiting), seeded on first poll to avoid
      launch spam.
- [x] **Diff / PR pane** (`app/src/components/DiffPane.tsx`): in the ticket detail panel,
      shows `git diff --merge-base <base>` (committed + uncommitted vs base, colored) and
      PR meta + `gh pr checks` status (green/red/amber dots). Backend `ticket_diff`/`ticket_pr`.
- [x] **Permission mode**: sessions launch with `--permission-mode` from the
      `permission_mode` setting (**default `auto`** = autonomous). Configurable in Settings
      (auto / acceptEdits / default / plan / bypassPermissions); applied to new sessions.
- [x] **Settings page** (`app/src/components/Settings.tsx`): list/add/**rename**/remove repos.
      Add uses the dialog plugin's **folder picker**; optional default Jira project key.
      Rename is click-to-edit inline. Remove is guarded (refuses if the repo still has
      worktrees) and clears the binding on its tickets. Backend `add_repo`/`rename_repo`/
      `delete_repo`; CLI: `harmony repo add/list/rename`.
- [x] **Resume-on-relaunch**: startup captures live-at-shutdown tickets; UI reattaches them
      (`pending_reattach` → resume) on launch. Prior conversation shown via `session_transcript`
      in a "Conversation so far" pane (`TranscriptPane.tsx`) — separate pane, not in-terminal
      scrollback (Claude's TUI uses the alternate screen).
- [ ] Move terminal output to base64 (currently UTF-8 lossy) for binary-safe streams.

**Run it:**
```bash
cd app && npm install            # once
export JIRA_API_TOKEN=…          # optional, for Sync/Draft
npm run tauri dev                # launches the desktop window (vite + Tauri)
```
Uses the same `~/.harmony/harmony.db` and hook server (:8787) as the CLI.

## Phase 4 — Polish / hardening
- [ ] Soft "N running" concurrency indicator (no hard cap).
- [ ] Worktree GC (offer cleanup on PR merged/closed; manual "remove").
- [ ] Secret handling review (tokens in keychain, never logged).
- [ ] Error/edge states: session crash, `gh`/Jira failures, dirty worktree, network loss.

---

## Deferred to v2 (tracked, not built)
Native permission **triage UI** · tmux/daemon persistence for unattended runs · shared
**team backend** · JQL/board scope + pull-by-key · live **MCP tools** for the running
agent · cascade/auto-merge. Plus the Symphony-inspired orchestration items: **orchestrator
loop + state machine**, **retry/backoff + reconciliation**, **observability (token
accounting + HTTP API)**, and **`WORKFLOW.md` + workspace lifecycle hooks**. `BACKLOG.md`
(Later tier + § "Symphony delta") is the source of truth for all of these.

## Risk register
See `DESIGN.md` → "Sharp edges / risks". The dominant one (interactive hooks) is retired
by Phase 0; the rest (Jira transition discovery, hook injection robustness, resume
fidelity, draft-from-Jira cost) are addressed in their respective phases.

---

## Immediate next action
**Run Task 0.1.** Nothing else should be built until the hook side-channel is proven on
this machine with the installed Claude Code version.
