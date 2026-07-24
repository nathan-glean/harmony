# harmony

A desktop board that runs Claude Code sessions to implement tickets, with an autonomous
state-machine loop (grill → implement → review → PR → merge). Tauri v2 app: Rust core +
React/Vite frontend.

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

First-time setup installs the git hooks (husky, from the root `package.json`) and the
frontend deps:

```sh
task setup                 # npm install at root (activates husky) + npm ci in app/
```

If you'd rather run it by hand: `npm install` at the repo root once, then
`cd app && npm install`.

```sh
task ci                    # fmt + clippy + tests + typecheck (what CI runs), one-shot
task tauri:dev             # run the app in dev mode
task tauri:build           # build the macOS bundle locally
```

`task --list` shows all tasks.

A **pre-commit hook** (`.husky/pre-commit`) runs the cheap checks locally so trivial slips
never reach CI: staged `.rs` files are checked with `cargo fmt --all --check` (fix with
`task fmt`, then re-stage) and staged `.ts`/`.tsx` files trigger `tsc --noEmit`. It's
check-only (never rewrites your files), skips cargo/tsc when nothing relevant is staged,
and leaves clippy + tests to CI.

CI runs the same five checks as **four independent, parallel jobs** — `rustfmt`, `clippy`,
`rust-test` (incl. the `docs/flow.md` drift guard), and `frontend` (tsc + vitest) — so
each signal reports on its own instead of waiting behind the others.

## Releasing

CI (`.github/workflows/ci.yml`) runs those four parallel jobs on every push/PR. Releases are cut
deliberately (never per-PR) and built + published by `.github/workflows/release.yml` on a `v*` tag.

Bump the version and publish with one command (semantic versioning — `patch` | `minor` | `major`, or
an explicit `X.Y.Z`):

```sh
task release -- minor      # bumps every version file, opens+merges a Release PR, tags vX.Y.Z
```

Run it from a **clean `main` that's in sync with origin**, with an authenticated `gh`. `task release`
updates all version spots together (`tauri.conf.json`, both `Cargo.toml`, `package.json` +
`package-lock.json` — regenerated so it can't drift and fail `npm ci` — and `Cargo.lock`) on a
`release-vX.Y.Z` branch, opens a PR with `gh`, merges it (retrying while GitHub computes
mergeability), fast-forwards your local `main`, then tags the merged commit and pushes the tag. This
PR-based flow lands cleanly even when `main` is branch-protected. The tag push builds the
Apple-Silicon `.dmg` and attaches it to a new GitHub Release (a CI guard rejects a tag that doesn't
match the app version). A manual **Run workflow** / `workflow_dispatch` builds without publishing — a
dry run.

The DMG is unsigned; to ship a notarized, double-click-installable build later, add a `bundle.macOS`
signing block to `tauri.conf.json` and the Apple signing secrets to the release workflow.
