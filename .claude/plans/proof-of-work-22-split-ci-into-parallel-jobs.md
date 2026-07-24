# Proof of work — Split CI into parallel jobs + Husky pre-commit hook

Branch: `harmony/local-22-split-ci-into-parallel-jobs`

> Note on CI evidence: this branch is not yet pushed, and this session is not permitted to
> push/commit, so the four jobs cannot be observed on GitHub Actions here. Instead I ran
> **each job's exact command locally** (they all pass) and statically verified the workflow
> defines four parallel jobs with the required per-job toolchain. Once pushed,
> `gh run list --workflow=ci.yml` will show the four jobs running concurrently.

## What works now

The monolithic serial `task ci` GitHub Actions job is now **four independent parallel
jobs** — `rustfmt`, `clippy`, `rust-test`, and `frontend` — each slimmed to only the
toolchain it needs, with a `concurrency` group that cancels superseded runs. A new
**Husky pre-commit hook** catches the two cheap classes of failure locally: staged `.rs`
files are gated by `cargo fmt --all --check` and staged `.ts`/`.tsx` files by
`tsc --noEmit`; it's check-only (never rewrites files), skips each tool when nothing
relevant is staged, and leaves clippy + tests to CI.

## How to see it

Visual summary: **`proof-summary.png`** (in `/Users/nathan/.harmony/proof/22`).

Reproduce locally:

```sh
# The four jobs' exact commands (all green):
task fmt:check        # rustfmt job
task lint             # clippy job
task test             # rust-test job (incl. flow_doc drift guard)
task app:typecheck    # frontend job, step 1
task app:test         # frontend job, step 2 (vitest)

# Confirm the workflow defines four parallel jobs:
yq e '.jobs | keys' .github/workflows/ci.yml

# Hook is active after `task setup`:
git config core.hooksPath          # -> .husky/_

# Exercise the hook without committing (stage a file, invoke the hook directly):
printf '\nfn __x(){let _y=1;}\n' >> core/src/lib.rs && git add core/src/lib.rs
sh .husky/pre-commit               # blocks: exit 1, "run `task fmt` and re-stage"
git restore --staged . && git restore .
```

## Evidence

### 1 · The workflow now defines four parallel jobs

`yq e '.jobs | keys | .[]' .github/workflows/ci.yml`:

```
rustfmt
clippy
rust-test
frontend
```

Per-job toolchain (verbatim step list, confirming each job is slimmed correctly):

```
════ JOB: rustfmt ════   runs-on: macos-14
  - actions/checkout@v4
  - dtolnay/rust-toolchain@stable [rustfmt]
  - arduino/setup-task@v2
  - Check formatting            -> task fmt:check      (NO rust-cache, NO Node)

════ JOB: clippy ════    runs-on: macos-14
  - actions/checkout@v4
  - dtolnay/rust-toolchain@stable [clippy]
  - Swatinem/rust-cache@v2
  - arduino/setup-task@v2
  - Lint (clippy -D warnings)   -> task lint           (NO Node)

════ JOB: rust-test ════ runs-on: macos-14
  - actions/checkout@v4
  - dtolnay/rust-toolchain@stable
  - Swatinem/rust-cache@v2
  - arduino/setup-task@v2
  - Run Rust tests (incl. flow_doc drift guard) -> task test   (NO Node)

════ JOB: frontend ════  runs-on: macos-14
  - actions/checkout@v4
  - actions/setup-node@v4 [24.16.0]
  - arduino/setup-task@v2
  - Install frontend deps       -> npm ci (working-directory: app)
  - Type-check (tsc)            -> task app:typecheck
  - Unit tests (vitest)         -> task app:test        (NO Rust toolchain)
```

Concurrency group and triggers (unchanged triggers, new concurrency):

```yaml
concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true
on:
  push:
    branches: [main]
  pull_request:
    types: [opened, synchronize, reopened]
```

`npm ci` appears exactly once, scoped to the frontend job's `app/` dir — no job installs
at the repo root:

```
.github/workflows/ci.yml:95:        working-directory: app
.github/workflows/ci.yml:96:        run: npm ci
```

### 2 · All five checks pass locally (the four jobs' exact commands)

See `ci-jobs-rustfmt-and-typecheck.log`, `ci-jobs-clippy-and-rust-test.log`,
`ci-job-frontend-vitest.log`.

`task fmt:check` and `task app:typecheck` — clean (no output = pass):

```
=== task fmt:check (rustfmt job) — no output means clean, exit 0 ===
task: [fmt:check] export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt --all --check

=== task app:typecheck (frontend job) — no output means clean, exit 0 ===
task: [app:typecheck] npx tsc --noEmit
```

`task lint` (clippy) and `task test`:

```
=== task lint (clippy job) ===
task: [lint] export PATH="$HOME/.cargo/bin:$PATH"; cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.71s
CLIPPY_EXIT=0

=== task test (rust-test job) ===
...
running 1 test
test flow_doc_matches_state_machine ... ok        <-- the docs/flow.md drift guard

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.96s
...
RUSTTEST_EXIT=0
```

Test totals across the workspace: **255 passed, 0 failed** (harmony_app_lib 20 · harmony_core
125 · flow 67 · flow_doc 1 · integration 42; 4 live tests ignored as expected).

`task app:test` (frontend job, vitest step):

```
> vitest run

 ✓ src/types.test.ts (5 tests) 3ms
 ✓ src/lib/terminalScroll.test.ts (6 tests) 2ms
 ✓ src/components/QuestionCard.test.tsx (7 tests) 41ms
 ✓ src/styles.test.ts (1 test) 1ms

 Test Files  4 passed (4)
      Tests  19 passed (19)
   Duration  1.52s
```

### 3 · Husky pre-commit hook — every branch exercised (nothing committed)

Full transcript: `husky-hook-all-scenarios.log`. The hook was invoked directly
(`sh .husky/pre-commit`) after staging files, and the working tree was restored to clean
after each scenario. Husky is active:

```
$ git config core.hooksPath
.husky/_
```

**No-op (docs-only staged):** neither cargo nor tsc runs.

```
Staged: README.md
$ sh .husky/pre-commit
  → hook exit code: 0
```

**Happy path (well-formatted `.rs`):** fmt-check runs and passes.

```
Staged: core/src/lib.rs
$ sh .husky/pre-commit
pre-commit: checking Rust formatting (cargo fmt --all --check)…
  → hook exit code: 0
```

**fmt gate (staged `.rs` with drift) — commit BLOCKED:**

```
Injected: fn __proof_drift(){let _x=1;}
$ sh .husky/pre-commit
pre-commit: checking Rust formatting (cargo fmt --all --check)…
-fn __proof_drift(){let _x=1;}
+fn __proof_drift() {
+    let _x = 1;
+}
pre-commit: formatting drift — run `task fmt` and re-stage.
  → hook exit code: 1
```

**…then `task fmt` fixes it, re-stage → passes:**

```
$ task fmt
task: [fmt] export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt --all
$ sh .husky/pre-commit
pre-commit: checking Rust formatting (cargo fmt --all --check)…
  → hook exit code: 0
```

**Selective — `.rs`-only stage does NOT trigger tsc:** only the Rust line prints.

```
Staged: core/src/lib.rs
$ sh .husky/pre-commit
pre-commit: checking Rust formatting (cargo fmt --all --check)…
  → hook exit code: 0
(no 'type-checking frontend' line)
```

**tsc gate (`.tsx`-only with a type error) — commit BLOCKED, and cargo fmt does NOT run:**

```
Staged: app/src/App.tsx
Injected: const __proof_bad: number = "not a number";
$ sh .husky/pre-commit
pre-commit: type-checking frontend (tsc --noEmit)…
src/App.tsx(1214,7): error TS2322: Type 'string' is not assignable to type 'number'.
pre-commit: type errors — fix them and re-commit.
  → hook exit code: 1
(no 'checking Rust formatting' line)
```

**…then fixed → passes.** Final state — tree restored, nothing committed:

```
$ git status --porcelain
(empty)
```

### 4 · Supporting config (static verification)

Root `package.json` (new): `private: true`, `husky ^9`, `prepare: husky`; committed
`package-lock.json` pins `husky 9.1.7`. `.gitignore` adds `/node_modules`. `task setup`
runs `npm install` (root, activates husky) + `cd app && npm ci`.

```json
{
  "private": true,
  "devDependencies": { "husky": "^9" },
  "scripts": { "prepare": "husky" }
}
```

## Evidence files (in `/Users/nathan/.harmony/proof/22`)

- `proof-summary.png` — one-page visual summary of the whole change
- `ci-jobs-rustfmt-and-typecheck.log` — `task fmt:check` + `task app:typecheck` (clean)
- `ci-jobs-clippy-and-rust-test.log` — `task lint` + `task test` (clippy exit 0; 255 tests, flow_doc guard)
- `ci-job-frontend-vitest.log` — `task app:test` (19 vitest tests)
- `husky-hook-all-scenarios.log` — full pre-commit hook transcript, all gates
