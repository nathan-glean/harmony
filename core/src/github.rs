//! GitHub PR operations via `gh` (DESIGN Q11): push the worktree's branch, open a draft PR,
//! read PR status/approval, and squash-merge once approved (on the move to Done).

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::process::Command;

fn run(cmd: &str, args: &[&str], cwd: &str) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| crate::cmd_err::spawn_error(cmd, &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify(
            cmd,
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn push_branch(worktree: &str, branch: &str) -> Result<()> {
    run("git", &["push", "-u", "origin", branch], worktree)?;
    Ok(())
}

/// Diff of the worktree (committed + uncommitted) against the merge-base with `base` —
/// i.e. everything this branch changed. Empty string if no changes.
pub fn diff(worktree: &str, base: &str) -> Result<String> {
    run("git", &["diff", "--merge-base", base], worktree)
}

/// Build a PR body: a Claude-generated summary of the branch diff (conforming to the repo's PR
/// template when one exists, and referencing `ticket_ref`), falling back to `fallback` (the
/// composed spec) whenever a summary can't be produced — no diff, or `claude`/`git` unavailable.
/// The single place the "generated summary when available, else spec" rule lives.
pub fn generated_pr_body(
    worktree: &str,
    repo_path: &str,
    ticket_ref: Option<&str>,
    fallback: &str,
) -> String {
    let base = crate::worktree::default_branch(repo_path).unwrap_or_else(|_| "main".into());
    let diff = match diff(worktree, &base) {
        Ok(d) if !d.trim().is_empty() => d,
        _ => return fallback.to_string(),
    };
    match crate::draft::pr_summary(worktree, &diff, ticket_ref) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => fallback.to_string(),
    }
}

/// `gh pr view --json …` for the branch's PR (Err if there's no PR).
pub fn pr_view_json(worktree: &str) -> Result<String> {
    run(
        "gh",
        &["pr", "view", "--json", "number,title,url,state,isDraft"],
        worktree,
    )
}

/// The branch PR's current description body (`gh pr view --json body`). None when there's no PR.
pub fn pr_body(worktree: &str) -> Option<String> {
    let json = run("gh", &["pr", "view", "--json", "body"], worktree).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    v.get("body")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

/// Replace the branch PR's description (`gh pr edit --body`).
pub fn update_pr_body(worktree: &str, body: &str) -> Result<()> {
    run("gh", &["pr", "edit", "--body", body], worktree)?;
    Ok(())
}

/// `gh pr checks --json …`. NOTE: `gh pr checks` exits non-zero when checks are failing or
/// pending, but still prints the JSON — so we read stdout regardless of exit code.
pub fn pr_checks_json(worktree: &str) -> Result<String> {
    let out = Command::new("gh")
        .args(["pr", "checks", "--json", "name,state,link,bucket"])
        .current_dir(worktree)
        .output()
        .map_err(|e| crate::cmd_err::spawn_error("gh", &e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() {
        return Err(crate::cmd_err::classify(
            "gh",
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(stdout)
}

/// The branch's HEAD commit SHA — used to fingerprint "the current change-set" so `/review`
/// isn't re-run when nothing has changed since the last review.
pub fn head_sha(worktree: &str) -> Result<String> {
    Ok(run("git", &["rev-parse", "HEAD"], worktree)?
        .trim()
        .to_string())
}

/// SHA a ref resolves to. Tries the remote-tracking `origin/<ref>` first (so a bare base-branch
/// name resolves to the commit CI actually ran against), then the bare ref.
pub fn rev_parse(worktree: &str, refname: &str) -> Result<String> {
    if let Ok(s) = run(
        "git",
        &["rev-parse", &format!("origin/{refname}")],
        worktree,
    ) {
        return Ok(s.trim().to_string());
    }
    Ok(run("git", &["rev-parse", refname], worktree)?
        .trim()
        .to_string())
}

/// Workflow runs for `branch` (`gh run list --json …`) — used to map a failing PR check to the
/// run id whose logs we need, filtered by HEAD sha downstream. Raw JSON array string.
pub fn run_list_json(worktree: &str, branch: &str) -> Result<String> {
    run(
        "gh",
        &[
            "run",
            "list",
            "--branch",
            branch,
            "--limit",
            "40",
            "--json",
            "databaseId,headSha,conclusion,status,name,workflowName",
        ],
        worktree,
    )
}

/// The failed-step logs for a workflow run (`gh run view <id> --log-failed`). Can be large — the
/// caller truncates before feeding it to the model. `gh` exits non-zero on some run states but
/// still prints logs, so read stdout regardless of exit code (like `pr_checks_json`).
pub fn failed_logs(worktree: &str, run_id: i64) -> Result<String> {
    let id = run_id.to_string();
    let out = Command::new("gh")
        .args(["run", "view", &id, "--log-failed"])
        .current_dir(worktree)
        .output()
        .map_err(|e| crate::cmd_err::spawn_error("gh", &e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() && !out.status.success() {
        return Err(crate::cmd_err::classify(
            "gh",
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(stdout)
}

/// Check-run results for a commit (`gh api repos/{owner}/{repo}/commits/<sha>/check-runs`). Used to
/// test whether a check is *already red on the base branch* (i.e. not caused by this PR). `gh`
/// substitutes `{owner}/{repo}` from the repo in `worktree`. Raw JSON object string.
pub fn check_runs_json(worktree: &str, sha: &str) -> Result<String> {
    run(
        "gh",
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/commits/{sha}/check-runs"),
            "--paginate",
        ],
        worktree,
    )
}

/// Branch-protection required status checks (`gh api …/branches/<base>/protection/required_status_checks`).
/// 404s (no protection) / permission errors surface as `Err`; the caller treats that as "unknown".
pub fn required_checks_json(worktree: &str, base: &str) -> Result<String> {
    run(
        "gh",
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/branches/{base}/protection/required_status_checks"),
        ],
        worktree,
    )
}

/// Stage everything and commit it (`git add -A` + `git commit`), if there's anything to commit.
/// Returns whether a commit was actually made (false when the worktree is clean). harmony owns
/// committing the agent's work — deterministic, and the squash-merge makes granularity moot.
pub fn commit_all(worktree: &str, message: &str) -> Result<bool> {
    run("git", &["add", "-A"], worktree)?;
    // `diff --cached --quiet` exits 1 when there ARE staged changes.
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(worktree)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    if !staged {
        return Ok(false);
    }
    run("git", &["commit", "-q", "-m", message], worktree)?;
    Ok(true)
}

/// Number of commits the branch is ahead of `base` (`git rev-list --count <base>..HEAD`).
/// Used to refuse opening an empty PR with a clear message instead of a cryptic `gh` failure.
pub fn commits_ahead(worktree: &str, base: &str) -> usize {
    run(
        "git",
        &["rev-list", "--count", &format!("{base}..HEAD")],
        worktree,
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

/// PR status for the worktree's branch. `exists` is false when there is no PR (or `gh` can't
/// answer) — that's a normal state, not an error, so this never returns `Err`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrStatus {
    pub exists: bool,
    /// GitHub `reviewDecision == "APPROVED"`.
    pub approved: bool,
    /// `OPEN` | `MERGED` | `CLOSED` (empty when no PR).
    pub state: String,
    pub is_draft: bool,
    pub url: String,
}

/// Read the branch's PR status + approval via `gh pr view`. No PR / `gh` unavailable → a
/// default (`exists: false`) status.
pub fn pr_status(worktree: &str) -> PrStatus {
    match run(
        "gh",
        &[
            "pr",
            "view",
            "--json",
            "number,state,isDraft,url,reviewDecision",
        ],
        worktree,
    ) {
        Ok(json) => parse_pr_status(&json),
        Err(_) => PrStatus::default(),
    }
}

/// Parse `gh pr view --json …` output into a `PrStatus` (split out for testing).
fn parse_pr_status(json: &str) -> PrStatus {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let exists = v.get("number").map(|n| !n.is_null()).unwrap_or(false);
    PrStatus {
        exists,
        approved: v.get("reviewDecision").and_then(|x| x.as_str()) == Some("APPROVED"),
        state: v
            .get("state")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        is_draft: v.get("isDraft").and_then(|x| x.as_bool()).unwrap_or(false),
        url: v
            .get("url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// A GitHub PR comment normalized for display: the conversation thread, review summaries
/// (approve/request-changes), and inline diff comments, unified into one shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrComment {
    pub author: String,
    pub body: String,
    /// ISO8601 timestamp from the API (created_at / submitted_at). Used to sort + display.
    pub created_at: String,
    /// "conversation" | "review" | "inline".
    pub kind: String,
    /// Review state (APPROVED / CHANGES_REQUESTED / COMMENTED) — empty for non-review comments.
    pub state: String,
    /// Inline comment file path — empty otherwise.
    pub path: String,
    /// Inline comment line — 0 otherwise.
    pub line: i64,
    /// `html_url` to the comment on GitHub.
    pub url: String,
    /// For inline comments: the unified `diff_hunk` the comment anchors to (the line changes it's
    /// about). Empty otherwise. The proposed edit (`suggestion`) lives in `body`.
    pub diff_hunk: String,
    /// For a multi-line inline comment: the first line of the commented range (`line` is the last).
    /// 0 for single-line / non-inline — used to size how many lines a `suggestion` replaces.
    pub start_line: i64,
    /// "high" | "medium" | "low" | "" — priority/severity parsed from the comment body (e.g. a
    /// Copilot review's stated severity). Empty when none is detected.
    pub priority: String,
}

/// Best-effort priority/severity from a comment body. Looks for a level near a "priority"/"severity"
/// label or a coloured-circle emoji — both strong, low-false-positive signals (a bare word like
/// "low-level" elsewhere in the prose won't trip it). Returns "high" | "medium" | "low" | "".
pub fn parse_priority(body: &str) -> String {
    let b = body.to_lowercase();
    for key in ["priority", "severity"] {
        if let Some(i) = b.find(key) {
            let w = &b[i..(i + 48).min(b.len())];
            if ["high", "critical", "blocker", "major"]
                .iter()
                .any(|k| w.contains(k))
            {
                return "high".into();
            }
            if ["medium", "moderate", "warning"]
                .iter()
                .any(|k| w.contains(k))
            {
                return "medium".into();
            }
            if ["low", "minor", "trivial", "nit"]
                .iter()
                .any(|k| w.contains(k))
            {
                return "low".into();
            }
        }
    }
    if body.contains('🔴') {
        return "high".into();
    }
    if body.contains('🟠') || body.contains('🟡') {
        return "medium".into();
    }
    if body.contains('🟢') {
        return "low".into();
    }
    String::new()
}

/// The branch's PR number, parsed from `pr_view_json` (None when there's no PR).
pub fn pr_number(worktree: &str) -> Option<i64> {
    let json = pr_view_json(worktree).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    v.get("number").and_then(|n| n.as_i64())
}

/// `gh api <path> --paginate` (gh substitutes `{owner}/{repo}` from the repo in `worktree`).
fn gh_api(worktree: &str, path: &str) -> Result<String> {
    run("gh", &["api", path, "--paginate"], worktree)
}

/// All PR comments for the branch's PR — conversation thread, review summaries, and inline diff
/// comments — merged and sorted oldest→newest. Best-effort: no PR (or any `gh` failure) → empty.
pub fn pr_comments(worktree: &str) -> Vec<PrComment> {
    let Some(n) = pr_number(worktree) else {
        return vec![];
    };
    let mut out = Vec::new();
    if let Ok(j) = gh_api(
        worktree,
        &format!("repos/{{owner}}/{{repo}}/issues/{n}/comments"),
    ) {
        out.extend(parse_issue_comments(&j));
    }
    if let Ok(j) = gh_api(
        worktree,
        &format!("repos/{{owner}}/{{repo}}/pulls/{n}/reviews"),
    ) {
        out.extend(parse_reviews(&j));
    }
    if let Ok(j) = gh_api(
        worktree,
        &format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments"),
    ) {
        out.extend(parse_review_comments(&j));
    }
    let mut out = dedup_comments(out);
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

/// Drop duplicate comments, keeping first occurrence. `gh api --paginate` can repeat items, which
/// would otherwise show the same comment twice in the UI. Identity is the GitHub `html_url` (unique
/// per comment/review); when a url is missing, fall back to author+kind+body+timestamp.
fn dedup_comments(comments: Vec<PrComment>) -> Vec<PrComment> {
    let mut seen = std::collections::HashSet::new();
    comments
        .into_iter()
        .filter(|c| {
            let key = if c.url.is_empty() {
                format!(
                    "{}\u{1}{}\u{1}{}\u{1}{}",
                    c.author, c.kind, c.created_at, c.body
                )
            } else {
                c.url.clone()
            };
            seen.insert(key)
        })
        .collect()
}

fn as_array(json: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}
fn str_at(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}
fn login(v: &serde_json::Value) -> String {
    v.get("user")
        .and_then(|u| u.get("login"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// Conversation thread comments (`…/issues/{n}/comments`).
pub fn parse_issue_comments(json: &str) -> Vec<PrComment> {
    as_array(json)
        .iter()
        .map(|c| {
            let body = str_at(c, "body");
            PrComment {
                author: login(c),
                priority: parse_priority(&body),
                body,
                created_at: str_at(c, "created_at"),
                kind: "conversation".into(),
                state: String::new(),
                path: String::new(),
                line: 0,
                url: str_at(c, "html_url"),
                diff_hunk: String::new(),
                start_line: 0,
            }
        })
        .collect()
}

/// Review summaries (`…/pulls/{n}/reviews`). Keep reviews with a non-empty body or a meaningful
/// state (APPROVED / CHANGES_REQUESTED); drop empty COMMENTED/PENDING noise.
pub fn parse_reviews(json: &str) -> Vec<PrComment> {
    as_array(json)
        .iter()
        .filter_map(|r| {
            let body = str_at(r, "body");
            let state = str_at(r, "state");
            let keep = !body.trim().is_empty()
                || matches!(state.as_str(), "APPROVED" | "CHANGES_REQUESTED");
            if !keep {
                return None;
            }
            Some(PrComment {
                author: login(r),
                priority: parse_priority(&body),
                body,
                created_at: str_at(r, "submitted_at"),
                kind: "review".into(),
                state,
                path: String::new(),
                line: 0,
                url: str_at(r, "html_url"),
                diff_hunk: String::new(),
                start_line: 0,
            })
        })
        .collect()
}

/// Inline diff comments (`…/pulls/{n}/comments`). `line` falls back to `original_line` when the
/// comment's line is no longer present in the current diff.
pub fn parse_review_comments(json: &str) -> Vec<PrComment> {
    as_array(json)
        .iter()
        .map(|c| {
            let line = c
                .get("line")
                .and_then(|x| x.as_i64())
                .or_else(|| c.get("original_line").and_then(|x| x.as_i64()))
                .unwrap_or(0);
            let start_line = c
                .get("start_line")
                .and_then(|x| x.as_i64())
                .or_else(|| c.get("original_start_line").and_then(|x| x.as_i64()))
                .unwrap_or(0);
            let body = str_at(c, "body");
            PrComment {
                author: login(c),
                priority: parse_priority(&body),
                body,
                created_at: str_at(c, "created_at"),
                kind: "inline".into(),
                state: String::new(),
                path: str_at(c, "path"),
                line,
                url: str_at(c, "html_url"),
                diff_hunk: str_at(c, "diff_hunk"),
                start_line,
            }
        })
        .collect()
}

/// Squash-merge the branch's PR and delete the remote branch (on the move to Done, once the PR
/// is approved on GitHub). harmony only merges here — never mid-flow.
pub fn merge_pr(worktree: &str) -> Result<()> {
    run(
        "gh",
        &["pr", "merge", "--squash", "--delete-branch"],
        worktree,
    )?;
    Ok(())
}

/// Open a draft PR; returns the PR URL (`gh` prints it to stdout).
pub fn create_draft_pr(worktree: &str, title: &str, body: &str, branch: &str) -> Result<String> {
    let out = run(
        "gh",
        &[
            "pr", "create", "--draft", "--title", title, "--body", body, "--head", branch,
        ],
        worktree,
    )?;
    // `gh pr create` succeeded; the PR URL is the last http line it printed. If we can't find
    // one, don't hand the raw output back as a "URL" — surface it as an error instead.
    out.lines()
        .rev()
        .find(|l| l.trim_start().starts_with("http"))
        .map(|l| l.trim().to_string())
        .ok_or_else(|| anyhow!("gh pr create did not return a PR URL: {}", out.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_status_approved() {
        let s = parse_pr_status(
            r#"{"number":12,"state":"OPEN","isDraft":false,"url":"https://x/pr/12","reviewDecision":"APPROVED"}"#,
        );
        assert!(s.exists && s.approved && !s.is_draft);
        assert_eq!(s.state, "OPEN");
        assert_eq!(s.url, "https://x/pr/12");
    }

    #[test]
    fn parse_pr_status_unreviewed_draft() {
        let s = parse_pr_status(
            r#"{"number":3,"state":"OPEN","isDraft":true,"url":"u","reviewDecision":"REVIEW_REQUIRED"}"#,
        );
        assert!(s.exists && !s.approved && s.is_draft);
    }

    #[test]
    fn parse_pr_status_no_pr() {
        // `gh` prints an empty object / null fields when there's no PR.
        assert_eq!(parse_pr_status("{}"), PrStatus::default());
        assert_eq!(parse_pr_status(""), PrStatus::default());
    }

    #[test]
    fn parse_issue_comments_maps_fields() {
        let j = r#"[
            {"user":{"login":"alice"},"body":"looks good","created_at":"2026-01-01T00:00:00Z","html_url":"https://x/c/1"}
        ]"#;
        let c = parse_issue_comments(j);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].author, "alice");
        assert_eq!(c[0].kind, "conversation");
        assert_eq!(c[0].body, "looks good");
        assert_eq!(c[0].url, "https://x/c/1");
    }

    #[test]
    fn parse_reviews_drops_empty_commented_keeps_state_or_body() {
        let j = r#"[
            {"user":{"login":"bob"},"body":"","state":"COMMENTED","submitted_at":"t1","html_url":"u1"},
            {"user":{"login":"bob"},"body":"","state":"APPROVED","submitted_at":"t2","html_url":"u2"},
            {"user":{"login":"cara"},"body":"please fix","state":"CHANGES_REQUESTED","submitted_at":"t3","html_url":"u3"},
            {"user":{"login":"dan"},"body":"nit only","state":"COMMENTED","submitted_at":"t4","html_url":"u4"}
        ]"#;
        let r = parse_reviews(j);
        // The empty COMMENTED review is dropped; the other three are kept.
        let states: Vec<&str> = r.iter().map(|c| c.state.as_str()).collect();
        assert_eq!(states, vec!["APPROVED", "CHANGES_REQUESTED", "COMMENTED"]);
        assert!(r.iter().all(|c| c.kind == "review"));
        assert_eq!(r[1].body, "please fix");
    }

    #[test]
    fn parse_review_comments_uses_line_with_original_fallback() {
        let j = r#"[
            {"user":{"login":"eve"},"body":"```suggestion\nlet x = 1;\n```","created_at":"t","html_url":"u","path":"src/x.rs","line":42,"diff_hunk":"@@ -40,3 +40,3 @@\n-let x = 0;\n+let x = 1;"},
            {"user":{"login":"eve"},"body":"stale","created_at":"t","html_url":"u","path":"src/y.rs","line":null,"original_line":7}
        ]"#;
        let c = parse_review_comments(j);
        assert_eq!(c[0].path, "src/x.rs");
        assert_eq!(c[0].line, 42);
        assert_eq!(c[0].kind, "inline");
        assert!(c[0].diff_hunk.contains("+let x = 1;"));
        assert!(c[0].body.contains("```suggestion"));
        assert_eq!(c[1].line, 7); // fell back to original_line
    }

    #[test]
    fn parse_comments_handle_empty_and_garbage() {
        assert!(parse_issue_comments("[]").is_empty());
        assert!(parse_reviews("not json").is_empty());
        assert!(parse_review_comments("{}").is_empty());
    }

    #[test]
    fn parse_priority_detects_labelled_and_emoji() {
        assert_eq!(parse_priority("Priority: High — fix this"), "high");
        assert_eq!(parse_priority("severity: medium"), "medium");
        assert_eq!(parse_priority("**Priority:** Low (nit)"), "low");
        assert_eq!(parse_priority("🔴 This will crash"), "high");
        assert_eq!(parse_priority("🟢 minor style note"), "low");
        // No label → no false positive from incidental words.
        assert_eq!(parse_priority("this is a low-level helper"), "");
        assert_eq!(parse_priority("looks good to me"), "");
    }

    #[test]
    fn dedup_comments_removes_repeats_by_url() {
        let mk = |url: &str, body: &str| PrComment {
            author: "a".into(),
            body: body.into(),
            created_at: "t".into(),
            kind: "conversation".into(),
            state: String::new(),
            path: String::new(),
            line: 0,
            url: url.into(),
            diff_hunk: String::new(),
            priority: String::new(),
            start_line: 0,
        };
        let out = dedup_comments(vec![
            mk("https://x/c/1", "one"),
            mk("https://x/c/2", "two"),
            mk("https://x/c/1", "one"), // duplicate (e.g. from --paginate)
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://x/c/1");
        assert_eq!(out[1].url, "https://x/c/2");
    }

    #[test]
    fn dedup_comments_keeps_distinct_urlless_by_content() {
        let mk = |body: &str| PrComment {
            author: "a".into(),
            body: body.into(),
            created_at: "t".into(),
            kind: "review".into(),
            state: String::new(),
            path: String::new(),
            line: 0,
            url: String::new(),
            diff_hunk: String::new(),
            priority: String::new(),
            start_line: 0,
        };
        // Same author/kind/time but different bodies → distinct; identical → collapsed.
        let out = dedup_comments(vec![mk("x"), mk("y"), mk("x")]);
        assert_eq!(out.len(), 2);
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn commit_all_commits_when_dirty_and_noops_when_clean() {
        let dir = std::env::temp_dir().join(format!("harmony-gh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_string_lossy().to_string();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@h.local"]);
        git(&dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);

        assert_eq!(commits_ahead(&p, "main"), 0);
        // Clean tree → no-op.
        assert!(!commit_all(&p, "noop").unwrap());

        // Dirty → commits, HEAD advances, but still 0 ahead of main (we committed onto main here).
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        assert!(commit_all(&p, "feat: add").unwrap());
        assert!(!commit_all(&p, "again").unwrap()); // now clean

        let _ = std::fs::remove_dir_all(&dir);
    }
}
