# Harmony — Feature Backlog

Living to-do list for Harmony. Companion to:
- **`DESIGN.md`** — the *why* and the resolved product decisions.
- **`PLAN.md`** — the *how*, as phases (0–4) and what each phase shipped.

This doc is the *what's left*: a prioritized backlog of features to implement, including currently-deferred v1 items, Phase 4 hardening, v2 ambitions, and ideas borrowed from OpenAI's [Symphony](https://github.com/openai/symphony) (issue-tracker-driven autonomous agent orchestration).

## How to read this

**Priority tiers**
- **Now** — finish v1; close the gaps that block confident daily use.
- **Next** — high-value v1.5; sharpen the loop.
- **Later** — v2 ambitions: autonomy and Symphony-style orchestration.

**Theme tags:** `[session]` `[board]` `[jira]` `[github]` `[worktree]` `[autonomy]` `[ui]` `[orchestration]` `[hardening]` `[test]`

Items marked *(Symphony)* are inspired by Symphony's autonomous-orchestration model. See the [Symphony delta](#symphony-delta) at the end.

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

### 3. Transcript tailer `[session]`
Tail `~/.claude/projects/<hash>/<id>.jsonl` for richer in-session progress (last assistant message, current tool) beyond the hook-derived working/waiting flag. Named in DESIGN architecture; not yet implemented.
- **AC:** Board card / detail shows a live "latest progress" line sourced from the transcript.

### 4. Verify `acli --json` mapping + tighten `parse_issues` `[jira][test]`
`acli`'s JSON schema is undocumented and `jira.rs` already parses defensively across multiple shapes. Validate against a real instance and lock it down.
- **AC:** A recorded real `acli` payload round-trips through `parse_issues` under test; brittle field guesses removed or asserted.

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

### 9. Jira pagination + optional JQL scope `[jira]`
Sync currently caps at the first ~50 assigned issues. Add `acli --paginate`, and optionally allow pulling an arbitrary issue by key / a JQL query.
- **AC:** Sync retrieves >50 issues; an issue can be imported by key even if unassigned.

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

### 16. Continuous board polling + auto-spawn `[orchestration]` *(Symphony)*
A daemon/loop that watches the board and ensures every active ticket has a running session, (re)spawning crashed or stalled ones — Symphony's core behavior. Today sessions are spawned manually ("Open terminal") or on drag-into-In-Progress.
- **AC:** With auto-mode on, a tick reconciles board state and ensures a session exists for each active ticket; crashed sessions restart.

### 17. Retry with exponential backoff `[orchestration][hardening]` *(Symphony)*
A retry queue for transient session/integration failures, with increasing delays.
- **AC:** A failed session is retried up to N times with backoff, and the retry state is visible in the UI.

### 18. Proof-of-work bundle `[orchestration][github]` *(Symphony)*
On completion, gather evidence: CI status, PR review feedback, a complexity/diff-size summary, optionally a walkthrough — Symphony's "proof of work."
- **AC:** Finishing a ticket attaches a proof-of-work summary to the ticket / PR.

### 19. `WORKFLOW.md` in-repo policy `[orchestration][autonomy]` *(Symphony)*
A repo-versioned prompt template + runtime settings (concurrency, active/terminal states, autonomy posture) loaded per repo, à la Symphony. Keeps policy with the code.
- **AC:** When a repo contains `WORKFLOW.md`, its prompt/settings override Harmony defaults for that repo.

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

---

## Cross-cutting risks

Carried from `DESIGN.md` §Sharp edges — relevant to several items above:
- **Auth quota under parallelism** — concurrent interactive sessions share one subscription login's quota. Relevant to #12, #16, #20.
- **Jira transition discovery** — status names/IDs vary per workflow; writeback must discover valid transitions per issue, not hardcode. Relevant to #8, #9.
- **Resume fidelity** — scrollback rebuilt from JSONL approximates the live TUI; acceptable, not pixel-identical. Relevant to #2.
- **Draft latency/cost** — repo-scanning Draft adds latency and token cost; keep it optional and bounded. Relevant to #8.

## Symphony delta

Where Harmony **already matches** Symphony: worktree-per-task isolation, reading work from an issue tracker (Jira), a lifecycle board as the control surface, and opening a draft PR with human handoff rather than blind merge.

Where Symphony **goes further** (and why the *Later* tier looks as it does): a continuous autonomous loop that keeps an agent running per active ticket and restarts failures (#16, #17), explicit *proof of work* on completion (#18), in-repo `WORKFLOW.md` policy (#19), and a posture toward unattended runs (#20). Harmony's distinct bets remain the **supervised-first** model with a richer GUI and a **Jira enrichment** layer (Draft, structured spec) that Symphony doesn't have.
