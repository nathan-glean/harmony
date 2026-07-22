# Proof of Work — Fix window padding (#19)

## What works now

The shared content region (`.main`) now carries `padding-bottom: 16px`, so content no
longer sits flush against the bottom edge of the app window — every view (Board, Sessions,
etc.) gets a consistent 16px gap beneath it. A new Vitest test guards the rule so it can't
silently disappear, and the full CI gate stays green.

The one-line change:

```diff
-.main { display: flex; flex: 1; min-height: 0; }
+.main { display: flex; flex: 1; min-height: 0; padding-bottom: 16px; }
```
(`app/src/styles.css:100`)

## How to see it

**Run the tests (the shipped verification):**
```bash
cd app && npm test            # or: task app:test
task ci                       # full gate: rust fmt/lint/test + app typecheck/test
```

**Eyeball the gap** — the screenshots below were produced by loading the branch's *actual*
`app/src/styles.css` in a headless Chromium (Playwright) against a faithful rebuild of the
real Board DOM (class names + column labels taken verbatim from `components/Board.tsx` and
`types.ts`). The **only** added styling is an annotation: a dashed outline on the `.board`
content box and a red ruler at the window's bottom edge, so the padding band is visible.
The "before" image is the same render with `padding-bottom` forced back to `0`.

The gap was measured in the live layout with
`window.innerHeight - board.getBoundingClientRect().bottom`:

| Variant | Measured gap below content |
|---|---|
| before (rule removed) | **0px** — flush |
| after (this branch)   | **16px** |

## Evidence

### Screenshots (in `/Users/nathan/.harmony/proof/19`)

- `00-before-after-side-by-side.png` — full-window Board, before vs after, single glance.
- `01-before-flush-no-padding.png` — before: `.board` outline runs to the window edge.
- `02-after-with-16px-padding.png` — after: 16px gap between content and window edge.
- `03-before-bottom-edge-zoom.png` — zoomed bottom-left: dashed outline flush with the red edge.
- `04-after-bottom-edge-zoom.png` — zoomed bottom-left: clear gap band above the red edge.

Live layout measurement (Playwright, verbatim script output):
```
{"beforeGap":0,"afterGap":16}
```

### `npm test` — new test passes (verbatim, `--reporter=verbose`)
```
 RUN  v2.1.9 /Users/nathan/.harmony/worktrees/harmony/harmony__local-19-fix-window-padding/app

 ✓ src/types.test.ts > parseActivity > parses a valid activity JSON
 ✓ src/types.test.ts > parseActivity > returns null for empty or unparseable input
 ✓ src/types.test.ts > parseProofArtifacts > parses a JSON array of artifacts
 ✓ src/types.test.ts > parseProofArtifacts > returns [] for empty, unparseable, or non-array input
 ✓ src/types.test.ts > board columns > has a label for every column, in lifecycle order
 ✓ src/styles.test.ts > styles.css > gives the .main content region a bottom padding

 Test Files  2 passed (2)
      Tests  6 passed (6)
```

### The test is a real regression guard, not a no-op

Running the test's exact `.main` regex assertion against both CSS variants:
```
current branch CSS -> guard passes: true
rule removed        -> guard passes: false (test would FAIL — regression caught)
```

### `npx tsc --noEmit` — frontend typechecks clean
The new test's `@ts-ignore` (for `node:fs` without `@types/node`) does not leak errors:
```
TYPECHECK EXIT: 0
```

### `task ci` — full gate green (verbatim tail)
```
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s
...
task: [app:typecheck] npx tsc --noEmit
task: [app:test] npm test

 ✓ src/types.test.ts (5 tests) 2ms
 ✓ src/styles.test.ts (1 test) 1ms

 Test Files  2 passed (2)
      Tests  6 passed (6)

CI EXIT: 0
```

## Notes on scope

- CSS change is exactly the single `padding-bottom: 16px` addition to the existing `.main`
  rule — `.app`, the topbar, and per-view padding are untouched. The `16px` matches the
  existing convention used by `.board`/`.sessions`.
- The test runs in the existing `node` Vitest environment with no config changes and no new
  dependencies (`app/vitest.config.ts` already includes `src/**/*.test.ts`).
- Playwright was used only to *capture* this evidence (installed in a scratchpad, never added
  to the repo); it is not a project dependency and no browser/visual tooling was added to the app.
