# Proof of work — Fix issues with multi-select

Branch: `harmony/local-18-fix-issues-with-multi-select` · Commit `2c13b01`

## What works now

Multi-select `AskUserQuestion` answers now deliver correctly to Claude's TUI, and the multi-select
card needs no confirm button: clicking options toggles them (multiple at once), and pressing **Enter**
submits the whole selection. On the backend, keystrokes are now written **one flushed chunk per key
with a short gap**, so the Space toggles the live Ink prompt used to swallow now each land — the exact
bytes are asserted by unit tests. Single-select (click-to-submit) and custom-answer (type + Send) are
unchanged.

## How to see it

**Rust keystroke-builder unit tests** (the backend fix — proves the exact bytes sent per answer):

```
cd app/src-tauri && ~/.cargo/bin/cargo test
```

**Frontend Vitest suite** (the `QuestionCard` behavior — toggle, no confirm button, Enter-to-submit):

```
cd app && npx vitest run
```

**Visual walkthrough of the real component:** the screenshots and `walkthrough.webm` in
`/Users/nathan/.harmony/proof/18` were produced by rendering the **unmodified** `QuestionCard.tsx` +
`styles.css` in a browser (Playwright) with the **real** `api.answerQuestion` bridge wired up. Only
Tauri's `invoke` core was stubbed to record the payload the frontend delivers to the Rust
`answer_question` command — so the JSON panel in each shot is the genuine end-to-end payload.

> Note on scope: the spec's *primary* manual check drives the live Claude Code TUI end-to-end. That
> needs a built Tauri desktop app plus a running Claude session and can't be captured headlessly here.
> The backend half of that path is proven instead by the keystroke unit tests (the exact bytes
> `deliver_answer` emits), and the frontend half by the browser walkthrough capturing the real
> delivered payload.

## Evidence

### 1. Rust unit tests — exact keystrokes per answer (12 passed)

Verbatim from `cargo test`:

```
running 12 tests
test tests::custom_answer_takes_precedence_over_multi_select ... ok
test tests::custom_answer_walks_to_other_then_types ... ok
test tests::every_chunk_is_one_complete_key ... ok
test tests::empty_custom_text_falls_through_to_selection ... ok
test tests::multi_select_all_options ... ok
test tests::multi_select_empty_is_a_bare_enter ... ok
test tests::multi_select_middle_option_only ... ok
test tests::multi_select_sorts_and_dedups_picks ... ok
test tests::multi_select_toggles_each_pick_then_confirms ... ok
test tests::single_select_empty_defaults_to_first_option ... ok
test tests::single_select_first_option_is_bare_enter ... ok
test tests::single_select_later_option_navigates_then_confirms ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Key assertions (from `app/src-tauri/src/lib.rs` `#[cfg(test)] mod tests`):
- **Multi-select `[0,2]` of 3** → `[SPACE, DOWN, DOWN, SPACE, ENTER]` — toggle item 0, move to item 2,
  toggle it, confirm. Each element is one complete key, written+flushed separately so the toggles are
  not dropped.
- **Multi-select all `[0,1,2]`** → `[SPACE, DOWN, SPACE, DOWN, SPACE, ENTER]`.
- **Multi-select `[]`** → `[ENTER]` (UI treats zero picks as a no-op before this is ever called).
- **Single-select first option `[0]`** → `[ENTER]`; **`[2]`** → `[DOWN, DOWN, ENTER]` (unchanged).
- **Custom answer** → walk past all real options, `ENTER`, type text, `ENTER`; wins even when
  `multiSelect` is set.
- `every_chunk_is_one_complete_key` guards that escape sequences are never split across chunks (so a
  lone `ESC` can't be misread).

### 2. Frontend Vitest suite — QuestionCard behavior (12 passed)

Verbatim from `npx vitest run`:

```
 RUN  v2.1.9 /Users/nathan/.harmony/worktrees/harmony/harmony__local-18-fix-issues-with-multi-select/app

 ✓ src/components/QuestionCard.test.tsx (7 tests) 32ms
 ✓ src/types.test.ts (5 tests) 2ms

 Test Files  2 passed (2)
      Tests  12 passed (12)
```

The `QuestionCard.test.tsx` cases assert: clicking options toggles `picked` without calling the
backend; no "Send N selected" button renders (only a hint); Enter submits sorted indices with
`multiSelect: true`; Enter with zero picks is a no-op; Enter inside the custom field submits the typed
text instead; and single-select still submits on click and ignores Enter.

### 3. Visual walkthrough (real component, real delivered payload)

Video: **`walkthrough.webm`** — full flow (toggle, toggle-off, Enter-to-submit, single-select click,
custom answer).

Screenshots (in `/Users/nathan/.harmony/proof/18`):

- **`01-multiselect-initial.png`** — multi-select card. No confirm button; hint reads
  "Click options to select, then press ↵ Enter to send".
- **`02-multiselect-two-picked.png`** — Auth (0) and Search (2) clicked in non-sorted order; both show
  the accent highlight, Billing does not. Hint updates to "↵ Enter to send 2 selected".
- **`03-multiselect-toggled-off.png`** — clicking Auth again removes its highlight (toggle works both
  ways).
- **`04-multiselect-enter-delivered.png`** — after pressing Enter, the payload panel shows the real
  `invoke("answer_question", …)` call the frontend delivered:

```json
{
  "cmd": "answer_question",
  "args": {
    "sessionId": 42,
    "optionCount": 3,
    "selected": [ 0, 2 ],
    "customText": null,
    "multiSelect": true
  }
}
```

- **`05-multiselect-enter-empty-noop.png`** — a fresh multi-select card with zero picks; pressing Enter
  fires no new delivery (the payload panel still shows the *previous* `[0,2]` value — nothing new was
  sent).
- **`06-singleselect-initial.png` / `07-singleselect-click-delivered.png`** — single-select: clicking
  "No, hold" (index 1) submits immediately (no Enter needed). Delivered payload:

```json
{ "cmd": "answer_question", "args": { "sessionId": 42, "optionCount": 2, "selected": [ 1 ], "customText": null, "multiSelect": false } }
```

- **`08-custom-answer-typed.png` / `09-custom-answer-delivered.png`** — typing "Ship a slimmer MVP
  first" and pressing Enter in the field delivers:

```json
{ "cmd": "answer_question", "args": { "sessionId": 42, "optionCount": 3, "selected": [], "customText": "Ship a slimmer MVP first", "multiSelect": true } }
```

These four payloads were also printed verbatim by the Playwright driver, confirming the acceptance
criteria: sorted indices for multi-select, immediate single-select on click, custom text carried
through, and Enter-with-nothing-selected being a no-op.

## Environment notes

- `ffmpeg` was not available, so the walkthrough is `.webm` (records natively via Playwright; plays in
  any modern browser) rather than `.mp4`.
- All evidence lives under `/Users/nathan/.harmony/proof/18`; no repo files were modified. The browser
  harness used to render the real component lives only in the session scratchpad.
