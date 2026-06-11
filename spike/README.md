# harmony spike — Task 0.1 (interactive-hook proof)

The de-risking spike from [`../PLAN.md`](../PLAN.md). It answers one question before any
real code is written:

> **Do Claude Code's HTTP hooks fire for an _interactive_ PTY session, and can a hook
> _return_ a permission decision that actually controls the session?**

The entire attention model (Q13) and live board state (Q14) depend on "yes."

## Prerequisites
- Rust toolchain (`cargo`).
- `claude` on your `PATH`, logged in (`claude` runs interactively for you today).
- `git` (used to `git init` the scratch repo; optional).

## Run it
```bash
cd spike
cargo run
```
That uses `./spike-scratch`, hook port `8787`, and decision policy `allow`.

When Claude launches it will try to use the **Write** tool (needs permission in
`default` mode). If hooks work, you'll see `========== HOOK: ... ==========` blocks
printed by the spike as each event arrives. **If Claude asks you to trust the folder or
its hooks, answer `yes`** (type into the terminal — stdin is bridged to the PTY).

### Knobs
```bash
HARMONY_SPIKE_DECISION=allow cargo run    # auto-approve (file created, no TUI prompt) ← default
HARMONY_SPIKE_DECISION=deny  cargo run    # auto-deny   (Write blocked)
HARMONY_SPIKE_DECISION=ask   cargo run    # fall through to the normal TUI prompt (you answer)
HARMONY_SPIKE_PORT=9001 cargo run
HARMONY_SPIKE_PROMPT="…your own tool-using prompt…" cargo run
cargo run -- /tmp/my-scratch              # custom scratch dir
```

## Pass criteria (maps to PLAN Task 0.1)
The summary printed at exit shows PASS/MISS per event. You're looking for:

- [ ] `SessionStart` received, with a `session_id`.
- [ ] **`PreToolUse` received before the Write runs**, carrying `tool` + `tool_input`
      (the proposed content). ← the critical one.
- [ ] With `DECISION=allow`: the Write **executes with no TUI prompt** →
      `spike-scratch/spike-proof.txt` exists. With `DECISION=deny`: it's **blocked**.
      → proves a hook can programmatically control an interactive session.
- [ ] `Stop` fires at end of turn; `SessionEnd` on exit.
- [ ] `~/.claude/projects/<hash>/<session_id>.jsonl` exists for the observed `session_id`.

> `PermissionRequest` may show MISS even on success — if `PreToolUse` already returns a
> decision, Claude may not emit a separate `PermissionRequest`. `PreToolUse` is the one
> that matters for harmony.

## If it fails
- **No hook POSTs at all** → interactive hooks aren't loaded from project
  `.claude/settings.json` on this version. Try the user-level `~/.claude/settings.json`,
  or check whether hooks need explicit trust/approval. If interactive hooks are truly
  unavailable, fall back to the **notify-only** attention model (badge from `Stop`/
  `Notification`, user always answers in the terminal) and shelve the triage UI — record
  this in `DESIGN.md` before Phase 3.
- **Hooks fire but `permissionDecision` is ignored** → the response schema differs on
  your version. The spike sends both `hookSpecificOutput.permissionDecision` and a
  top-level `permissionDecision`; inspect the logged request and adjust the response shape
  in `hook_handler` to match. The decision-control capability is what we need; the exact
  JSON is an implementation detail to pin down here.

## Notes
- The PTY bridge is intentionally minimal (no termios raw-mode), so the Claude TUI may
  render a little roughly — fine for a spike. The real app will host it in `xterm.js`.
- This crate is throwaway: it validates `portable-pty` + `axum` + the hook contract, the
  exact pieces the real Rust core will use.
