//! Core test suite — covers the load-bearing logic that has no UI to catch regressions:
//!   1. Store CRUD (repos / tickets / worktrees / sessions).
//!   2. Worktree create + reuse, branch naming, and slugify.
//!   3. The cwd → worktree → session correlation in the hook server.
//!
//! Run with `cargo test -p harmony-core`. Each test gets its own temp SQLite file and,
//! where git is needed, its own throwaway repo, so they're independent and parallel-safe.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use harmony_core::hooks;
use harmony_core::status;
use harmony_core::store::Store;
use harmony_core::worktree;

// ---- temp scaffolding ----------------------------------------------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique temp directory for this process+test. Best-effort cleanup on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("harmony-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        // Canonicalize so it matches what the store/hooks produce (macOS /tmp -> /private/tmp).
        TempDir(std::fs::canonicalize(&p).unwrap())
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn str(&self) -> String {
        self.0.to_string_lossy().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn open_store(dir: &TempDir) -> Store {
    let db = dir.path().join("harmony.db");
    Store::open(db.to_str().unwrap()).await.unwrap()
}

// =====================================================================
// 1. Store CRUD
// =====================================================================

#[tokio::test]
async fn repo_crud_and_canonicalization() {
    let dir = TempDir::new("repo");
    let store = open_store(&dir).await;

    // add_repo canonicalizes the path it stores.
    let repo_dir = dir.path().join("myrepo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let id = store
        .add_repo("myrepo", repo_dir.to_str().unwrap(), Some("PROJ"))
        .await
        .unwrap();

    let repo = store.get_repo(id).await.unwrap().expect("repo exists");
    assert_eq!(repo.name, "myrepo");
    assert_eq!(repo.path, std::fs::canonicalize(&repo_dir).unwrap().to_string_lossy());
    assert_eq!(repo.default_project_key.as_deref(), Some("PROJ"));

    // lookups
    assert_eq!(store.get_repo_by_name("myrepo").await.unwrap().unwrap().id, id);
    assert_eq!(store.default_repo_for_key("PROJ").await.unwrap().unwrap().id, id);
    assert!(store.get_repo_by_name("nope").await.unwrap().is_none());

    // rename
    store.rename_repo(id, "renamed").await.unwrap();
    assert_eq!(store.get_repo(id).await.unwrap().unwrap().name, "renamed");
    assert_eq!(store.list_repos().await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_repo_refuses_with_worktrees_then_clears_ticket_binding() {
    let dir = TempDir::new("repodel");
    let store = open_store(&dir).await;

    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "harmony/x", "/tmp/x", false).await.unwrap();

    // Refuses while a worktree still references the repo.
    assert!(store.delete_repo(repo_id).await.is_err());

    // After the worktree is gone, delete succeeds and the ticket's repo binding is cleared.
    store.delete_worktree(wt_id).await.unwrap();
    store.delete_repo(repo_id).await.unwrap();
    assert!(store.get_repo(repo_id).await.unwrap().is_none());
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().repo_id, None);
}

#[tokio::test]
async fn ticket_crud_and_flags() {
    let dir = TempDir::new("ticket");
    let store = open_store(&dir).await;

    let id = store.add_ticket(None, "local", "My Ticket", "spec body", None).await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    // New tickets land in Todo (DESIGN Q14).
    assert_eq!(t.status, status::TODO);
    assert_eq!(t.title, "My Ticket");
    assert_eq!(t.spec, "spec body");
    assert_eq!((t.planned, t.drafting, t.grilled), (0, 0, 0));

    store.set_ticket_status(id, status::WORKING).await.unwrap();
    store.set_ticket_spec(id, "new spec").await.unwrap();
    store.set_ticket_todos(id, r#"[{"content":"a","status":"pending"}]"#).await.unwrap();
    store.set_ticket_question(id, r#"{"session_id":1}"#).await.unwrap();
    store.mark_ticket_planned(id).await.unwrap();
    store.set_ticket_drafting(id, true).await.unwrap();
    store.mark_ticket_grilled(id).await.unwrap();

    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.status, status::WORKING);
    assert_eq!(t.spec, "new spec");
    assert!(t.todos.contains("pending"));
    assert!(t.pending_question.contains("session_id"));
    assert_eq!((t.planned, t.drafting, t.grilled), (1, 1, 1));

    store.clear_ticket_question(id).await.unwrap();
    assert_eq!(store.get_ticket(id).await.unwrap().unwrap().pending_question, "");

    // repo (re)binding
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    store.set_ticket_repo(id, repo_id).await.unwrap();
    assert_eq!(store.get_ticket(id).await.unwrap().unwrap().repo_id, Some(repo_id));

    assert_eq!(store.list_tickets().await.unwrap().len(), 1);
}

#[tokio::test]
async fn review_loop_fields_roundtrip() {
    let dir = TempDir::new("revfields");
    let store = open_store(&dir).await;
    let id = store.add_ticket(None, "local", "T", "", None).await.unwrap();

    // Defaults: never judged.
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!((t.review_verdict.as_str(), t.review_findings.as_str(), t.judged_sha.as_str()), ("", "", ""));
    assert_eq!(t.review_fix_attempts, 0);

    // Record a verdict + findings + fingerprint and bump the attempt counter.
    store
        .set_ticket_review_verdict(id, "abc123", "changes_requested", r#"["fix x","add test"]"#)
        .await
        .unwrap();
    store.bump_review_fix_attempts(id).await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.review_verdict, "changes_requested");
    assert_eq!(t.judged_sha, "abc123");
    assert!(t.review_findings.contains("add test"));
    assert_eq!(t.review_fix_attempts, 1);

    // Reset (fresh work cycle) clears verdict, findings, fingerprint, and attempts.
    store.reset_review_loop(id).await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!((t.review_verdict.as_str(), t.review_findings.as_str(), t.judged_sha.as_str()), ("", "", ""));
    assert_eq!(t.review_fix_attempts, 0);
}

#[tokio::test]
async fn activity_field_and_session_kind_roundtrip() {
    let dir = TempDir::new("activity");
    let store = open_store(&dir).await;
    let id = store.add_ticket(None, "local", "T", "", None).await.unwrap();

    // Default empty; round-trips an Activity JSON blob.
    assert_eq!(store.get_ticket(id).await.unwrap().unwrap().activity, "");
    store
        .set_ticket_activity(id, r#"{"category":"working","label":"Implementing…","detail":null}"#)
        .await
        .unwrap();
    assert!(store.get_ticket(id).await.unwrap().unwrap().activity.contains("Implementing"));

    // active_session_kind_for_ticket: None with no live session, the kind once one exists.
    assert_eq!(store.active_session_kind_for_ticket(id).await.unwrap(), None);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let wt = store.add_worktree(id, repo_id, "b", "/tmp/x", false).await.unwrap();
    store.add_session(id, wt, "/tmp/x", "review").await.unwrap();
    assert_eq!(
        store.active_session_kind_for_ticket(id).await.unwrap().as_deref(),
        Some("review")
    );
}

#[tokio::test]
async fn spec_fields_persist_independently_and_compose() {
    let dir = TempDir::new("specfields");
    let store = open_store(&dir).await;
    let id = store.add_ticket(None, "local", "T", "body text", None).await.unwrap();

    store
        .set_ticket_spec_fields(id, "Goal body.", "- must pass", "src/a.rs\nsrc/b.rs", "no new deps")
        .await
        .unwrap();

    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.spec, "Goal body.");
    assert_eq!(t.acceptance_criteria, "- must pass");
    assert_eq!(t.relevant_paths, "src/a.rs\nsrc/b.rs");
    assert_eq!(t.constraints, "no new deps");

    // compose_spec rebuilds the canonical markdown with the section headings.
    let composed = harmony_core::spec::compose_spec(&t);
    assert!(composed.contains("Goal body."));
    assert!(composed.contains("## Acceptance criteria\n- must pass"));
    assert!(composed.contains("## Relevant paths\nsrc/a.rs"));
    assert!(composed.contains("## Constraints\nno new deps"));

    // A field can be cleared independently without touching the others.
    store.set_ticket_spec_fields(id, "Goal body.", "", "src/a.rs\nsrc/b.rs", "no new deps").await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.acceptance_criteria, "");
    assert_eq!(t.relevant_paths, "src/a.rs\nsrc/b.rs");
}

#[tokio::test]
async fn upsert_jira_ticket_inserts_then_updates_title_only() {
    let dir = TempDir::new("jira");
    let store = open_store(&dir).await;

    let (id, inserted) = store.upsert_jira_ticket("DNA-1", "Original Title").await.unwrap();
    assert!(inserted);
    // Author a local spec + advance the board column.
    store.set_ticket_spec(id, "local spec").await.unwrap();
    store.set_ticket_status(id, status::WORKING).await.unwrap();

    // Re-sync: title refreshes, but the locally-authored spec and status are preserved.
    let (id2, inserted2) = store.upsert_jira_ticket("DNA-1", "Renamed Upstream").await.unwrap();
    assert_eq!(id2, id);
    assert!(!inserted2);
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.title, "Renamed Upstream");
    assert_eq!(t.spec, "local spec");
    assert_eq!(t.status, status::WORKING);

    assert_eq!(store.get_ticket_by_key("DNA-1").await.unwrap().unwrap().id, id);
}

#[tokio::test]
async fn worktree_crud_primary_and_alternate() {
    let dir = TempDir::new("wt");
    let store = open_store(&dir).await;

    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(Some("K-1"), "jira", "Title", "", Some(repo_id)).await.unwrap();

    let primary = store.add_worktree(ticket_id, repo_id, "harmony/K-1", "/tmp/p", false).await.unwrap();
    let _alt = store.add_worktree(ticket_id, repo_id, "harmony/K-1-alt", "/tmp/a", true).await.unwrap();

    // primary_worktree_for_ticket returns the non-alternate (reuse target).
    let p = store.primary_worktree_for_ticket(ticket_id).await.unwrap().unwrap();
    assert_eq!(p.id, primary);
    assert_eq!(p.is_alternate, 0);

    assert_eq!(store.worktrees_for_ticket(ticket_id).await.unwrap().len(), 2);

    // list_worktrees join carries ticket + repo info.
    let views = store.list_worktrees().await.unwrap();
    assert_eq!(views.len(), 2);
    assert!(views.iter().all(|v| v.repo_name == "r" && v.jira_key.as_deref() == Some("K-1")));

    store.delete_worktree(primary).await.unwrap();
    assert!(store.get_worktree(primary).await.unwrap().is_none());
    // The alternate remains; with no primary left, the reuse lookup is now empty.
    assert!(store.primary_worktree_for_ticket(ticket_id).await.unwrap().is_none());
}

#[tokio::test]
async fn session_lifecycle_and_resume_lookup() {
    let dir = TempDir::new("sess");
    let store = open_store(&dir).await;

    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "harmony/local-1", "/tmp/w", false).await.unwrap();

    let sid = store.add_session(ticket_id, wt_id, "/tmp/w", "work").await.unwrap();
    store.set_session_claude_id(sid, "claude-abc").await.unwrap();
    store.set_session_transcript_path(sid, "/tmp/t.jsonl").await.unwrap();

    // set_session_state keeps last_tool when passed None (COALESCE).
    store.set_session_state(sid, "working", Some("Edit")).await.unwrap();
    store.set_session_state(sid, "waiting", None).await.unwrap();
    let view = store.list_sessions().await.unwrap();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].state, "waiting");
    assert_eq!(view[0].last_tool.as_deref(), Some("Edit"));
    assert_eq!(view[0].claude_session_id.as_deref(), Some("claude-abc"));

    // resume + transcript lookups
    assert_eq!(
        store.latest_claude_session_id_for_ticket(ticket_id).await.unwrap().as_deref(),
        Some("claude-abc")
    );
    assert_eq!(
        store.latest_transcript_path_for_ticket(ticket_id).await.unwrap().as_deref(),
        Some("/tmp/t.jsonl")
    );

    // Open work session shows up for reattach.
    assert_eq!(store.tickets_with_open_session().await.unwrap(), vec![ticket_id]);

    // end → no longer open.
    store.end_session(sid).await.unwrap();
    assert!(store.tickets_with_open_session().await.unwrap().is_empty());

    // delete_session only drops ended ones.
    let removed = store.delete_ended_sessions().await.unwrap();
    assert_eq!(removed, 1);
    assert!(store.list_sessions().await.unwrap().is_empty());
}

#[tokio::test]
async fn tickets_with_open_session_excludes_spec_and_end_all() {
    let dir = TempDir::new("specsess");
    let store = open_store(&dir).await;

    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", "/tmp/w", false).await.unwrap();

    // A worktree-less spec/grill session must not count as a reattachable work session.
    let _spec = store.add_session(ticket_id, 0, dir.path().to_str().unwrap(), "spec").await.unwrap();
    assert!(store.tickets_with_open_session().await.unwrap().is_empty());

    let _work = store.add_session(ticket_id, wt_id, "/tmp/w", "work").await.unwrap();
    assert_eq!(store.tickets_with_open_session().await.unwrap(), vec![ticket_id]);

    // end_all_open_sessions zombifies everything still open (called on startup).
    store.end_all_open_sessions().await.unwrap();
    assert!(store.tickets_with_open_session().await.unwrap().is_empty());
}

#[tokio::test]
async fn resume_lookup_ignores_grill_session() {
    // The work session must start fresh from the spec, never resume the grill's conversation.
    let dir = TempDir::new("resumegrill");
    let store = open_store(&dir).await;
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", "/tmp/w", false).await.unwrap();

    // Only a grill (spec) session has run, and it learned a Claude session id.
    let spec = store.add_session(ticket_id, 0, "/tmp/w", "spec").await.unwrap();
    store.set_session_claude_id(spec, "grill-claude-id").await.unwrap();
    // Resume lookup must NOT return the grill id → the first work session starts fresh.
    assert!(store.latest_claude_session_id_for_ticket(ticket_id).await.unwrap().is_none());

    // Once a work session exists, its id is the resume target.
    let work = store.add_session(ticket_id, wt_id, "/tmp/w", "work").await.unwrap();
    store.set_session_claude_id(work, "work-claude-id").await.unwrap();
    assert_eq!(
        store.latest_claude_session_id_for_ticket(ticket_id).await.unwrap().as_deref(),
        Some("work-claude-id")
    );
}

#[tokio::test]
async fn settings_kv_upsert() {
    let dir = TempDir::new("kv");
    let store = open_store(&dir).await;

    assert!(store.get_setting("permission_mode").await.unwrap().is_none());
    store.set_setting("permission_mode", "auto").await.unwrap();
    store.set_setting("permission_mode", "plan").await.unwrap(); // ON CONFLICT update
    assert_eq!(store.get_setting("permission_mode").await.unwrap().as_deref(), Some("plan"));
}

// =====================================================================
// 2. Worktree: branch naming / slugify / git create + reuse
// =====================================================================

#[test]
fn slugify_normalizes_and_truncates() {
    assert_eq!(worktree::slugify("Hello World"), "hello-world");
    assert_eq!(worktree::slugify("  Spaces & Symbols!! "), "spaces-symbols");
    assert_eq!(worktree::slugify("multi---dash__case"), "multi-dash-case");
    assert_eq!(worktree::slugify("café déjà"), "caf-d-j"); // non-ascii dropped
    assert_eq!(worktree::slugify("---"), ""); // all separators trim to empty
    // Capped at 40 chars.
    let long = "a".repeat(100);
    assert_eq!(worktree::slugify(&long).len(), 40);
}

#[test]
fn branch_name_prefers_jira_key_else_local_id() {
    let mut t = sample_ticket();
    t.jira_key = Some("DNA-42".into());
    t.title = "Add Login Page".into();
    assert_eq!(worktree::branch_name(&t), "harmony/DNA-42-add-login-page");

    t.jira_key = None;
    t.id = 7;
    assert_eq!(worktree::branch_name(&t), "harmony/local-7-add-login-page");
}

#[test]
fn worktree_path_flattens_branch_slashes() {
    let p = worktree::worktree_path("myrepo", "harmony/DNA-1-foo");
    // The repo name is a path segment; the branch's '/' is flattened to '__'.
    assert!(p.ends_with(Path::new("myrepo").join("harmony__DNA-1-foo")), "got {p:?}");
    assert!(p.starts_with(worktree::worktree_root()));
}

#[test]
fn head_sha_and_has_changes() {
    use harmony_core::github;
    let dir = TempDir::new("headsha");
    let repo = dir.path().join("repo");
    init_git_repo(&repo);
    let repo_str = repo.to_string_lossy().to_string();

    let dest = dir.path().join("wt");
    worktree::create(&repo_str, "main", "harmony/local-1-x", &dest).unwrap();
    let wt = dest.to_string_lossy().to_string();

    // Fresh worktree off main: a 40-char HEAD sha, no changes vs base.
    let sha = github::head_sha(&wt).unwrap();
    assert_eq!(sha.len(), 40, "rev-parse HEAD should be a full sha");
    assert!(github::diff(&wt, "main").unwrap().trim().is_empty(), "no changes yet");

    // Commit a change on the branch → diff vs base is non-empty and HEAD moves.
    std::fs::write(dest.join("new.txt"), "hello").unwrap();
    let run = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&dest).args(args).status().unwrap().success());
    };
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "change"]);
    assert!(!github::diff(&wt, "main").unwrap().trim().is_empty(), "now has changes");
    assert_ne!(github::head_sha(&wt).unwrap(), sha, "HEAD advanced after commit");
}

#[tokio::test]
async fn reviewed_flag_persists() {
    let dir = TempDir::new("reviewed");
    let store = open_store(&dir).await;
    let id = store.add_ticket(None, "local", "T", "", None).await.unwrap();

    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.reviewed, 0);
    assert_eq!(t.reviewed_sha, "");

    store.mark_reviewed(id, "abc123").await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.reviewed, 1);
    assert_eq!(t.reviewed_sha, "abc123");

    store.clear_reviewed(id).await.unwrap();
    let t = store.get_ticket(id).await.unwrap().unwrap();
    assert_eq!(t.reviewed, 0);
    assert_eq!(t.reviewed_sha, "");
}

#[test]
fn git_worktree_create_then_reuse() {
    let dir = TempDir::new("git");
    let repo = dir.path().join("repo");
    init_git_repo(&repo);
    let repo_str = repo.to_string_lossy().to_string();

    // No remote → default branch falls back to current HEAD ("main").
    assert_eq!(worktree::default_branch(&repo_str).unwrap(), "main");

    let branch = "harmony/local-1-feature";
    let dest = dir.path().join("wt");

    // First create: branch doesn't exist yet → created off base, worktree checked out.
    worktree::create(&repo_str, "main", branch, &dest).unwrap();
    assert!(dest.join("README.md").exists(), "worktree should contain the committed file");
    assert!(branch_exists(&repo_str, branch), "branch should now exist");

    // Remove the worktree dir but keep the branch (mirrors Done: drop worktree, keep PR branch).
    worktree::remove(&repo_str, &dest).unwrap();
    assert!(!dest.exists());

    // Second create: branch already exists → reused (not re-forked), worktree re-checked-out.
    worktree::create(&repo_str, "main", branch, &dest).unwrap();
    assert!(dest.join("README.md").exists(), "reused worktree should be checked out again");
}

#[test]
fn worktree_dirty_detection() {
    let dir = TempDir::new("dirty");
    let repo = dir.path().join("repo");
    init_git_repo(&repo);
    let repo_str = repo.to_string_lossy().to_string();

    let dest = dir.path().join("wt");
    worktree::create(&repo_str, "main", "harmony/local-1-x", &dest).unwrap();
    let wt = dest.to_string_lossy().to_string();

    // Freshly checked-out worktree is clean.
    assert!(!worktree::is_dirty(&wt).unwrap());
    assert_eq!(worktree::uncommitted_count(&wt).unwrap(), 0);

    // An untracked file makes it dirty.
    std::fs::write(dest.join("scratch.txt"), "wip").unwrap();
    assert!(worktree::is_dirty(&wt).unwrap());
    assert_eq!(worktree::uncommitted_count(&wt).unwrap(), 1);

    // A modification to a tracked file also counts.
    std::fs::write(dest.join("README.md"), "changed").unwrap();
    assert_eq!(worktree::uncommitted_count(&wt).unwrap(), 2);
}

#[tokio::test]
async fn fail_session_marks_error_and_ends() {
    let dir = TempDir::new("failsess");
    let store = open_store(&dir).await;
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", "/tmp/w", false).await.unwrap();
    let sid = store.add_session(ticket_id, wt_id, "/tmp/w", "work").await.unwrap();

    store.fail_session(sid).await.unwrap();

    // Errored session is ended (state 'error', not a lingering 'working') and no longer open.
    let view = store.list_sessions().await.unwrap();
    assert_eq!(view[0].state, "error");
    assert!(view[0].ended_at.is_some());
    assert!(store.tickets_with_open_session().await.unwrap().is_empty());
    // It is ended, so the cleanup sweep can remove it.
    assert_eq!(store.delete_ended_sessions().await.unwrap(), 1);
}

// =====================================================================
// 3. Hook server: cwd → session correlation
// =====================================================================

#[tokio::test]
async fn active_session_by_cwd_latest_wins_and_excludes_ended() {
    let dir = TempDir::new("cwd");
    let store = open_store(&dir).await;
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", "/tmp/w", false).await.unwrap();

    let cwd = "/tmp/work-a";
    let first = store.add_session(ticket_id, wt_id, cwd, "work").await.unwrap();
    let second = store.add_session(ticket_id, wt_id, cwd, "work").await.unwrap();

    // Two live sessions share a cwd → the latest (highest id) wins.
    assert_eq!(store.active_session_by_cwd(cwd).await.unwrap().unwrap().id, second);

    // End the latest → correlation falls back to the still-open earlier one.
    store.end_session(second).await.unwrap();
    assert_eq!(store.active_session_by_cwd(cwd).await.unwrap().unwrap().id, first);

    // Unknown cwd → no match.
    assert!(store.active_session_by_cwd("/tmp/nowhere").await.unwrap().is_none());
}

#[tokio::test]
async fn hook_server_correlates_and_drives_work_session() {
    let dir = TempDir::new("hookwork");
    let store = Arc::new(open_store(&dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    // cwd must be a real directory so the server's canonicalize matches what we stored.
    let cwd = dir.str();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", &cwd, false).await.unwrap();
    let sid = store.add_session(ticket_id, wt_id, &cwd, "work").await.unwrap();

    let port = spawn_hooks(store.clone()).await;
    let client = reqwest::Client::new();

    // PreToolUse(TodoWrite): learns the claude session id, sets working, mirrors todos.
    post(&client, port, "PreToolUse", &serde_json::json!({
        "cwd": cwd,
        "session_id": "claude-xyz",
        "transcript_path": "/tmp/transcript.jsonl",
        "tool_name": "TodoWrite",
        "tool_input": { "todos": [ { "content": "do thing", "status": "in_progress" } ] }
    })).await;

    let sess = store.active_session_by_cwd(&cwd).await.unwrap().unwrap();
    assert_eq!(sess.id, sid);
    assert_eq!(sess.claude_session_id.as_deref(), Some("claude-xyz"));
    assert_eq!(sess.state, "working");
    let t = store.get_ticket(ticket_id).await.unwrap().unwrap();
    assert_eq!(t.status, status::WORKING);
    assert!(t.todos.contains("do thing") && t.todos.contains("in_progress"));
    assert_eq!(
        store.latest_transcript_path_for_ticket(ticket_id).await.unwrap().as_deref(),
        Some("/tmp/transcript.jsonl")
    );

    // Stop → session waiting, ticket moves to "For Your Review".
    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd, "session_id": "claude-xyz" })).await;
    assert_eq!(store.active_session_by_cwd(&cwd).await.unwrap().unwrap().state, "waiting");
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().status, status::WAITING);

    // A hook for an unknown cwd is a no-op (no panic, no state change).
    post(&client, port, "PreToolUse", &serde_json::json!({ "cwd": "/tmp/no-such-session" })).await;
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().status, status::WAITING);
}

#[tokio::test]
async fn hook_server_spec_session_captures_plan_without_moving_board() {
    let dir = TempDir::new("hookspec");
    let store = Arc::new(open_store(&dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "Draft", "", Some(repo_id)).await.unwrap();
    store.set_ticket_drafting(ticket_id, true).await.unwrap();
    let cwd = dir.str();
    // Spec/grill session: worktree-less (worktree_id 0), cwd = repo root.
    store.add_session(ticket_id, 0, &cwd, "spec").await.unwrap();

    let port = spawn_hooks(store.clone()).await;
    let client = reqwest::Client::new();

    // ExitPlanMode in a spec session: capture plan as spec, clear drafting, mark grilled —
    // but the ticket must NOT be dragged onto the board (stays a Todo draft).
    post(&client, port, "PreToolUse", &serde_json::json!({
        "cwd": cwd,
        "tool_name": "ExitPlanMode",
        "tool_input": { "plan": "# The Spec\n- step one" }
    })).await;

    let t = store.get_ticket(ticket_id).await.unwrap().unwrap();
    assert_eq!(t.spec, "# The Spec\n- step one");
    assert_eq!(t.drafting, 0);
    assert_eq!(t.grilled, 1);
    assert_eq!(t.status, status::TODO, "spec session must not move the board column");
}

#[tokio::test]
async fn hook_server_spec_session_captures_plan_file_write() {
    // Current Claude Code plan mode writes the plan to ~/.claude/plans/*.md via the Write tool
    // instead of carrying it in ExitPlanMode — the spec must be captured from that write.
    let dir = TempDir::new("hookplanfile");
    let store = Arc::new(open_store(&dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "Draft", "", Some(repo_id)).await.unwrap();
    store.set_ticket_drafting(ticket_id, true).await.unwrap();
    let cwd = dir.str();
    store.add_session(ticket_id, 0, &cwd, "spec").await.unwrap();

    let port = spawn_hooks(store.clone()).await;
    let client = reqwest::Client::new();

    let plan = "# Title\n\nBody.\n\n## Acceptance criteria\n- a\n\n## Constraints\nkeep small";
    // A Write to a plan file → captured as the spec.
    post(&client, port, "PreToolUse", &serde_json::json!({
        "cwd": cwd,
        "tool_name": "Write",
        "tool_input": { "file_path": "/Users/x/.claude/plans/scoping-foo.md", "content": plan }
    })).await;

    let t = store.get_ticket(ticket_id).await.unwrap().unwrap();
    assert_eq!(t.acceptance_criteria, "- a");
    assert_eq!(t.constraints, "keep small");
    assert!(t.spec.contains("Body."));
    assert_eq!(t.drafting, 0);
    assert_eq!(t.grilled, 1);
    assert_eq!(t.status, status::TODO);
}

#[tokio::test]
async fn hook_server_spec_session_ignores_non_plan_write() {
    // A Write to some other file during the grill must NOT be mistaken for the spec.
    let dir = TempDir::new("hooknonplan");
    let store = Arc::new(open_store(&dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "Draft", "", Some(repo_id)).await.unwrap();
    store.set_ticket_drafting(ticket_id, true).await.unwrap();
    let cwd = dir.str();
    store.add_session(ticket_id, 0, &cwd, "spec").await.unwrap();

    let port = spawn_hooks(store.clone()).await;
    let client = reqwest::Client::new();

    post(&client, port, "PreToolUse", &serde_json::json!({
        "cwd": cwd,
        "tool_name": "Write",
        "tool_input": { "file_path": "/repo/src/notes.txt", "content": "scratch" }
    })).await;

    let t = store.get_ticket(ticket_id).await.unwrap().unwrap();
    assert_eq!(t.spec, "", "non-plan write must not be captured as the spec");
    assert_eq!(t.drafting, 1, "still drafting — the grill hasn't produced a spec");
}

// =====================================================================
// 4. Hook → executor system events (Phase 2 event bus)
// =====================================================================

/// Seed a ticket + worktree + a live session of `kind`, returning (store, ticket_id, cwd).
async fn seed_session(dir: &TempDir, kind: &str) -> (Arc<Store>, i64, String) {
    let store = Arc::new(open_store(dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    let cwd = dir.str();
    let wt_id = store.add_worktree(ticket_id, repo_id, "b", &cwd, false).await.unwrap();
    let _ = store.add_session(ticket_id, wt_id, &cwd, kind).await.unwrap();
    (store, ticket_id, cwd)
}

#[tokio::test]
async fn hook_emits_work_finished_on_stop_without_question() {
    let dir = TempDir::new("evwork");
    let (store, ticket_id, cwd) = seed_session(&dir, "work").await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    let ev = rx.recv().await.expect("a system event");
    assert_eq!(ev, hooks::SystemEvent::WorkFinished { ticket_id });
    // App mode must NOT write ticket status directly (the executor owns it).
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().status, status::TODO);
}

#[tokio::test]
async fn hook_emits_work_finished_even_when_question_pending() {
    // The hook is a thin adapter: it always emits WorkFinished on a work Stop. The "a pending
    // question means work isn't done" rule now lives in `flow::decide`
    // (see `core/tests/flow.rs::work_finished_with_question_pending_stays_in_progress`).
    let dir = TempDir::new("evq");
    let (store, ticket_id, cwd) = seed_session(&dir, "work").await;
    store.set_ticket_question(ticket_id, r#"{"session_id":1,"questions":[]}"#).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::WorkFinished { ticket_id });
}

#[tokio::test]
async fn hook_emits_review_finished_on_review_stop() {
    let dir = TempDir::new("evreview");
    let (store, ticket_id, cwd) = seed_session(&dir, "review").await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::ReviewFinished { ticket_id });
}

#[tokio::test]
async fn hook_emits_grill_finished_on_spec_capture() {
    let dir = TempDir::new("evgrill");
    let store = Arc::new(open_store(&dir).await);
    let repo_id = store.add_repo("r", dir.path().to_str().unwrap(), None).await.unwrap();
    let ticket_id = store.add_ticket(None, "local", "T", "", Some(repo_id)).await.unwrap();
    store.set_ticket_drafting(ticket_id, true).await.unwrap();
    let cwd = dir.str();
    store.add_session(ticket_id, 0, &cwd, "spec").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "PreToolUse", &serde_json::json!({
        "cwd": cwd,
        "tool_name": "Write",
        "tool_input": { "file_path": "/x/.claude/plans/foo.md", "content": "# S\n\n## Constraints\nx" }
    })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::GrillFinished { ticket_id });
    // Spec still captured as before.
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().drafting, 0);
}

#[tokio::test]
async fn hook_emits_session_idle_for_spec_stop_when_auto_end_on() {
    // auto_end_idle on: a spec/grill session that comes to rest on a Stop (no pending question,
    // no plan captured) has its PTY freed via SessionIdle.
    let dir = TempDir::new("evidle");
    let (store, ticket_id, cwd) = seed_session(&dir, "spec").await;
    store.set_setting("auto_end_idle", "on").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::SessionIdle { ticket_id });
}

#[tokio::test]
async fn hook_emits_session_idle_regardless_of_auto_end() {
    // The hook always emits SessionIdle for an idle (spec/other) Stop; whether the PTY is actually
    // freed is decided by `flow::decide` from the `auto_end_idle` fact
    // (see `core/tests/flow.rs::session_idle_*`). Here the setting is off, yet the event still fires.
    let dir = TempDir::new("evidleoff");
    let (store, ticket_id, cwd) = seed_session(&dir, "spec").await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::SessionIdle { ticket_id });
}

#[tokio::test]
async fn hook_emits_session_idle_even_when_question_pending() {
    // Same thin-adapter contract: the hook emits SessionIdle even with a question pending. The
    // "keep the session alive for the user" rule is enforced in `flow::decide`
    // (see `core/tests/flow.rs::session_idle_with_question_pending_keeps_session_alive`).
    let dir = TempDir::new("evidleq");
    let (store, ticket_id, cwd) = seed_session(&dir, "spec").await;
    store.set_setting("auto_end_idle", "on").await.unwrap();
    store.set_ticket_question(ticket_id, r#"{"session_id":1,"questions":[]}"#).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hooks::SystemEvent>();
    let port = spawn_hooks_with(store.clone(), Some(tx)).await;
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;

    assert_eq!(rx.recv().await.unwrap(), hooks::SystemEvent::SessionIdle { ticket_id });
}

#[tokio::test]
async fn hook_cli_mode_still_writes_status() {
    // events=None (CLI): the hook keeps the legacy direct ticket-status write on a work Stop.
    let dir = TempDir::new("evcli");
    let (store, ticket_id, cwd) = seed_session(&dir, "work").await;
    let port = spawn_hooks(store.clone()).await; // None
    let client = reqwest::Client::new();

    post(&client, port, "Stop", &serde_json::json!({ "cwd": cwd })).await;
    assert_eq!(store.get_ticket(ticket_id).await.unwrap().unwrap().status, status::WAITING);
}

// ---- helpers -------------------------------------------------------------

fn sample_ticket() -> harmony_core::models::Ticket {
    harmony_core::models::Ticket {
        id: 1,
        jira_key: None,
        source: "local".into(),
        title: "T".into(),
        spec: "".into(),
        status: status::TODO.into(),
        repo_id: None,
        created_at: 0,
        updated_at: 0,
        todos: "".into(),
        pending_question: "".into(),
        planned: 0,
        drafting: 0,
        grilled: 0,
        acceptance_criteria: "".into(),
        relevant_paths: "".into(),
        constraints: "".into(),
        reviewed: 0,
        reviewed_sha: "".into(),
        review_text: "".into(),
        ci_triaged_sha: "".into(),
        ci_fix_attempts: 0,
        ci_triage: "".into(),
        proposed_spec: "".into(),
        review_verdict: "".into(),
        review_findings: "".into(),
        judged_sha: "".into(),
        review_fix_attempts: 0,
        activity: "".into(),
    }
}

fn init_git_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    let run = |args: &[&str]| {
        let ok = Command::new("git").arg("-C").arg(repo).args(args).status().unwrap().success();
        assert!(ok, "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@harmony.local"]);
    run(&["config", "user.name", "Harmony Test"]);
    std::fs::write(repo.join("README.md"), "hello").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

fn branch_exists(repo: &str, branch: &str) -> bool {
    // `rev-parse --verify` prints the SHA on success — capture it so it doesn't leak to
    // the test runner's stdout.
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()
        .unwrap()
        .status
        .success()
}

/// Bind the hook server on a free ephemeral port (no executor channel) and return it.
async fn spawn_hooks(store: Arc<Store>) -> u16 {
    spawn_hooks_with(store, None).await
}

/// Bind the hook server with an optional system-event channel (app mode) and return the port.
async fn spawn_hooks_with(
    store: Arc<Store>,
    events: Option<tokio::sync::mpsc::UnboundedSender<hooks::SystemEvent>>,
) -> u16 {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    hooks::spawn_server(store, port, events).await.unwrap();
    port
}

async fn post(client: &reqwest::Client, port: u16, event: &str, body: &serde_json::Value) {
    let resp = client
        .post(format!("http://127.0.0.1:{port}/hook/{event}"))
        .json(body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "hook POST {event} failed: {}", resp.status());
}
