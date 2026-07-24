//! Proportional re-verification triage.
//!
//! After a ticket has been reviewed/proofed once, any new commit moves HEAD and — because the
//! review/judge/proof gates key on the exact HEAD sha (the action log) — would otherwise re-run the
//! full review + proof. Most loop-back commits don't warrant that. [`decide`] classifies the
//! *incremental* diff (since the last verified commit) into whether it needs a fresh review and/or a
//! fresh proof, so trivial changes carry the prior verification forward and only meaningful changes
//! re-verify.
//!
//! Hybrid: a cheap, LLM-free [`heuristic`] short-circuits obviously-trivial changes (docs / comments
//! / formatting only); everything else falls to [`triage`], a read-only `claude -p` call (the same
//! pattern as [`crate::review::judge`]) that flags *behaviour-changing* edits — so a small-but-breaking
//! fix still gets `needs_proof`.
//!
//! Note: `claude -p` counts against separate Agent-SDK usage credits; the driver triages each HEAD at
//! most once (fingerprinted in the action log), not on every poll.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::draft::truncate_diff;

/// Max diff bytes piped to the triage call — keep it fast and within context.
const MAX_DIFF_BYTES: usize = 60 * 1024;

/// What re-verification a change since the last verified commit warrants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The change warrants a fresh code review.
    pub needs_review: bool,
    /// The change alters user-visible behaviour, so the proof is stale and must be regenerated.
    pub needs_proof: bool,
}

impl Decision {
    /// Everything is up to date — carry the prior review + proof forward, run nothing.
    pub const SKIP: Decision = Decision {
        needs_review: false,
        needs_proof: false,
    };
    /// The full pipeline (used as the fail-safe when the classifier can't be trusted).
    pub const FULL: Decision = Decision {
        needs_review: true,
        needs_proof: true,
    };
}

/// File extensions/paths that never change behaviour on their own — a change touching only these is
/// safe to skip (no re-review, no re-proof).
fn is_prose_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".md")
        || p.ends_with(".mdx")
        || p.ends_with(".markdown")
        || p.ends_with(".txt")
        || p.ends_with(".rst")
        || p.ends_with("license")
        || p.starts_with("docs/")
        || p.contains("/docs/")
}

/// LLM-free fast path. `Some(Decision::SKIP)` when every changed file is prose/docs; `None` when the
/// change touches anything else (code, config, tests) and must be judged by [`triage`].
pub fn heuristic(changed_paths: &[String]) -> Option<Decision> {
    if changed_paths.is_empty() {
        // No file-level changes we can see (e.g. an empty/merge commit) — nothing to re-verify.
        return Some(Decision::SKIP);
    }
    if changed_paths.iter().all(|p| is_prose_path(p)) {
        Some(Decision::SKIP)
    } else {
        None
    }
}

/// Parse `git diff --name-only`-style output into a path list.
pub fn changed_paths(name_only: &str) -> Vec<String> {
    name_only
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Sentinels the triage call emits on its first line.
const REVERIFY: &str = "REVERIFY";
const SKIP: &str = "SKIP";

/// Ask `claude -p` (read-only) whether the incremental `diff` warrants re-review and/or re-proof.
/// Fail-safe: on any spawn/parse failure the caller should fall back to [`Decision::FULL`].
pub fn triage(worktree: &str, diff: &str) -> Result<Decision> {
    let prompt = format!(
        "You are the re-verification gate for an autonomous coding loop. A change was already \
         reviewed and proven to work at an earlier commit; the INCREMENTAL diff since then is on \
         stdin. Decide what re-verification the NEW change warrants.\n\n\
         - Needs REVIEW when the code logic changed in a way worth a fresh code review (not pure \
         formatting, comments, or trivial renames).\n\
         - Needs PROOF when it changes user-visible behaviour or fixes/affects how the feature \
         actually works (even a tiny fix to a breaking bug needs proof). Pure refactors, comments, \
         tests, and formatting do NOT need proof.\n\n\
         Respond in EXACTLY this format and nothing else:\n\
         - First line: `{SKIP}` (no re-verification needed) or `{REVERIFY}`.\n\
         - If `{REVERIFY}`, a second line with a space-separated subset of `review` `proof` for what \
         is needed (e.g. `review` or `review proof`).\n\
         No preamble, no explanation, no code fences."
    );

    let mut child = Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--permission-mode")
        .arg("plan")
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(truncate_diff(diff, MAX_DIFF_BYTES).as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify(
            "claude",
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(parse_triage(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse the triage response. Fail-safe toward *more* verification: an explicit `SKIP` first line
/// skips; anything else (incl. `REVERIFY`, empty, or garbage) re-verifies. `REVERIFY` with no
/// recognised second line defaults to the full pipeline.
fn parse_triage(raw: &str) -> Decision {
    let mut lines = raw.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("");
    if first.eq_ignore_ascii_case(SKIP) {
        return Decision::SKIP;
    }
    // REVERIFY (or anything non-SKIP): read the needs from the next line; default to full.
    let needs = lines.next().unwrap_or("").to_ascii_lowercase();
    if needs.contains("review") || needs.contains("proof") {
        Decision {
            needs_review: needs.contains("review"),
            needs_proof: needs.contains("proof"),
        }
    } else {
        Decision::FULL
    }
}

/// Decide re-verification for the incremental change: heuristic fast-path, else the LLM triage,
/// falling back to the full pipeline if the LLM call fails.
pub fn decide(worktree: &str, changed: &[String], diff: &str) -> Decision {
    if let Some(d) = heuristic(changed) {
        return d;
    }
    triage(worktree, diff).unwrap_or(Decision::FULL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_skips_docs_only() {
        assert_eq!(
            heuristic(&["README.md".into(), "docs/guide.md".into()]),
            Some(Decision::SKIP)
        );
        assert_eq!(heuristic(&[]), Some(Decision::SKIP));
    }

    #[test]
    fn heuristic_defers_when_code_touched() {
        assert_eq!(heuristic(&["src/lib.rs".into(), "README.md".into()]), None);
        assert_eq!(heuristic(&["tests/foo.rs".into()]), None);
    }

    #[test]
    fn parse_skip() {
        assert_eq!(parse_triage("SKIP"), Decision::SKIP);
        assert_eq!(parse_triage("  skip \nnothing to do"), Decision::SKIP);
    }

    #[test]
    fn parse_reverify_subset() {
        assert_eq!(
            parse_triage("REVERIFY\nreview"),
            Decision {
                needs_review: true,
                needs_proof: false
            }
        );
        assert_eq!(
            parse_triage("REVERIFY\nreview proof"),
            Decision {
                needs_review: true,
                needs_proof: true
            }
        );
    }

    #[test]
    fn parse_garbage_is_fail_safe_full() {
        // Non-SKIP with no recognisable needs => full pipeline (never silently skip).
        assert_eq!(parse_triage("REVERIFY"), Decision::FULL);
        assert_eq!(parse_triage(""), Decision::FULL);
        assert_eq!(parse_triage("blah blah"), Decision::FULL);
    }
}
