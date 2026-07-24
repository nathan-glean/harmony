# harmony — Design

A local, per-developer desktop tool that sits **between Jira and Claude Code**: it
ingests your assigned Jira tickets (plus local-only tickets), lets you enrich each
with an agent-friendly spec, and runs an isolated, supervised Claude Code session
per ticket in its own git worktree — then opens a PR and hands off to your normal
review process.

Reference points: OpenAI's **Symphony** is a *headless* daemon that polls **Linear** and
runs **Codex** agents in per-issue workspaces, fully autonomous. harmony shares its
tracker-driven, isolated-workspace-per-task shape but deliberately diverges on every axis:
**Claude Code (PTY)** not Codex, **Jira** not Linear, a **supervised desktop GUI** not a
headless loop — closer to **claude-commander** (worktree-per-task, live sessions) in
posture — with a Jira enrichment layer neither has. See `BACKLOG.md` § "Symphony delta"
for the full capability comparison and the Symphony-inspired items on the roadmap.

## Resolved decisions

| # | Decision | Choice |
|---|----------|--------|
| 1 | Autonomy model | **Supervised-first**; full autonomy is a per-ticket opt-in (just an elevated permission mode), off by default |
| 2 | Deployment | **Local, one instance per developer**. No server, no multi-user auth. Reads shared Jira |
| 3 | Form factor | **Tauri desktop app** — Rust core + web frontend |
| 4 | Claude driver | **PTY-hosted interactive `claude`** per session (xterm.js), native UX. Status from **HTTP hooks** + tailing the session JSONL. No headless/SDK in the loop |
| 5 | Ticket model | **Enriched local tickets**, each optionally **linked to a Jira issue**. Local-only work = unlinked ticket. One unified board |
| 6 | Jira read scope | **Assigned to me** only (no JQL box) |
| 7 | Jira writeback | **Column-driven**: moving a Jira-linked ticket between board columns transitions the issue (Todo / In Progress / In PR Review / Done) **iff that status exists in its workflow** (best-effort, no-op otherwise; "For Your Review" has no Jira equivalent). Plus PR link posted as a comment on PR open |
| 8 | Repo model | **Multi-repo**; pick target repo at start, default remembered per Jira project key |
| 9 | Worktree scope | **Per-ticket, reused** across sessions (1 ticket → 1 worktree → 1 branch → 1 PR). "New alternate attempt" forks a second worktree on demand |
| 10 | Spec authoring | **Light structure + AI draft**: markdown body + optional fields (acceptance criteria, relevant paths, constraints). "Draft from Jira" expands the terse issue into an editable first pass |
| 11 | Output flow | **Open a PR ready for review via `gh`** (body from spec + optional Claude summary), link to Jira, show diff in-app. Reaching "In PR Review" is the human's hand-off to the team, so the PR is not a draft — it requests reviewers and is mergeable. harmony only merges once approved + green (gated/auto-merge, default off) |
| 12 | Session persistence | **Resume-on-relaunch**: PTYs are children of harmony; on relaunch resume each via `claude --resume <id>`, rebuild view from transcript. No tmux |
| 13 | Attention model | **Notify + jump-to-terminal** (v1): card badge + OS notification from hooks; answer in embedded terminal. Native approve/deny **triage UI** is the north-star evolution |
| 14 | Board model | **harmony-native lifecycle columns**: Todo → In Progress (`working`) → For Your Review (`waiting`) → In PR Review (`in_review`) → Done. New tickets land in **Todo**. **Drag-and-drop** moves a ticket (manual override); In Progress/For-Your-Review are also driven live by session hooks, In PR Review/Done by PR+Jira |

## Architecture

```
Tauri desktop app (single process)
├── Rust core
│   ├── Jira client            (Cloud REST v3; read assigned-to-me; write status+PR-link)
│   ├── Repo registry          (N local git repos; default repo per project key)
│   ├── Worktree manager        (~/.harmony/worktrees/<repo>/<branch>, off fresh default branch)
│   ├── Session manager         (spawn `claude` in PTY, cwd = worktree; resume via --resume)
│   ├── Local hook server       (HTTP on localhost: receives SessionStart/PreToolUse/
│   │                            PermissionRequest/Stop/SessionEnd → drives board state + notifs)
│   ├── Transcript tailer        (~/.claude/projects/<hash>/<id>.jsonl → progress)
│   ├── PR/gh integration        (push branch, gh pr create — ready for review)
│   └── Store                    (SQLite: tickets, spec, ticket↔repo↔worktree↔session map, settings)
└── Web frontend
    ├── Board (native lifecycle columns)
    ├── Ticket detail + spec editor ("Draft from Jira")
    ├── Embedded terminals (xterm.js over PTY)
    └── Diff / PR panes
```

### Ticket lifecycle state machine
The board's behaviour — which column a ticket lands in and what side effects (`Action`s) run — is a
single pure decision function, `flow::decide` (`core/src/flow.rs`), driven by one executor
(`apply_event`/`run_action` in `app/src-tauri/src/lib.rs`) that every event source funnels through
(board drags, session/hook completion events, and the poll loop). Its contract is pinned by
`core/tests/flow.rs`, and a **generated diagram + transition table** lives at
[`docs/flow.md`](docs/flow.md) — rendered from `decide` itself (`task flow:doc`) and drift-checked in
CI, so it always matches the code.

**Idempotency — the action log.** An append-only `ticket_actions` table (`store::record_state_action`
/ `has_action_at`) is the single durable source of truth for "have we already done this at this commit?"
— each autonomous step (proof / review / judge / ci-triage / conflict / reverify) is stamped with the
branch HEAD it acted on. Because it persists, nothing autonomous is redone after an app restart at an
unchanged HEAD, and the same log powers the Orchestrator tab's audit feed.

### Autonomous drivers (the long-running loop)
The state machine is event-driven and pure; the **autonomy** lives in a 60s background poll loop
(`app/src-tauri/src/lib.rs`, the `run()` setup) whose passes *observe* state and *inject* the events
a human would, so the board drives itself with `flow::decide` still authoritative. Passes, in order:
- `poll_ci_once` — triage PR-stage CI; auto-spawn a fix session on PR-caused failures (capped by
  `MAX_CI_FIX_ATTEMPTS`). [`ci_autofix`, default on]
- `poll_reviews_once` — re-run `/review` when a reviewed branch's HEAD has moved. [`auto_review`, on]
- `poll_review_loop_once` — **self-correcting review loop**: an LLM judge (`core/src/review.rs`)
  classifies a current `/review` as `pass` / `changes_requested`; on the latter it auto-spawns a
  fix session seeded with the findings → the fix commits → HEAD moves → re-review → re-judge, until
  clean or `MAX_REVIEW_FIX_ATTEMPTS` (then a desktop **escalation** notification). Pre-PR only; a
  clean verdict rests in "For Your Review" for the human to open the PR. [`review_loop`, default off]
- `poll_reverify_once` — **proportional re-verification**: before the review/proof passes, decide once
  per new HEAD whether the delta since the last verified commit actually needs a fresh review and/or
  proof (hybrid: an LLM-free heuristic for docs/format-only, else a cheap `claude -p` triage that
  flags behaviour-changing edits). Trivial deltas carry the prior verification forward (via the action
  log) so a one-line fix doesn't re-trigger a full review + proof. [`core/src/reverify.rs`]
- `poll_proof_loop_once` — after review passes, capture **proof-of-work** (walkthrough/screenshots/
  grounded report) once per reviewed HEAD, surfaced in the Proof tab + posted as a PR comment.
  [`proof`, default on; `core/src/proof.rs`]
- `poll_conflicts_once` — when a PR is conflicting, spawn an autonomous rebase/resolve session (capped,
  then escalates).
- `poll_auto_merge_once` — when a PR is approved on GitHub (`reviewDecision == APPROVED`) **and** CI
  is green, inject `Move(Done)` so `decide` merges + cleans up. The agent never self-approves.
  [`auto_merge`, default off — the one irreversible, outward-facing step]
- `poll_pr_state_once` — **bidirectional PR↔ticket sync**: a PR merged/closed on GitHub → ticket Done;
  reopened → back to In PR Review; keeps the persisted PR snapshot fresh.
- `poll_stuck_sessions_once` — **watchdog**: a finished-but-stuck session (missed completion hook)
  detected from its idle transcript re-fires the completion event; a genuinely hung one escalates.
- `poll_orchestrator_once` — the **coordinator**: restart crashed working sessions (capped), answer
  derivable worker questions, accept low-risk spec proposals, escalate the rest. [`orchestrator`, off]

The judge and the other autonomous steps are fingerprinted by HEAD in the **action log** so each runs
once per reviewed change, not per poll — and not again after a restart. Smarter loops also mean a
re-worked ticket returns to the **furthest stage** it had reached (e.g. straight back to In PR Review),
not always through Human Review. On **relaunch**, still-open sessions are classified from their
transcript (resume genuinely mid-work / recover a finished one without resuming / drop the rest) rather
than naively resumed. The generated [`docs/flow.md`](docs/flow.md) covers the underlying transitions.

**Activity status.** A pure classifier (`core/src/activity.rs`, a sibling of `flow::decide`/`warnings`)
turns the same facts + settings into a single per-ticket `Activity { category, label }` — *Working*
(the system is handling it), *WaitingOnYou*, *WaitingExternal*, or *Idle*. The rule: **if the system
will act on this state automatically (given the settings + caps) it's Working; once it has done all it
can it's WaitingOnYou/WaitingExternal** — so the same board state reads differently depending on whether
autonomy is on. The backend recomputes + persists it (`tickets.activity`) on every state-machine event
and each poll tick, fires the "needs you" desktop notification on the transition into WaitingOnYou, and
the UI renders it as the per-card pill + modal detail.

### Per-session flow
1. Pick ticket → choose repo (defaulted) → write/Draft spec.
2. Worktree created off fresh default branch; branch `harmony/<KEY>-<slug>`.
3. harmony writes per-worktree `.claude/settings.json` with HTTP hooks pointing at its
   local server, then spawns interactive `claude` in a PTY (cwd = worktree).
4. You supervise in the embedded terminal; board state tracks hooks (Working/Waiting).
   Optional autonomy = launch with elevated `--permission-mode`.
5. On finish: push branch, `gh pr create` (ready for review), transition Jira + comment PR link.
6. Hand off to normal review. Close harmony anytime → resume later via `--resume`.

- **Jira Cloud** via the official **Atlassian CLI (`acli`)** — harmony shells out to it
  (the same pattern as `gh` for GitHub). acli owns auth: `acli jira auth login` is a
  browser login with **no app registration, no API key, no stored secret in harmony**.
  _(confirmed; chosen over OAuth-REST and MCP — simpler setup, less code to own. MCP is
  reserved for the future "agent talks to Jira mid-session" feature.)_
- **GitHub** + `gh` CLI installed/authed. _(confirmed)_
- **SQLite** (`sqlx`) for harmony's store. _(confirmed)_
- **Frontend: React + TypeScript** (Vite) with `xterm.js` for embedded terminals. _(confirmed)_
- Concurrency: soft "N running" indicator, no hard cap (self-limiting under supervision).

## Shipped since v1 (was "deferred")
The autonomous loop and its supporting machinery have landed — see "Autonomous drivers" above and
`BACKLOG.md` for per-item status: the **state machine + generated docs**, **self-correcting review
loop**, **proof-of-work**, the **immutable action log** (idempotency across restarts), **auto-resolve
PR conflicts**, **gated auto-merge + bidirectional PR↔ticket sync** ("↗ PR" button + persisted PR
snapshot), **smarter loops** (return-to-furthest-stage + proportional re-verification + incremental
review), the **stuck-session watchdog + restart recovery**, and the **orchestrator coordinator + tab**.

## Deferred to v2
- Native approve/deny **permission triage UI** (intercept via PermissionRequest/PreToolUse HTTP hooks).
- **tmux / daemon** persistence for long unattended autonomous runs that outlive the window.
- Shared **team backend** (coordination, who's-on-what, team board).
- JQL / board-based Jira scope beyond assigned-to-me; pull arbitrary issue by key.
- Giving the running agent **live MCP tools** for Jira/ticket context or progress posting.
- Fully-autonomous **candidate dispatch** (priority sort, `blocked_by`, `Todo → In Progress`) — kept
  human-only by design today — plus a formal retry queue and token/rate-limit **observability**.

## Sharp edges / risks
1. **Auth quota**: concurrent interactive sessions share one subscription login's quota.
   (Interactive PTY avoids the separate "Agent SDK" credit pool that `-p` would hit — a
   reason we chose PTY.) Heavy parallelism could still hit limits.
2. ~~**Permission prompt signal**~~ — **RETIRED by the Phase 0 spike (2026-06-11)**.
   On Claude Code **v2.1.173**, HTTP hooks (`PreToolUse`/`PostToolUse`/`Stop`) fire for an
   interactive PTY session, and a `PreToolUse` response of `{"permissionDecision":"allow"}`
   auto-approved a Write in `default` mode with **no TUI prompt** — proving programmatic
   allow/deny control. Implication: the v2 **triage UI is feasible now**, not just the v1
   notify path. Note: `PermissionRequest` does not fire separately once `PreToolUse` decides
   — key off `PreToolUse`.
3. **Hook injection** — **resolved (Phase 1, 2026-06-11)**: write to
   `.claude/settings.local.json`, NOT `settings.json`. The repo's `settings.json` is
   usually tracked and holds the team's safety hooks; overwriting it clobbers them and
   creates a spurious diff the agent flags (observed). `settings.local.json` is gitignored
   and its hooks merge additively, so the repo's hooks stay active and ours fire too.
   harmony merges idempotently (replacing only its own entries). `--settings <path>` is a
   viable alternative (cmd-line tier, merges) if we later want zero worktree files.
4. **MCP / folder-trust startup prompts** (autonomy blocker): launching in a repo with a
   `.mcp.json` or a never-trusted worktree shows interactive prompts that would hang an
   *unattended* session. No documented setting pre-approves these for interactive mode.
   Supervised mode: user answers once per worktree (trust + MCP choices persist on disk).
   For autonomy: bypass MCP via `--strict-mcp-config` and bootstrap folder-trust once.
   Deferred with the autonomy work.
4. **Resume fidelity**: terminal scrollback rebuilt from JSONL is an approximation of the
   live TUI; acceptable, not pixel-identical.
5. **Jira transitions**: status names/IDs vary per project workflow — writeback must
   discover valid transitions per issue via the API, not hardcode names.
6. **"Draft from Jira"** repo scan adds latency/cost; keep it optional and bounded.
