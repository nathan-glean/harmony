# harmony — Design

A local, per-developer desktop tool that sits **between Jira and Claude Code**: it
ingests your assigned Jira tickets (plus local-only tickets), lets you enrich each
with an agent-friendly spec, and runs an isolated, supervised Claude Code session
per ticket in its own git worktree — then opens a PR and hands off to your normal
review process.

Reference points: closest to OpenAI's **Symphony** (orchestrate agents from a
tracker) but **supervised** like **claude-commander** (worktree-per-task, live
sessions), with a richer GUI and a Jira enrichment layer neither has.

## Resolved decisions

| # | Decision | Choice |
|---|----------|--------|
| 1 | Autonomy model | **Supervised-first**; full autonomy is a per-ticket opt-in (just an elevated permission mode), off by default |
| 2 | Deployment | **Local, one instance per developer**. No server, no multi-user auth. Reads shared Jira |
| 3 | Form factor | **Tauri desktop app** — Rust core + web frontend |
| 4 | Claude driver | **PTY-hosted interactive `claude`** per session (xterm.js), native UX. Status from **HTTP hooks** + tailing the session JSONL. No headless/SDK in the loop |
| 5 | Ticket model | **Enriched local tickets**, each optionally **linked to a Jira issue**. Local-only work = unlinked ticket. One unified board |
| 6 | Jira read scope | **Assigned to me** only (no JQL box) |
| 7 | Jira writeback | **Opt-in minimal**: status transition (start → In Progress, PR → In Review) + post PR link as comment. Each toggleable |
| 8 | Repo model | **Multi-repo**; pick target repo at start, default remembered per Jira project key |
| 9 | Worktree scope | **Per-ticket, reused** across sessions (1 ticket → 1 worktree → 1 branch → 1 PR). "New alternate attempt" forks a second worktree on demand |
| 10 | Spec authoring | **Light structure + AI draft**: markdown body + optional fields (acceptance criteria, relevant paths, constraints). "Draft from Jira" expands the terse issue into an editable first pass |
| 11 | Output flow | **Open a draft PR via `gh`** (body from spec + optional Claude summary), link to Jira, show diff in-app. **harmony never merges** — hand off to normal review/CI |
| 12 | Session persistence | **Resume-on-relaunch**: PTYs are children of harmony; on relaunch resume each via `claude --resume <id>`, rebuild view from transcript. No tmux |
| 13 | Attention model | **Notify + jump-to-terminal** (v1): card badge + OS notification from hooks; answer in embedded terminal. Native approve/deny **triage UI** is the north-star evolution |
| 14 | Board model | **harmony-native lifecycle columns**: Available → Ready → Working → Waiting → In Review → Done. Working/Waiting from hooks; In Review/Done from PR+Jira |

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

### Per-session flow
1. Pick ticket → choose repo (defaulted) → write/Draft spec.
2. Worktree created off fresh default branch; branch `harmony/<KEY>-<slug>`.
3. harmony writes per-worktree `.claude/settings.json` with HTTP hooks pointing at its
   local server, then spawns interactive `claude` in a PTY (cwd = worktree).
4. You supervise in the embedded terminal; board state tracks hooks (Working/Waiting).
   Optional autonomy = launch with elevated `--permission-mode`.
5. On finish: push branch, `gh pr create` (draft), transition Jira + comment PR link.
6. Hand off to normal review. Close harmony anytime → resume later via `--resume`.

## Confirmed environment
- **Jira Cloud** + email/API-token auth (keychain-stored). _(confirmed)_
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
3. **Hook injection**: harmony must reliably write hook config into each worktree's
   `.claude/settings.json` (or pass `--settings`) with a stable local port + shared secret.
4. **Resume fidelity**: terminal scrollback rebuilt from JSONL is an approximation of the
   live TUI; acceptable, not pixel-identical.
5. **Jira transitions**: status names/IDs vary per project workflow — writeback must
   discover valid transitions per issue via the API, not hardcode names.
6. **"Draft from Jira"** repo scan adds latency/cost; keep it optional and bounded.
```
