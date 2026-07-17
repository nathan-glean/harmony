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

That one-time install is the last manual step: harmony **auto-updates**. On launch it checks the
latest release and, if a newer (Tauri-signed) build exists, asks to install it and restarts — no
reinstall per version. (The update is Tauri-signature-verified but not yet Apple-notarized, so a
future macOS could still show a Gatekeeper prompt on an update; notarization is the planned fix.)

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

CI (`.github/workflows/ci.yml`) runs `task ci` on every push/PR. Releases are built and published by
`.github/workflows/release.yml` on a `v*` tag:

1. Bump the version in the **four** files that must stay in sync — `app/src-tauri/tauri.conf.json`
   (authoritative for the bundle), `app/src-tauri/Cargo.toml`, `core/Cargo.toml`, and
   `app/package.json`.
2. Commit as `Release vX.Y.Z`.
3. Tag and push:

   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```

The workflow builds the Apple-Silicon `.dmg` and attaches it to a new GitHub Release. (A manual
**Run workflow** / `workflow_dispatch` builds without publishing — a dry run.)

The DMG is unsigned; to ship a notarized, double-click-installable build later, add a `bundle.macOS`
signing block to `tauri.conf.json` and the Apple signing secrets to the release workflow.
