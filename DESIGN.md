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
| 11 | Output flow | **Open a draft PR via `gh`** (body from spec + optional Claude summary), link to Jira, show diff in-app. **harmony never merges** — hand off to normal review/CI |
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
│   ├── PR/gh integration        (push branch, gh pr create draft)
│   └── Store                    (SQLite: tickets, spec, ticket↔repo↔worktree↔session map, settings)
└── Web frontend
    ├── Board (native lifecycle columns)
    ├── Ticket detail + spec editor ("Draft from Jira")
    ├── Embedded terminals (xterm.js over PTY)
    └── Diff / PR panes
```

### Ticket lifecycle state machine
The board's behaviour — which column a ticket lands in and what side effects run — is a single pure
decision function, `flow::decide` (`core/src/flow.rs`), driven by one shared executor
(`core/src/executor.rs`) from both the Tauri app and the headless CLI. Its contract is pinned by
`core/tests/flow.rs`, and a **generated diagram + transition table** lives at
[`docs/flow.md`](docs/flow.md) — rendered from `decide` itself (`task flow:doc`) and drift-checked in
CI, so it always matches the code.

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
- `poll_auto_merge_once` — when a PR is approved on GitHub (`reviewDecision == APPROVED`) **and** CI
  is green, inject `Move(Done)` so `decide` merges + cleans up. The agent never self-approves.
  [`auto_merge`, default off — the one irreversible, outward-facing step]

The judge is fingerprinted by HEAD (`judged_sha`) so it runs once per reviewed change, not per poll.
The generated [`docs/flow.md`](docs/flow.md) covers the underlying transitions these drivers trigger.

### Per-session flow
1. Pick ticket → choose repo (defaulted) → write/Draft spec.
2. Worktree created off fresh default branch; branch `harmony/<KEY>-<slug>`.
3. harmony writes per-worktree `.claude/settings.json` with HTTP hooks pointing at its
   local server, then spawns interactive `claude` in a PTY (cwd = worktree).
4. You supervise in the embedded terminal; board state tracks hooks (Working/Waiting).
   Optional autonomy = launch with elevated `--permission-mode`.
5. On finish: push branch, `gh pr create` (draft), transition Jira + comment PR link.
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

## Deferred to v2
- Native approve/deny **permission triage UI** (intercept via PermissionRequest/PreToolUse HTTP hooks).
- **tmux / daemon** persistence for long unattended autonomous runs that outlive the window.
- Shared **team backend** (coordination, who's-on-what, team board).
- JQL / board-based Jira scope beyond assigned-to-me; pull arbitrary issue by key.
- Giving the running agent **live MCP tools** for Jira/ticket context or progress posting.
- Cascade/auto-merge.

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
```
