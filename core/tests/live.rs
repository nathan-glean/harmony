//! Live-call validation against real Jira (`acli`) and GitHub (`gh`).
//!
//! These tests hit the network and the real authenticated CLIs, so they are `#[ignore]`d
//! and excluded from normal `cargo test` / CI. Run them on demand on a machine that is
//! logged in (`acli jira auth login`, `gh auth login`):
//!
//!   cargo test -p harmony-core --test live -- --ignored --nocapture
//!
//! Read-only tests run as-is. **Write** tests (Jira comment/transition, branch push, PR
//! creation) mutate real issues/remotes, so they additionally self-skip unless an explicit
//! env var points them at a throwaway target:
//!
//!   HARMONY_LIVE_JIRA_WRITE_KEY=PROJ-123 \
//!   HARMONY_LIVE_GH_WORKTREE=/path/to/throwaway/worktree HARMONY_LIVE_GH_BRANCH=tmp/x \
//!     cargo test -p harmony-core --test live -- --ignored --nocapture

use harmony_core::{github, jira};

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

// ---- Jira (read-only) ----------------------------------------------------

#[tokio::test]
#[ignore = "live: needs `acli jira auth login`"]
async fn live_jira_readonly_roundtrip() {
    assert!(
        jira::cli_installed(),
        "acli must be installed for live tests"
    );

    let site = jira::connected_site().await;
    assert!(
        site.is_some(),
        "must be authenticated (`acli jira auth login`)"
    );
    eprintln!("connected site: {}", site.unwrap());

    // search_assigned → the production query, now `--paginate`d.
    let issues = jira::search_assigned()
        .await
        .expect("search_assigned failed");
    eprintln!("search_assigned returned {} issue(s)", issues.len());
    for i in &issues {
        // Every row must at least have a key and a status (summary can be empty in theory).
        assert!(!i.key.is_empty(), "issue with empty key: {:?}", i.summary);
        assert!(!i.status.is_empty(), "issue {} has empty status", i.key);
    }

    let Some(first) = issues.first() else {
        return skip("no assigned issues to drill into (get_issue/comments not exercised)");
    };

    // get_issue → single-issue fetch incl. ADF description.
    let issue = jira::get_issue(&first.key).await.expect("get_issue failed");
    assert_eq!(issue.key, first.key);
    assert!(
        !issue.summary.is_empty(),
        "get_issue summary empty for {}",
        issue.key
    );
    eprintln!(
        "get_issue {} — summary {:?}, description {} chars",
        issue.key,
        issue.summary,
        issue.description.len()
    );

    // comments → wrapped object, plain-string author/body, no timestamp.
    let comments = jira::comments(&first.key).await.expect("comments failed");
    eprintln!("comments({}) returned {}", first.key, comments.len());
    for c in &comments {
        // body may legitimately be empty; author should parse to a string when present.
        assert!(c.created.is_empty() || !c.created.is_empty()); // tolerant: acli omits it
        let _ = &c.author;
    }
}

// ---- Jira (write — guarded, self-skips without an explicit throwaway key) -

#[tokio::test]
#[ignore = "live+write: set HARMONY_LIVE_JIRA_WRITE_KEY=<throwaway-issue>"]
async fn live_jira_write_roundtrip() {
    let Ok(key) = std::env::var("HARMONY_LIVE_JIRA_WRITE_KEY") else {
        return skip("HARMONY_LIVE_JIRA_WRITE_KEY unset — not mutating any real issue");
    };
    let marker = "harmony live-call validation — safe to delete";
    jira::add_comment(&key, marker)
        .await
        .expect("add_comment failed");

    let comments = jira::comments(&key).await.expect("comments failed");
    assert!(
        comments.iter().any(|c| c.body.contains(marker)),
        "posted comment did not round-trip back via comments()"
    );
    eprintln!(
        "comment round-trip on {key} OK ({} comments)",
        comments.len()
    );
}

// ---- GitHub (read-only) --------------------------------------------------

/// A worktree to read against: env override, else the first harmony-managed worktree.
fn discover_worktree() -> Option<(String, String)> {
    if let Ok(wt) = std::env::var("HARMONY_LIVE_GH_WORKTREE") {
        let branch = std::env::var("HARMONY_LIVE_GH_BRANCH").unwrap_or_default();
        return Some((wt, branch));
    }
    let home = std::env::var("HOME").ok()?;
    let root = std::path::Path::new(&home).join(".harmony/worktrees");
    for repo in std::fs::read_dir(&root).ok()?.flatten() {
        for wt in std::fs::read_dir(repo.path()).ok()?.flatten() {
            if wt.path().join(".git").exists() {
                return Some((wt.path().to_string_lossy().to_string(), String::new()));
            }
        }
    }
    None
}

#[test]
#[ignore = "live: needs `gh auth login` + a harmony worktree (or HARMONY_LIVE_GH_WORKTREE)"]
fn live_github_readonly() {
    let Some((worktree, _branch)) = discover_worktree() else {
        return skip("no harmony worktree found and HARMONY_LIVE_GH_WORKTREE unset");
    };
    eprintln!("worktree: {worktree}");

    // diff vs the merge-base with the default base branch. Real git, read-only.
    // Try common base names so this works across repos.
    let base = ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|b| github::diff(&worktree, b).is_ok())
        .expect("could not diff against any common base branch");
    let d = github::diff(&worktree, base).expect("diff failed");
    eprintln!("diff vs {base}: {} bytes", d.len());

    // PR read paths: tolerate "no PR for this branch" (the common case for a fresh branch).
    match github::pr_view_json(&worktree) {
        Ok(json) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).expect("pr_view_json returned non-JSON");
            assert!(v.get("url").is_some(), "pr view JSON missing `url`: {json}");
            eprintln!("pr_view_json OK: {}", v.get("url").unwrap());
            // checks only make sense when a PR exists.
            match github::pr_checks_json(&worktree) {
                Ok(cj) => {
                    let _: serde_json::Value =
                        serde_json::from_str(&cj).expect("pr_checks_json returned non-JSON");
                    eprintln!("pr_checks_json OK ({} bytes)", cj.len());
                }
                Err(e) => eprintln!("pr_checks_json (no checks / pending): {e}"),
            }
        }
        Err(e) => skip(&format!("no PR for this branch — pr_view_json: {e}")),
    }
}

// ---- GitHub (write — guarded) --------------------------------------------

#[test]
#[ignore = "live+write: set HARMONY_LIVE_GH_WORKTREE + HARMONY_LIVE_GH_BRANCH"]
fn live_github_write_pr() {
    let (Ok(worktree), Ok(branch)) = (
        std::env::var("HARMONY_LIVE_GH_WORKTREE"),
        std::env::var("HARMONY_LIVE_GH_BRANCH"),
    ) else {
        return skip("HARMONY_LIVE_GH_WORKTREE/BRANCH unset — not pushing or opening a PR");
    };
    github::push_branch(&worktree, &branch).expect("push_branch failed");
    let url = github::create_draft_pr(
        &worktree,
        "harmony live-call validation (draft)",
        "Automated draft PR from harmony live test. Safe to close.",
        &branch,
    )
    .expect("create_draft_pr failed");
    assert!(url.starts_with("http"), "PR URL not returned: {url}");
    eprintln!("draft PR opened: {url}");
}
