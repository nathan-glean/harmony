# Proof of Work — "Draft PR"

**Question raised:** dragging a ticket from *For Your Review* → *In PR Review* opened a
PR, which is correct — but it opened a **draft** PR. Should it?

**Answer shipped:** No. Reaching *In PR Review* is the human's explicit hand-off to the
team, so the PR should be opened **ready for review**, not as a draft. This branch renames
`harmony_core::github::create_draft_pr` → `create_pr` and drops the `--draft` flag from the
`gh pr create` invocation (plus the matching CLI/UI/DESIGN wording).

## What works now

When a ticket moves into *In PR Review*, harmony runs `gh pr create` **without** `--draft`,
so GitHub opens a normal, review-ready PR — it requests reviewers, notifies them, and is
mergeable by harmony's gated/auto-merge once approved. A draft PR would do none of those and
would block `gh pr merge`.

## How to see it

The behavioral change is purely which arguments harmony hands to the `gh` CLI. The
reproduction below runs the **real** production function `create_pr` with a stub `gh` on
`PATH` (so there are no GitHub side effects) and prints the exact argv it receives:

```
/Users/nathan/.harmony/proof/20/reproduce.sh
```

To confirm it compiles everywhere the symbol is used:

```
~/.cargo/bin/cargo check --workspace          # core + app/src-tauri
~/.cargo/bin/cargo test  -p harmony-core --test live   # live PR test now uses create_pr
```

## Evidence

### `before-after.png`
Visual side-by-side: the old `--draft` arg list → DRAFT, the new arg list → READY-FOR-REVIEW,
plus the live run of the real `create_pr`.

### Real run of `create_pr()` through a stub `gh` (`demo-output.txt`, verbatim)

```
== stdout from the real create_pr() ==
create_pr() returned PR URL: https://github.com/nathan-glean/harmony/pull/999

== exact argv the real create_pr() handed to gh ==
ARGV: pr create --title HAR-20: move to In PR Review --body PR body generated from the spec / Claude diff summary. --head harmony/local-20-draft-pr
would-create: READY-FOR-REVIEW pull request (requests reviewers, mergeable)

== assertion ==
PASS: no --draft flag -> gh opens a PR ready for review
```

### Old vs new source (`git show bffe58c:core/src/github.rs` vs branch)

```rust
// BEFORE (merge-base bffe58c) — create_draft_pr
"pr", "create", "--draft", "--title", title, "--body", body, "--head", branch,

// AFTER (this branch) — create_pr
"pr", "create", "--title", title, "--body", body, "--head", branch,
```

The `--draft` token is the only functional difference; URL parsing is unchanged.

### The flag's meaning, from the real `gh` CLI (`gh pr create --help`)

```
  -d, --draft                Mark pull request as a draft
```

Removing it is exactly what flips the PR from draft to ready-for-review.

### Old vs new arg lists, exercised against the stub `gh`

```
ARGV: pr create --draft --title HAR-20 --body ... --head harmony/local-20-draft-pr
would-create: DRAFT pull request (not review-ready, blocks gh pr merge)
ARGV: pr create --title HAR-20 --body ... --head harmony/local-20-draft-pr
would-create: READY-FOR-REVIEW pull request (requests reviewers, mergeable)
```

### Build / test / cleanliness (verbatim)

```
$ cargo check --workspace
    Checking harmony-core v0.1.0 (…/core)
    Checking harmony-app v0.1.0 (…/app/src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo test -p harmony-core --test live
running 4 tests
test live_github_write_pr ... ignored, live+write: set HARMONY_LIVE_GH_WORKTREE + HARMONY_LIVE_GH_BRANCH
...
test result: ok. 0 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out

$ grep -rn "create_draft_pr" --include="*.rs" .
none — fully renamed to create_pr

$ grep -rn '"--draft"' --include="*.rs" .
none — --draft flag fully removed from source
```

The gated `live_github_write_pr` test (which would open a real PR against
`HARMONY_LIVE_GH_WORKTREE`) was updated to call `create_pr` and compiles; it stays ignored
here because opening a real PR requires pushing a branch, which this evidence session must
not do.

## Files

- `before-after.png` — rendered before/after + live-run terminal (from real output)
- `demo-output.txt` — verbatim stdout of the reproduction
- `reproduce.sh` — self-contained, re-runnable proof (builds a throwaway harness + stub `gh`)
