//! PR CI triage: decide whether a failing check is *genuinely caused by this PR* (worth an
//! automatic fix) versus noise we should only surface — main already red, a non-required job, or
//! an infra/flake failure. The gates run cheap→expensive; only a check that clears all of them is
//! actionable. All `gh`/`git`/`claude` I/O lives in `github`/`draft`; the parsing + `decide` logic
//! here is pure and unit-tested.

use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Confidence below which an LLM `PrCaused` verdict is treated as too uncertain to auto-fix.
pub const CONFIDENCE_THRESHOLD: f32 = 0.7;
/// Cap on the failed-log text fed to the model (keep the `claude -p` call fast / in-context).
const MAX_LOG_BYTES: usize = 24 * 1024;
const MAX_DIFF_BYTES: usize = 40 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiCategory {
    /// The failure is caused by something in this PR's diff.
    PrCaused,
    /// Environmental / unrelated (infra, missing secret, main already broken, dependency outage).
    UnrelatedInfra,
    /// Non-deterministic / flaky.
    Flaky,
    /// Couldn't attribute the failure with any confidence.
    Undetermined,
}

/// The LLM's attribution of a CI failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiVerdict {
    pub category: CiCategory,
    /// 0.0–1.0 confidence in `category`.
    pub confidence: f32,
    pub rationale: String,
    /// What to change to fix it (only meaningful when `category == PrCaused`).
    #[serde(default)]
    pub proposed_fix: String,
}

/// The full triage result for a ticket's PR, persisted on the ticket and shown in the Proof tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiTriage {
    /// HEAD sha this triage was computed against.
    pub head_sha: String,
    /// Failing check names reported by `gh pr checks`.
    pub failing_checks: Vec<String>,
    /// Subset of `failing_checks` also failing on the base branch (treated as unrelated).
    pub base_red_checks: Vec<String>,
    /// Required check names, when branch protection could be read (else `None` → gate skipped).
    pub required_checks: Option<Vec<String>>,
    pub verdict: Option<CiVerdict>,
    /// Whether the decision rule says we should auto-fix.
    pub actionable: bool,
    /// Human-readable reason for the decision (shown in the UI / logs).
    pub reason: String,
}

// ---- pure parsing of gh JSON ------------------------------------------------

/// Failing check names from `gh pr checks --json name,state,link,bucket` (bucket == "fail").
pub fn parse_failing_checks(checks_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(checks_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|c| {
            let bucket = c.get("bucket").and_then(|x| x.as_str()).unwrap_or("");
            let state = c.get("state").and_then(|x| x.as_str()).unwrap_or("");
            bucket.eq_ignore_ascii_case("fail")
                || state.eq_ignore_ascii_case("failure")
                || state.eq_ignore_ascii_case("error")
        })
        .filter_map(|c| {
            c.get("name")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// `databaseId`s of failed workflow runs at `head_sha`, from `gh run list --json …`. These are the
/// runs whose `--log-failed` output we fetch for attribution.
pub fn parse_failed_run_ids(run_list_json: &str, head_sha: &str) -> Vec<i64> {
    serde_json::from_str::<serde_json::Value>(run_list_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            let sha = r.get("headSha").and_then(|x| x.as_str()).unwrap_or("");
            let concl = r.get("conclusion").and_then(|x| x.as_str()).unwrap_or("");
            sha == head_sha
                && matches!(
                    concl.to_ascii_lowercase().as_str(),
                    "failure" | "timed_out" | "startup_failure"
                )
        })
        .filter_map(|r| r.get("databaseId").and_then(|x| x.as_i64()))
        .collect()
}

/// Names of checks that are red on the base commit, from `gh api …/check-runs`. Used to drop
/// failures that pre-exist on main (not caused by the PR).
pub fn parse_base_red_checks(check_runs_json: &str) -> HashSet<String> {
    serde_json::from_str::<serde_json::Value>(check_runs_json)
        .ok()
        .and_then(|v| v.get("check_runs").and_then(|x| x.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter(|c| {
            matches!(
                c.get("conclusion")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "failure" | "timed_out" | "startup_failure" | "action_required"
            )
        })
        .filter_map(|c| {
            c.get("name")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Required check names from `gh api …/required_status_checks`. `None` when the JSON can't be read
/// (no protection / no permission) — the caller then skips the "required only" gate rather than
/// treating everything as non-required.
pub fn parse_required_checks(json: &str) -> Option<HashSet<String>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut set = HashSet::new();
    // Newer API: `checks: [{context, app_id}]`; older: `contexts: [string]`.
    if let Some(arr) = v.get("checks").and_then(|x| x.as_array()) {
        for c in arr {
            if let Some(ctx) = c.get("context").and_then(|x| x.as_str()) {
                set.insert(ctx.to_string());
            }
        }
    }
    if let Some(arr) = v.get("contexts").and_then(|x| x.as_array()) {
        for c in arr {
            if let Some(ctx) = c.as_str() {
                set.insert(ctx.to_string());
            }
        }
    }
    Some(set)
}

// ---- pure decision rule -----------------------------------------------------

/// The actionable check candidates: failing checks that are required (when the required set is
/// known) and not already red on the base branch.
pub fn candidate_checks<'a>(
    failing: &'a [String],
    required: Option<&HashSet<String>>,
    base_red: &HashSet<String>,
) -> Vec<&'a String> {
    failing
        .iter()
        .filter(|c| required.is_none_or(|r| r.contains(*c)))
        .filter(|c| !base_red.contains(*c))
        .collect()
}

/// Decide whether to auto-fix: there must be at least one required, PR-specific (not base-red)
/// failing check AND the LLM must attribute it to the PR with sufficient confidence. Pure.
pub fn decide(
    failing: &[String],
    required: Option<&HashSet<String>>,
    base_red: &HashSet<String>,
    verdict: Option<&CiVerdict>,
    threshold: f32,
) -> (bool, String) {
    if failing.is_empty() {
        return (false, "no failing checks".into());
    }
    let candidates = candidate_checks(failing, required, base_red);
    if candidates.is_empty() {
        return (
            false,
            "failing checks are all non-required or already red on the base branch".into(),
        );
    }
    let Some(v) = verdict else {
        return (false, "no attribution verdict available".into());
    };
    match v.category {
        CiCategory::PrCaused if v.confidence >= threshold => (
            true,
            format!(
                "PR-caused failure on a required check ({:.0}% confidence)",
                v.confidence * 100.0
            ),
        ),
        CiCategory::PrCaused => (
            false,
            format!(
                "PR-caused but low confidence ({:.0}%)",
                v.confidence * 100.0
            ),
        ),
        other => (false, format!("attributed as {other:?}, not auto-fixing")),
    }
}

// ---- LLM attribution (claude -p) -------------------------------------------

/// Ask Claude to attribute a CI failure from the diff + failed logs, returning a structured
/// verdict. One-shot `claude -p` in plan mode (read-only), mirroring `draft::pr_summary`.
pub fn ci_verdict(worktree: &str, diff: &str, failing: &[String], logs: &str) -> Result<CiVerdict> {
    let prompt = format!(
        "You are triaging a failing CI check on a pull request. Decide whether the failure is \
         caused by THIS PR's changes, or is unrelated (infrastructure, a missing secret, the base \
         branch already being broken, a dependency outage) or flaky.\n\n\
         The PR diff and the failed-job logs are provided on stdin (DIFF then FAILED LOGS). \
         Failing checks: {}.\n\n\
         Reply with ONLY a JSON object (no prose, no code fence) of the form:\n\
         {{\"category\": \"pr_caused\" | \"unrelated_infra\" | \"flaky\" | \"undetermined\", \
         \"confidence\": 0.0-1.0, \"rationale\": \"one or two sentences\", \
         \"proposed_fix\": \"what to change (empty unless pr_caused)\"}}.\n\
         Only choose pr_caused when the logs point at code/tests/config the diff actually changed.",
        failing.join(", ")
    );
    let stdin_body = format!(
        "DIFF:\n{}\n\nFAILED LOGS:\n{}\n",
        truncate(diff, MAX_DIFF_BYTES),
        truncate(logs, MAX_LOG_BYTES)
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
        let _ = stdin.write_all(stdin_body.as_bytes());
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
    parse_verdict(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| anyhow::anyhow!("could not parse CI verdict from claude output"))
}

/// Extract a `CiVerdict` from model output: find the first balanced `{…}` JSON object (tolerant of
/// a stray code fence or surrounding prose) and deserialize it.
pub fn parse_verdict(raw: &str) -> Option<CiVerdict> {
    let start = raw.find('{')?;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str::<CiVerdict>(&raw[start..=i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // For logs the tail (the actual error) is most useful; for diffs the head. We keep the tail
    // for logs by caller convention — here keep the head on a char boundary plus a marker.
    let cut = s
        .char_indices()
        .take_while(|(i, _)| *i <= max)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("{}\n…(truncated)…", &s[..cut])
}

// ---- orchestration ----------------------------------------------------------

/// Run the full triage funnel for a ticket's PR worktree and return the result. Best-effort: gh
/// failures degrade gracefully (e.g. unknown required set → gate skipped). The caller decides
/// whether to spawn a fix based on `CiTriage::actionable`.
pub fn triage(worktree: &str, base: &str, diff: &str) -> Result<CiTriage> {
    let head = crate::github::head_sha(worktree).unwrap_or_default();
    let failing = crate::github::pr_checks_json(worktree)
        .ok()
        .map(|j| parse_failing_checks(&j))
        .unwrap_or_default();

    if failing.is_empty() {
        return Ok(CiTriage {
            head_sha: head,
            failing_checks: vec![],
            base_red_checks: vec![],
            required_checks: None,
            verdict: None,
            actionable: false,
            reason: "no failing checks".into(),
        });
    }

    let required = crate::github::required_checks_json(worktree, base)
        .ok()
        .and_then(|j| parse_required_checks(&j));
    let base_red: HashSet<String> = crate::github::rev_parse(worktree, base)
        .ok()
        .and_then(|sha| crate::github::check_runs_json(worktree, &sha).ok())
        .map(|j| parse_base_red_checks(&j))
        .unwrap_or_default();

    // Attribution only when there is a PR-specific candidate worth attributing.
    let candidates = candidate_checks(&failing, required.as_ref(), &base_red);
    let verdict = if candidates.is_empty() {
        None
    } else {
        let logs = collect_failed_logs(worktree, &head);
        ci_verdict(worktree, diff, &failing, &logs).ok()
    };

    let (actionable, reason) = decide(
        &failing,
        required.as_ref(),
        &base_red,
        verdict.as_ref(),
        CONFIDENCE_THRESHOLD,
    );
    Ok(CiTriage {
        head_sha: head,
        failing_checks: failing,
        base_red_checks: base_red.into_iter().collect(),
        required_checks: required.map(|r| r.into_iter().collect()),
        verdict,
        actionable,
        reason,
    })
}

/// Concatenate the `--log-failed` output of every failed run at `head` (best-effort).
fn collect_failed_logs(worktree: &str, head: &str) -> String {
    let ids = crate::github::run_list_json(worktree, "HEAD")
        .ok()
        .map(|j| parse_failed_run_ids(&j, head))
        .unwrap_or_default();
    let mut out = String::new();
    for id in ids {
        if let Ok(l) = crate::github::failed_logs(worktree, id) {
            out.push_str(&l);
            out.push('\n');
        }
        if out.len() > MAX_LOG_BYTES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(cat: CiCategory, conf: f32) -> CiVerdict {
        CiVerdict {
            category: cat,
            confidence: conf,
            rationale: "r".into(),
            proposed_fix: "".into(),
        }
    }
    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn decide_fixes_required_pr_caused_high_confidence() {
        let (act, _) = decide(
            &["build".into()],
            Some(&set(&["build"])),
            &HashSet::new(),
            Some(&verdict(CiCategory::PrCaused, 0.9)),
            CONFIDENCE_THRESHOLD,
        );
        assert!(act);
    }

    #[test]
    fn decide_skips_when_base_already_red() {
        let (act, reason) = decide(
            &["build".into()],
            Some(&set(&["build"])),
            &set(&["build"]),
            Some(&verdict(CiCategory::PrCaused, 0.99)),
            CONFIDENCE_THRESHOLD,
        );
        assert!(!act);
        assert!(reason.contains("base"));
    }

    #[test]
    fn decide_skips_non_required_check() {
        let (act, _) = decide(
            &["experimental".into()],
            Some(&set(&["build", "test"])),
            &HashSet::new(),
            Some(&verdict(CiCategory::PrCaused, 0.99)),
            CONFIDENCE_THRESHOLD,
        );
        assert!(!act);
    }

    #[test]
    fn decide_skips_unrelated_or_flaky() {
        for cat in [
            CiCategory::UnrelatedInfra,
            CiCategory::Flaky,
            CiCategory::Undetermined,
        ] {
            let (act, _) = decide(
                &["build".into()],
                None,
                &HashSet::new(),
                Some(&verdict(cat, 0.99)),
                CONFIDENCE_THRESHOLD,
            );
            assert!(!act, "{cat:?} should not be actionable");
        }
    }

    #[test]
    fn decide_skips_low_confidence() {
        let (act, _) = decide(
            &["build".into()],
            None,
            &HashSet::new(),
            Some(&verdict(CiCategory::PrCaused, 0.4)),
            CONFIDENCE_THRESHOLD,
        );
        assert!(!act);
    }

    #[test]
    fn decide_unknown_required_set_still_works() {
        // required == None → "required only" gate is skipped; a PR-caused failure is actionable.
        let (act, _) = decide(
            &["build".into()],
            None,
            &HashSet::new(),
            Some(&verdict(CiCategory::PrCaused, 0.8)),
            CONFIDENCE_THRESHOLD,
        );
        assert!(act);
    }

    #[test]
    fn decide_no_failing_checks() {
        let (act, _) = decide(&[], None, &HashSet::new(), None, CONFIDENCE_THRESHOLD);
        assert!(!act);
    }

    #[test]
    fn parse_failing_checks_picks_failures() {
        let j = r#"[
            {"name":"build","bucket":"fail","state":"FAILURE"},
            {"name":"lint","bucket":"pass","state":"SUCCESS"},
            {"name":"e2e","bucket":"pending","state":"IN_PROGRESS"}
        ]"#;
        assert_eq!(parse_failing_checks(j), vec!["build".to_string()]);
    }

    #[test]
    fn parse_failed_run_ids_filters_by_sha_and_conclusion() {
        let j = r#"[
            {"databaseId":1,"headSha":"abc","conclusion":"failure"},
            {"databaseId":2,"headSha":"abc","conclusion":"success"},
            {"databaseId":3,"headSha":"def","conclusion":"failure"},
            {"databaseId":4,"headSha":"abc","conclusion":"timed_out"}
        ]"#;
        let mut ids = parse_failed_run_ids(j, "abc");
        ids.sort();
        assert_eq!(ids, vec![1, 4]);
    }

    #[test]
    fn parse_base_red_checks_collects_failed_names() {
        let j = r#"{"check_runs":[
            {"name":"build","conclusion":"failure"},
            {"name":"lint","conclusion":"success"},
            {"name":"flake","conclusion":"timed_out"}
        ]}"#;
        let s = parse_base_red_checks(j);
        assert!(s.contains("build") && s.contains("flake") && !s.contains("lint"));
    }

    #[test]
    fn parse_required_checks_both_shapes() {
        let new_shape =
            r#"{"checks":[{"context":"build","app_id":1},{"context":"test","app_id":2}]}"#;
        assert_eq!(
            parse_required_checks(new_shape).unwrap(),
            set(&["build", "test"])
        );
        let old_shape = r#"{"contexts":["build","lint"]}"#;
        assert_eq!(
            parse_required_checks(old_shape).unwrap(),
            set(&["build", "lint"])
        );
        assert!(parse_required_checks("not json").is_none());
    }

    #[test]
    fn parse_verdict_extracts_json_object() {
        let raw = "Here is my verdict:\n```json\n{\"category\":\"pr_caused\",\"confidence\":0.85,\"rationale\":\"the failing test imports a function this diff renamed\",\"proposed_fix\":\"update the call site\"}\n```";
        let v = parse_verdict(raw).expect("parse");
        assert_eq!(v.category, CiCategory::PrCaused);
        assert!((v.confidence - 0.85).abs() < 1e-6);
        assert!(v.rationale.contains("renamed"));
    }

    #[test]
    fn parse_verdict_handles_braces_in_strings() {
        let raw = r#"{"category":"unrelated_infra","confidence":0.6,"rationale":"log says { token } missing","proposed_fix":""}"#;
        let v = parse_verdict(raw).expect("parse");
        assert_eq!(v.category, CiCategory::UnrelatedInfra);
    }
}
