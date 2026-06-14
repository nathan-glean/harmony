# Harmony — Feature Backlog

Living to-do list for Harmony. Companion to:
- **`DESIGN.md`** — the *why* and the resolved product decisions.
- **`PLAN.md`** — the *how*, as phases (0–4) and what each phase shipped.

This doc is the *what's left*: a prioritized backlog of features to implement, including currently-deferred v1 items, Phase 4 hardening, v2 ambitions, and ideas borrowed from OpenAI's [Symphony](https://github.com/openai/symphony/blob/main/SPEC.md) (a headless **Codex + Linear** orchestration daemon).

## How to read this

**Priority tiers**
- **Now** — finish v1; close the gaps that block confident daily use.
- **Next** — high-value v1.5; sharpen the loop.
- **Later** — v2 ambitions: autonomy and Symphony-style orchestration.

**Theme tags:** `[session]` `[board]` `[jira]` `[github]` `[worktree]` `[autonomy]` `[ui]` `[orchestration]` `[hardening]` `[test]`

Items marked *(Symphony)* are inspired by an actual component of the Symphony [SPEC.md](https://github.com/openai/symphony/blob/main/SPEC.md). See the [Symphony delta](#symphony-delta) at the end for what genuinely overlaps, what harmony diverges on by design, and what is *not* a Symphony concept.

---

## Already shipped (for honesty)

Harmony is well past "scaffolding" — Phases 0–3 are functionally built. Backlog items below are scoped against this surface.

| Area | Shipped |
|------|---------|
| Store | SQLite (`repos`, `tickets`, `worktrees`, `sessions`, `settings`) with migrations & full CRUD — `core/src/store.rs` |
| Worktrees | Per-ticket worktree create/reuse/remove, `harmony/<KEY>-<slug>` branch naming, prune+reuse — `core/src/worktree.rs` |
| Sessions | Interactive `claude` in PTY, first-prompt = spec, `--resume`, exit detection — `core/src/session.rs` |
| Hooks | Localhost axum server; `PreToolUse`/`PostToolUse`/`Stop`/`Notification`; cwd→session correlation; drives board state — `core/src/hooks.rs` |
| Settings inject | Additive hooks into `.claude/settings.local.json` (not tracked) — `core/src/settings.rs` |
| Jira | `acli` shell-out: assigned-to-me sync, transitions, comments, ADF→text; install/login/logout/status — `core/src/jira.rs` |
| GitHub | `git push` + `gh pr create --draft`, PR URL capture — `core/src/github.rs` |
| Draft | One-shot `claude -p` Jira→spec — `core/src/draft.rs` |
| CLI | `repo` / `ticket` / `start` / `serve` / `sessions` / `worktrees` / `jira` / `draft` / `pr` — `core/src/main.rs` |
| UI | Drag-drop board, ticket detail + spec editor, sessions & worktrees tables, settings (folder picker), embedded xterm.js terminal, OS notifications, Jira connect panel — `app/src/`, `app/src-tauri/src/lib.rs` |

---

## Now — finish v1

### 1. Diff / PR pane `[ui][github]` — ✅ DONE
Render the worktree diff and PR check status in-app. DESIGN promises "Diff / PR panes"; today only the PR URL surfaces (in a toast).
- **AC:** Selecting a ticket with a branch shows `git diff` against its base branch and `gh pr checks` status without leaving the app. ✅
- Built: `DiffPane.tsx` in the ticket detail panel; backend `ticket_diff` (`git diff --merge-base <base>` = committed + uncommitted vs base) and `ticket_pr` (`gh pr view` + `gh pr checks --json`, reads stdout even when checks fail/pending). Auto-loads on select + Refresh; colored diff, PR link/state, check dots (green/red/amber).

### 2. Resume-on-relaunch UI `[session][ui]` — ✅ DONE
Core already resumes via `claude --resume <id>`, but the terminal opens blank. Rebuild scrollback from the JSONL transcript on app start.
- **AC:** Relaunching the app reattaches live tickets and renders the prior conversation, not an empty terminal. (Scrollback is an approximation — see risks.) ✅
- Built: **(a) Reattach** — startup captures tickets that were live at shutdown (`tickets_with_open_session`, before the zombie reconcile); UI drains `pending_reattach` on mount and resumes each (`claude --resume`), repopulating the live map. **(b) Prior conversation** — the hook stores `transcript_path`; `session_transcript` renders the JSONL into a read-only "Conversation so far" pane in the detail. NOTE: rendered as a separate pane, not in-terminal scrollback — Claude's TUI uses the **alternate screen**, so writing scrollback into the live xterm would be wiped on redraw.

### 3. Transcript tailer `[session]` — ✅ DONE
Tail `~/.claude/projects/<hash>/<id>.jsonl` for richer in-session progress (last assistant message, current tool) beyond the hook-derived working/waiting flag. Named in DESIGN architecture.
- **AC:** Board card / detail shows a live "latest progress" line sourced from the transcript. ✅
- Built: **(a) Tailer** — `session::latest_progress` seeks the last 64 KB of the JSONL (dropping the partial leading line, lossy UTF-8 at the boundary), walks the assistant lines and returns the latest text block (whitespace-collapsed, capped 280 chars) + most recent `tool_use` name. **(b) Plumbing** — `live_progress` command tails every session live in *this* process (reusing `latest_transcript_path_for_ticket`, off-thread via `spawn_blocking`); the UI polls it on the existing 1.5 s board loop into a ticket-keyed `progress` map. **(c) UI** — shared `ProgressLine` renders a tool chip + message; shown compact on board cards and in the detail panel. Live-only (keyed off the in-process session map), so ended sessions drop their line.

### 4. Verify `acli --json` mapping + tighten `parse_issues` `[jira][test]` — ✅ DONE
`acli`'s JSON schema is undocumented and `jira.rs` already parses defensively across multiple shapes. Validate against a real instance and lock it down.
- **AC:** A recorded real `acli` payload round-trips through `parse_issues` under test; brittle field guesses removed or asserted. ✅
- Done (verified live against acli 1.3.19 / teamgenio.atlassian.net, 2026-06-15): captured real payloads — `workitem search` → top-level **array**, `summary`/`status.name`/`description`(ADF) under `fields`; `comment list` → `{comments:[…]}` with **plain-string** `author`+`body` and **no timestamp**. Reordered `item_to_issue`/`comment_from_json` to try verified paths first (defensive fallbacks kept + asserted), documented the schema in the module header, and added 3 regression tests from the recorded fixtures (`parses_real_search_shape`, `parses_real_comment_shape`, `description_handles_plain_string_fallback`). Note: fixtures are snapshots — they pin our parser to the recorded shape, not acli version drift (the fallbacks hedge that).

### 5. Core test suite `[test]`
No tests today. Cover the load-bearing logic: store CRUD, worktree create/reuse + branch-naming/slugify, and the cwd→worktree→session correlation in the hook server.
- **AC:** `cargo test -p harmony-core` exercises these paths and passes in CI.

### 6. Error & edge-state handling `[hardening]`
Today many failures are best-effort/silent. Surface session crash, `gh`/Jira failure, dirty worktree, and network loss as user-visible states.
- **AC:** Each failure path shows a toast/state and leaves the DB consistent (no orphaned "working" sessions or half-created worktrees).

---

## Next — sharpen the loop (v1.5)

### 7. Structured spec fields `[ui][jira]`
Promote acceptance criteria / relevant paths / constraints from one markdown blob to first-class fields (store columns + editor). Deferred in Phase 1 pending the UI.
- **AC:** Fields persist independently and are composed into the first prompt fed to `claude`.

### 8. Claude-generated PR summary + repo-aware Draft `[github][jira]`
PR body is currently the spec verbatim. Generate a summary of the actual diff for the PR body, and let Draft scan the repo for relevant paths.
- **AC:** `harmony pr` uses a generated diff summary when available; Draft output references real repo paths.

### 9. Jira pagination + optional JQL scope `[jira]` — ⏳ PARTIAL
Sync currently caps at the first ~50 assigned issues. Add `acli --paginate`, and optionally allow pulling an arbitrary issue by key / a JQL query.
- **AC:** Sync retrieves >50 issues ✅; an issue can be imported by key even if unassigned ⬜ (not yet).
- Done: `search_assigned` now uses `--paginate` instead of `--limit 50` (acli returns the full set as one top-level array — verified 127 results in a single call, 2026-06-15).
- Remaining: import-by-key / arbitrary-JQL scope for unassigned issues.

### 10. Hook auth token `[hardening]`
The only boundary today is localhost-bind. Inject a shared secret into the per-worktree settings and verify it server-side.
- **AC:** Hook requests missing/with a wrong token are rejected; legitimate injected sessions still work.

### 11. Worktree GC `[worktree][hardening]`
Offer cleanup when a PR merges/closes, plus a "remove all worktrees for this ticket" action. Disk grows unbounded otherwise.
- **AC:** A merged/closed PR offers (or auto-performs, per setting) worktree removal; branch/PR untouched.

### 12. Soft concurrency indicator `[ui]`
No hard cap, but show how many sessions are live so the user can self-throttle (auth quota is shared — see risks).
- **AC:** Header shows a live "N running" count sourced from the backend session map.

### 13. Base64 terminal stream + secret-handling review `[session][hardening]`
PTY output is currently UTF-8 lossy; switch to base64 for binary-safe streaming. Audit that any tokens live in the keychain and never hit logs.
- **AC:** Binary/control output renders correctly; a log audit shows no secrets.

---

## Later — autonomy & orchestration (v2)

### 14. Native permission triage UI (autonomy) `[autonomy][ui]`
Intercept `PreToolUse` over the hook channel and present an in-app approve/deny card instead of requiring the user to answer in the terminal. Phase 0 proved a `{"permissionDecision":"allow"}` response auto-approves. This is the north-star evolution of the attention model.
- **AC:** A PreToolUse pause renders an approve/deny card whose decision drives the live session.

### 15. MCP / folder-trust bootstrap for autonomy `[autonomy][hardening]`
Unattended runs hang on `.mcp.json` / never-trusted-folder startup prompts. Use `--strict-mcp-config` and bootstrap folder-trust once per worktree.
- **AC:** An autonomous start in an MCP-containing or untrusted repo proceeds without blocking on an interactive prompt.

### 16. Orchestrator loop + state machine `[orchestration]` *(Symphony)*
A daemon/loop that watches the board and ensures every active ticket has a running session, (re)spawning crashed or stalled ones — Symphony's core behavior. Today sessions are spawned manually ("Open terminal") or on drag-into-In-Progress. Symphony models this as a **single-authority in-memory state** (each issue is unclaimed → claimed → running → retry-queued → released) plus a periodic **poll-and-dispatch tick**:
1. **Reconcile** running tickets (see #17), then
2. **Fetch candidates** in active states (skip the tick if the fetch fails), then
3. **Sort** by priority (ascending), then creation time (oldest first), then
4. **Dispatch** eligible tickets until concurrency slots are exhausted.

A ticket is **eligible** when: it's in an active column, carries every required label (if a label filter is configured), is **not already running or claimed**, a slot is free, and — for `Todo` — it has no non-terminal blockers (`blocked_by`). Claiming a ticket before spawn prevents a double-dispatch race.
- **Concurrency slots:** `available = max(max_concurrent - running, 0)`, with an optional **per-column** cap (e.g. limit how many In-Progress run at once) falling back to the global cap. This subsumes the soft indicator in #12 (the count it shows becomes the slot accounting).
- **AC:** With auto-mode on, a tick reconciles board state, sorts candidates by priority/age, and dispatches eligible tickets up to the (global + per-column) slot limit; a `Todo` with an open blocker is skipped; crashed sessions are restarted; no ticket is ever double-dispatched.

### 17. Retry/backoff + reconciliation `[orchestration][hardening]` *(Symphony)*
A retry queue for transient session/integration failures, plus active-run reconciliation — the other half of the orchestrator (#16). Symphony distinguishes two retry kinds and reconciles every running issue each tick.
- **Continuation retries** (after a *clean* exit, more work likely remains): re-dispatch on a short fixed delay (~1s).
- **Failure retries** (abnormal exit): exponential backoff `min(base · 2^(attempt-1), cap)` with a configurable cap.
- **Reconciliation each tick:** **(a) stall detection** — if no session event has arrived within a stall-timeout (event-inactivity), terminate and retry; **(b) tracker-state refresh** of running tickets — a ticket whose Jira/board state went **terminal** → stop the session + clean its worktree; went **non-active** (e.g. moved back to Todo) → stop without cleanup; still **active** → just update the snapshot.
- **AC:** A failed session is retried up to N times with exponential backoff (continuation re-dispatch uses the short fixed delay); a stalled session is detected and retried; a ticket moved to a terminal/non-active state out-of-band is reconciled (stopped, cleaned per rule); retry state is visible in the UI.

### 18. Proof-of-work bundle `[orchestration][github]`
On completion, gather evidence: CI status, PR review feedback, a complexity/diff-size summary, optionally a walkthrough.
- **Note:** harmony-original — **not** a Symphony concept. The Symphony SPEC has no "proof of work"; its completion story is just observability (token/rate-limit accounting, event logs). Kept here because it fits harmony's handoff-to-review model.
- **AC:** Finishing a ticket attaches a proof-of-work summary to the ticket / PR.

### 19. `WORKFLOW.md` in-repo policy `[orchestration][autonomy]` *(Symphony)*
A repo-versioned prompt template + runtime settings (concurrency, active/terminal states, autonomy posture) loaded per repo, à la Symphony. Keeps policy with the code. Symphony's concrete model: a Markdown file with a **YAML front-matter** config block + a prompt-template body, where:
- Secret/path values support **`$VAR_NAME` env indirection** (resolved at load; never logged).
- The file is **hot-reloaded** — edits re-apply without restart; an invalid edit keeps the **last-known-good** config and surfaces an operator error.
- The prompt template renders against an `issue` object + an `attempt` integer (null on first run), so retries/continuations can use different instructions.

For harmony, a repo's `WORKFLOW.md` would override the DB/Settings defaults for that repo (prompt scaffold, autonomy/permission mode, per-column concurrency, which board columns count as active/terminal).
- **AC:** When a repo contains `WORKFLOW.md`, its prompt/settings override Harmony defaults for that repo; `$VAR` values resolve from the environment; editing the file re-applies live, and an invalid file falls back to the last good config with a visible error.

### 20. tmux / daemon persistence `[orchestration]`
Long unattended runs that outlive the desktop window. Today PTYs are children of the app and die with it (resume bridges the gap, but not for live unattended work).
- **AC:** In daemon mode, sessions keep running after the app window closes and reattach on reopen.

### 21. Live MCP tools for the running agent `[jira][autonomy]`
Give the agent Jira/ticket context and progress-posting tools mid-run (vs. Harmony doing all tracker writes around it).
- **AC:** The agent can read its own ticket and post progress without the user leaving the session.

### 22. Cascade / auto-merge `[github][orchestration]`
Opt-in auto-merge once CI is green and review approves. Harmony deliberately never merges in v1.
- **AC:** With the setting enabled, a PR merges automatically after checks + reviews pass; disabled by default.

### 23. Shared team backend `[orchestration]` *(epic)*
Coordination, who's-on-what, a shared team board. The largest item; explicitly v2+ and currently out of detailed scope.
- **AC:** Tracked as an epic; to be broken down when prioritized.

### 24. Observability: token accounting + rate limits + HTTP API `[orchestration][hardening]` *(Symphony)*
Symphony tracks, per run and in aggregate, the agent's **token usage** and **rate-limit** posture, and optionally exposes a read-only HTTP surface. harmony already runs a localhost hook server (`core/src/hooks.rs`) — this extends it.
- **Token accounting:** prefer **absolute thread totals** when the agent reports them; **ignore delta payloads** for totals and **dedup against the last reported total** to avoid double-counting; accumulate input/output/total across sessions.
- **Rate limits:** retain the latest rate-limit payload seen in agent updates and surface it (informs #12 throttling and the auth-quota risk below).
- **Optional HTTP API** (loopback-bound, read-only except a refresh trigger): `GET /api/v1/state` (running rows + retry queue + token totals), `GET /api/v1/<ticket>` (per-ticket detail), `POST /api/v1/refresh` (force a poll tick — pairs with #16). A `GET /` dashboard is optional.
- **AC:** The app shows live + cumulative token totals and the latest rate-limit state per session, sourced without double-counting; if the API is enabled, `/api/v1/state` returns running/retry/token data and `/api/v1/refresh` triggers a tick.

### 25. Workspace lifecycle hooks `[worktree][orchestration]` *(Symphony)*
Configurable shell hooks around the worktree lifecycle, run with `cwd` = the worktree, with a timeout. Distinct from harmony's existing `.claude/settings.local.json` *Claude-hook* injection (#settings) — these are **operator** hooks for workspace setup/teardown (e.g. install deps, warm caches, cleanup). Symphony's set + failure semantics:
- `after_create` — runs once on a newly created worktree; **failure is fatal** to creation.
- `before_run` — runs before each session attempt; **failure aborts** the attempt.
- `after_run` — runs after each attempt; failure is **logged and ignored**.
- `before_remove` — runs before worktree deletion; failure is **logged and ignored**.
- **AC:** Configured hooks run at the right lifecycle point with `cwd` = the worktree and a bounded timeout; `after_create`/`before_run` failures block (creation/attempt) while `after_run`/`before_remove` failures are non-fatal and logged.

### 26. Remote / SSH workers `[orchestration]` — *(Symphony, far-future / likely out-of-scope)*
Symphony's Appendix-A optional extension runs the agent on a remote host over SSH (the orchestrator stays the single source of truth; the workspace + agent live remotely). Noted for completeness; **likely out-of-scope** for a per-developer desktop tool whose whole point is local supervision. Revisit only if a "run my agents on a beefier remote box" use case emerges.
- **AC:** (deferred) An assigned ticket can run its session on a configured remote host with the same isolation + reconciliation guarantees as local.

---

## Cross-cutting risks

Carried from `DESIGN.md` §Sharp edges — relevant to several items above:
- **Auth quota under parallelism** — concurrent interactive sessions share one subscription login's quota. Relevant to #12, #16, #20, #24.
- **Jira transition discovery** — status names/IDs vary per workflow; writeback must discover valid transitions per issue, not hardcode. Relevant to #8, #9.
- **Resume fidelity** — scrollback rebuilt from JSONL approximates the live TUI; acceptable, not pixel-identical. Relevant to #2.
- **Draft latency/cost** — repo-scanning Draft adds latency and token cost; keep it optional and bounded. Relevant to #8.

## Symphony delta

Comparison against the actual [SPEC.md](https://github.com/openai/symphony/blob/main/SPEC.md). Symphony is a **headless daemon** that polls **Linear**, creates a per-issue workspace, and runs a **Codex** app-server agent in it — fully autonomous, scheduler/reader only (the *agent* does any tracker writes, e.g. via an optional `linear_graphql` tool).

### Where Harmony already matches
Per-task isolation (Symphony per-issue workspace ≈ harmony per-ticket git worktree), reading work from an issue tracker, a lifecycle/state surface as the control plane, and opening a draft PR with **human handoff rather than blind merge** (Symphony likewise never auto-merges; "done" means reaching a handoff state).

### Deliberate divergences (intentional product bets — not gaps)
| Dimension | Symphony | Harmony |
|---|---|---|
| Agent driver | Codex app-server (JSON-line subprocess) | Claude Code in a PTY (xterm.js) |
| Tracker | Linear (GraphQL) | Jira (via `acli`) |
| Form factor | Headless daemon / CLI | Tauri **supervised desktop GUI** |
| Autonomy | Autonomous poll-and-dispatch loop | **Supervised-first**; manual / drag-to-start |
| Policy & config | In-repo `WORKFLOW.md`, hot-reloaded | SQLite settings + Settings UI (see #19) |
| Tracker writes | Agent does them via its own tools | Harmony does **column-driven writeback** |
| Enrichment | none | **Jira Draft + structured spec** (Harmony-only) |

### Genuine gaps Harmony could adopt (mapped to backlog items)
- **Orchestrator loop + single-authority state machine** — candidate selection (priority sort, `blocked_by` gating, required labels), claim/run/retry/release states, concurrency slots (global + per-column) → **#16**.
- **Retry/backoff + reconciliation** — continuation (fixed) vs failure (exponential w/ cap) retries; stall detection + tracker-state refresh of running issues → **#17**.
- **Observability** — token accounting (absolute totals, dedup deltas), rate-limit tracking, optional `/api/v1/*` JSON API + dashboard → **#24**.
- **`WORKFLOW.md` policy + workspace lifecycle hooks** — versioned prompt/config with `$VAR` + hot-reload, and `after_create`/`before_run`/`after_run`/`before_remove` shell hooks → **#19**, **#25**.
- **Remote/SSH workers** — Appendix-A extension; noted but likely out-of-scope for a per-developer desktop tool → **#26**.

### Not a Symphony concept (Harmony-original)
**Proof-of-work (#18)** is *not* in the Symphony spec — Symphony's completion story is purely observability (token/rate-limit accounting + structured event logs). It's kept on the roadmap because it fits Harmony's handoff-to-review model, but it's not "borrowed from Symphony."

Harmony's distinct bets remain the **supervised-first** model with a richer GUI and a **Jira enrichment** layer (Draft, structured spec) that Symphony doesn't have.
