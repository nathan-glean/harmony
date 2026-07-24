# harmony

A desktop board that runs Claude Code sessions to implement tickets, with an autonomous
state-machine loop (grill → implement → review → PR → merge). Tauri v2 app: Rust core +
React/Vite frontend.

## What it does

- **Board → session per ticket.** Todo → In Progress → For Your Review → In PR Review → Done. Each
  ticket runs an isolated, supervised Claude Code session in its own git worktree; drag to move.
- **Jira & GitHub, both ways.** Syncs assigned Jira issues (via `acli`), drafts a spec from the issue,
  and writes status back on column moves. Opens a PR ready for review (via `gh`), and keeps the ticket
  and PR in step both directions — PR merged/closed → Done, reopened → In PR Review — with an "↗ PR"
  button and inline diff/checks.
- **Autonomous loop.** A background loop drives the board off a pure state machine: re-review on new
  commits, a self-correcting review judge → auto-fix, proof-of-work capture, CI-failure auto-fix,
  merge-conflict resolution, and gated auto-merge — all idempotent via a durable action log so nothing
  is redone after a restart.
- **Smart about loops & restarts.** Re-work returns to the furthest stage it reached; trivial changes
  skip a full re-review/re-proof; and on relaunch finished sessions are recovered (not naively resumed).
- **Supervised-first.** Every autonomous step is opt-in and capped; you watch and steer in an embedded
  terminal, with proof, diff, review and PR tabs per ticket.

## Install (macOS, Apple Silicon)

1. Download the latest **`harmony_*_aarch64.dmg`** from the
   [Releases](../../releases/latest) page.
2. Open the DMG and drag **harmony** into **Applications**.
3. The app is **unsigned**, so macOS quarantines it and may report it as "damaged" — it isn't.
   Clear the quarantine flag once, then open it normally:

   ```sh
   xattr -cr /Applications/harmony.app
   ```

> Requires an Apple Silicon Mac (M-series). Runtime features expect `gh` (GitHub) and, for Jira,
> the Atlassian CLI `acli` on your PATH.

## Development

Toolchain is pinned in `.tool-versions` (Node, [go-task](https://taskfile.dev)); Rust stable.

```sh
cd app && npm install      # frontend deps
task ci                    # fmt + clippy + tests + typecheck (what CI runs)
task tauri:dev             # run the app in dev mode
task tauri:build           # build the macOS bundle locally
```

`task --list` shows all tasks.

## Releasing

CI (`.github/workflows/ci.yml`) runs `task ci` on every push/PR. Releases are cut deliberately (never
per-PR) and built + published by `.github/workflows/release.yml` on a `v*` tag.

Bump the version and tag with one command (semantic versioning — `patch` | `minor` | `major`, or an
explicit `X.Y.Z`):

```sh
task release -- minor      # bumps every version file in lockstep, commits "Release vX.Y.Z", tags vX.Y.Z
git push --follow-tags     # review first, then push to publish the release
```

`task release` updates all version spots together (`tauri.conf.json`, both `Cargo.toml`,
`package.json` + `package-lock.json`, and `Cargo.lock`) so the tag, the bundle, and the auto-update
manifest always agree; it stops before pushing so you can review. The tag push builds the Apple-Silicon
`.dmg` and attaches it to a new GitHub Release (a CI guard rejects a tag that doesn't match the app
version). A manual **Run workflow** / `workflow_dispatch` builds without publishing — a dry run.

The DMG is unsigned; to ship a notarized, double-click-installable build later, add a `bundle.macOS`
signing block to `tauri.conf.json` and the Apple signing secrets to the release workflow.
