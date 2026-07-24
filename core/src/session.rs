//! PTY-based Claude session manager (DESIGN Q4/Q9/Q12).
//!
//! Starts (or resumes) an interactive `claude` process inside a ticket's worktree,
//! after injecting the hook settings. Returns a handle exposing the PTY master so a
//! caller (the CLI today, the Tauri UI later) can bridge/attach a terminal. Session
//! end is detected by the child process exiting (Phase 0: SessionEnd hook unreliable).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;

use crate::models::{DiffComment, Repo, Ticket, Worktree};
use crate::store::Store;
use crate::worktree;

pub struct SessionHandle {
    pub session_id: i64,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub struct SessionManager {
    store: Arc<Store>,
    hook_port: u16,
}

impl SessionManager {
    pub fn new(store: Arc<Store>, hook_port: u16) -> Self {
        Self { store, hook_port }
    }

    /// The ticket's primary worktree, creating it (off the repo's default branch) if absent.
    /// If recording the new worktree in the DB fails, the on-disk worktree is rolled back so we
    /// never leave a half-created worktree (a directory with no row that breaks the next
    /// `git worktree add`).
    async fn ensure_primary_worktree(
        &self,
        ticket: &Ticket,
        repo_id: i64,
        repo: &Repo,
    ) -> Result<Worktree> {
        if let Some(w) = self.store.primary_worktree_for_ticket(ticket.id).await? {
            return Ok(w);
        }
        let branch = worktree::branch_name(ticket);
        let dest = worktree::worktree_path(&repo.name, &branch);
        let base = worktree::default_branch(&repo.path)?;
        worktree::create(&repo.path, &base, &branch, &dest)?;
        let canon = std::fs::canonicalize(&dest)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| dest.to_string_lossy().to_string());
        let recorded = async {
            let id = self
                .store
                .add_worktree(ticket.id, repo_id, &branch, &canon, false)
                .await?;
            self.store
                .get_worktree(id)
                .await?
                .ok_or_else(|| anyhow!("worktree insert failed"))
        }
        .await;
        match recorded {
            Ok(w) => Ok(w),
            Err(e) => {
                let _ = worktree::remove(&repo.path, &dest);
                Err(e)
            }
        }
    }

    /// Create/reuse the ticket's worktree, inject hooks, then spawn (or resume) Claude.
    pub async fn start(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;

        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        // The first work session starts fresh from the spec (planning + implementing in one
        // autonomous run — see `render_implement_prompt`); never `--resume` (that would continue
        // an earlier conversation, e.g. the grill, instead of implementing). Only after that
        // first run (persisted via `planned`) do later sessions resume where they left off.
        let resume = if ticket.planned == 0 {
            None
        } else {
            self.store
                .latest_claude_session_id_for_ticket(ticket_id)
                .await?
        };
        // Autonomy (DESIGN Q1): map the configured setting to a real Claude permission mode.
        // Default is fully autonomous (`bypassPermissions`) — the worktree is isolated and the
        // run is unattended, so it must never stall on a permission prompt.
        let mode = claude_mode(
            &self
                .store
                .get_setting("permission_mode")
                .await?
                .unwrap_or_default(),
        );
        // On resume, fold any open reviewer comments left on the diff into the opening turn so
        // Claude addresses them; otherwise just continue. A fresh first run implements from spec.
        let pending = if resume.is_some() {
            self.store
                .pending_diff_comments_for_ticket(ticket_id)
                .await?
        } else {
            Vec::new()
        };
        let prompt = if resume.is_some() {
            if pending.is_empty() {
                "Continue where you left off.".to_string()
            } else {
                render_feedback_prompt(&pending, &ticket)
            }
        } else {
            render_implement_prompt(&ticket)
        };
        let (master, child) = spawn_claude(&wt.path, &prompt, resume.as_deref(), mode)?;
        if resume.is_none() {
            self.store.mark_ticket_planned(ticket_id).await?;
        }
        if !pending.is_empty() {
            self.store.mark_diff_comments_sent(ticket_id).await?;
        }

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "work")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;

        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start a worktree-less "spec" session: runs an interactive grill interview in plan mode
    /// to produce the ticket's spec, before any work begins. No worktree/branch is created; the
    /// ticket is flagged `drafting` until the spec is captured (on ExitPlanMode, by the hook
    /// server). Does not move the ticket off Todo. `seed` is optional opening context for the
    /// interview (e.g. a Jira ticket's description), woven into the grill prompt but never
    /// persisted — the captured spec comes from the grill.
    ///
    /// The grill runs in the ticket's **git worktree** (created/reused via
    /// `ensure_primary_worktree`), NOT the repo root. The worktree is a unique per-ticket
    /// directory, so its `cwd` can't be confused with another `claude` session in the same repo,
    /// and it inherits the repo's trust (an empty non-git scratch dir would hit Claude's
    /// interactive trust gate and never start). Plan mode keeps it read-only — it explores the
    /// checkout but makes no commits — and the later work session reuses the same worktree.
    pub async fn start_spec_session(
        &self,
        ticket_id: i64,
        seed: Option<String>,
    ) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;
        // Self-heal: an earlier version injected harmony's hooks into the repo root — remove
        // them so the user's own `claude` sessions in that repo stop reporting to harmony.
        let _ = crate::settings::remove_hooks(&repo.path, self.hook_port);

        let prompt = render_grill_prompt(&ticket, seed.as_deref());
        // Plan mode keeps the grill read-only — safe to run in the ticket's worktree.
        let (master, child) = spawn_claude(&wt.path, &prompt, None, "plan")?;

        // worktree_id = 0: the spec session stays worktree-less in the DB (kept out of the
        // Sessions view); correlation is by cwd, which is now the unique worktree path.
        let session_id = self
            .store
            .add_session(ticket_id, 0, &wt.path, "spec")
            .await?;
        self.store.set_ticket_drafting(ticket_id, true).await?;

        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start a read-only `/review` session in the ticket's worktree: Claude reviews the branch's
    /// changes against the spec and presents its verdict via `ExitPlanMode`, then ends (the executor
    /// stops it when the hook captures that plan). Runs in **plan mode** — read-only (edits blocked)
    /// and giving one definitive completion signal (ExitPlanMode). Plan mode would normally prompt to
    /// approve the review's bash (it runs the test suite), so harmony's hook auto-approves the review
    /// session's tool calls (`core/src/hooks.rs`) — no prompts, and edits stay blocked. harmony never
    /// commits a review session's changes.
    pub async fn start_review(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        // Incremental review: if the branch was already reviewed at an earlier commit, focus the
        // re-review on the delta since then (cheaper + sharper); the first review stays full-scope.
        let since = self.store.latest_action_head(ticket_id, "review").await?;
        let prompt = render_review_prompt(&ticket, since.as_deref());
        // Plan mode keeps it read-only; the hook auto-approves the review's bash so it never stalls
        // on an approval prompt, and completion is the definitive ExitPlanMode plan capture.
        let (master, child) = spawn_claude(&wt.path, &prompt, None, "plan")?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "review")
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start a proof-of-work session in the ticket's worktree: an autonomous run (execute perms — it
    /// must run the app/tests and record) that captures the richest feasible evidence the change
    /// works (walkthrough video → screenshots → grounded report), writing media to the per-ticket
    /// artifact dir under `~/.harmony` (never the repo) and the prose report to its plan file (which
    /// the hook captures). Session `kind = "proof"`; harmony fingerprints + collects artifacts on
    /// Stop (`ProofFinished`). Runs after review passes; does NOT change the ticket column (it stays
    /// in "For Your Review" while proof is produced).
    pub async fn start_proof(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;
        // Keep the repo clean: capture leftovers + harmony config must never land in `git add -A`.
        let _ = crate::github::add_git_excludes(
            &wt.path,
            &[
                ".harmony/",
                "node_modules/",
                "playwright-report/",
                "test-results/",
                "*.cast",
                "harmony-proof*",
            ],
        );

        // Provision the shared capture toolchain + the fresh per-run artifact dir, and get the env.
        let penv = crate::settings::provision_proof_env(ticket_id)?;
        let prompt = render_proof_prompt(&ticket, &penv.artifact_dir);
        let (master, child) =
            spawn_claude_with_env(&wt.path, &prompt, None, "bypassPermissions", &penv.env)?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "proof")
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start an autonomous session to fix a failing CI check. Runs in the ticket's worktree with
    /// `bypassPermissions` (unattended), the opening prompt carrying the triage context (failing
    /// check, rationale, proposed fix, and a tail of the failed logs). harmony commits + pushes on
    /// the session's Stop (`FixFinished`), which re-triggers CI. Session `kind = "fix"`.
    pub async fn start_ci_fix(&self, ticket_id: i64, context: &str) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let prompt = render_ci_fix_prompt(context);
        let (master, child) = spawn_claude(&wt.path, &prompt, None, "bypassPermissions")?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "fix")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start an autonomous session to resolve the branch's merge conflicts with its base. Runs in
    /// the worktree with `bypassPermissions`; the prompt tells Claude to merge `origin/<base>` in and
    /// resolve every conflict, then stop. harmony commits (completing the merge) + pushes on the
    /// session's Stop (`ConflictFinished`), updating the PR. Session `kind = "conflict"`.
    pub async fn start_conflict_resolve(
        &self,
        ticket_id: i64,
        base: &str,
    ) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let prompt = render_conflict_resolve_prompt(base);
        let (master, child) = spawn_claude(&wt.path, &prompt, None, "bypassPermissions")?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "conflict")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start an autonomous session to fix the blocking issues the review-loop judge flagged. The
    /// review-loop sibling of `start_address`: resumes the worktree's Claude (keeps context),
    /// `bypassPermissions`, with the judge's must-fix findings (stored as `review_findings` JSON)
    /// folded into the prompt. Errors when there are no findings. Session `kind = "address"` so
    /// harmony commits + pushes on Stop (`AddressFinished`), which moves HEAD and re-triggers the
    /// review loop.
    pub async fn start_review_fix(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let findings: Vec<String> =
            serde_json::from_str(&ticket.review_findings).unwrap_or_default();
        if findings.is_empty() {
            return Err(anyhow!("no review findings to address"));
        }

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let resume = self
            .store
            .latest_claude_session_id_for_ticket(ticket_id)
            .await?;
        let prompt = crate::review::render_review_fix_prompt(&findings, &ticket);
        let (master, child) =
            spawn_claude(&wt.path, &prompt, resume.as_deref(), "bypassPermissions")?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "address")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start an autonomous session to address review feedback (comments from any surface) and
    /// improve the PR. Resumes the worktree's Claude (so it keeps context), `bypassPermissions`,
    /// with all open comments folded into the prompt + the spec reconcile instruction. Marks the
    /// folded comments sent. Errors when there's no pending feedback. Session `kind = "address"`.
    pub async fn start_address(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let pending = self
            .store
            .pending_diff_comments_for_ticket(ticket_id)
            .await?;
        if pending.is_empty() {
            return Err(anyhow!("no pending feedback to address"));
        }

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let resume = self
            .store
            .latest_claude_session_id_for_ticket(ticket_id)
            .await?;
        let prompt = render_feedback_prompt(&pending, &ticket);
        let (master, child) =
            spawn_claude(&wt.path, &prompt, resume.as_deref(), "bypassPermissions")?;
        self.store.mark_diff_comments_sent(ticket_id).await?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "address")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start a session to implement a just-accepted spec revision. The user accepted Claude's
    /// proposed spec (the live spec fields are already updated by the caller), so resume the
    /// worktree's Claude (keeping context from the address session that proposed it),
    /// `bypassPermissions`, with a prompt carrying the updated spec and an instruction to implement
    /// the change it previously deferred. Session `kind = "address"` (resumed autonomous work on an
    /// existing PR; harmony commits + pushes on Stop, same as feedback addressing).
    pub async fn start_implement_spec(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = self
            .ensure_primary_worktree(&ticket, repo_id, &repo)
            .await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let resume = self
            .store
            .latest_claude_session_id_for_ticket(ticket_id)
            .await?;
        let prompt = render_implement_spec_prompt(&ticket);
        let (master, child) =
            spawn_claude(&wt.path, &prompt, resume.as_deref(), "bypassPermissions")?;

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "address")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;
        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    pub async fn end_session(&self, session_id: i64) -> Result<()> {
        self.store.end_session(session_id).await
    }
}

/// Opening prompt for a review session. The review approach is matched to whether a PR exists yet:
/// - **Pre-PR** (For Your Review): there is no pull request for a PR-review skill to fetch, so review
///   the working/branch diff directly (preferring a working-diff skill like `/code-review` when one
///   is available). Naming `/review` here misdirects the agent toward fetching a non-existent PR.
/// - **Post-PR** (In PR Review): the `/review` PR skill can use the pull request's context.
///
/// Skills are only ever *preferred*, never required — harmony spawns in arbitrary repos where a given
/// skill may be absent or not model-invocable, so the prompt always describes the review directly too.
/// Runs in plan mode (read-only; the review's bash is auto-approved by the hook, so it never stalls).
/// The verdict is captured when the review presents it via `ExitPlanMode` — the single definitive
/// completion signal — so the review must do all its investigation first, then call `ExitPlanMode`
/// ONCE with the complete review.
fn render_review_prompt(t: &Ticket, since: Option<&str>) -> String {
    // On a re-review, focus on the delta since the last-reviewed commit (the earlier changes were
    // already reviewed) — keeps the loop's re-reviews cheap and sharp. First review is full-scope.
    let scope = match since {
        Some(sha) if !sha.is_empty() => format!(
            "The changes up to commit `{sha}` were already reviewed. Focus your review on the NEW \
             changes since then (`git diff {sha}..HEAD`); you may consult earlier code for context, \
             but don't re-litigate already-reviewed lines.\n\n"
        ),
        _ => String::new(),
    };
    // Pick the review approach + purpose from whether a PR exists yet.
    let (lead, purpose) = if t.pr_url.trim().is_empty() {
        (
            "Review the changes this branch makes versus its base. If a working-diff review skill \
             such as `/code-review` is available, use it; otherwise review the branch diff directly. \
             Do NOT use the `/review` skill — there is no pull request for this branch yet."
                .to_string(),
            " This is a pre-PR sanity check for the human before they open a PR.".to_string(),
        )
    } else {
        (
            format!(
                "Run the `/review` skill on this branch's open pull request ({}). If that skill is \
                 unavailable, review the branch diff versus its base directly.",
                t.pr_url.trim()
            ),
            " This re-review checks the latest changes on the open PR.".to_string(),
        )
    };
    format!(
        "{lead} Review against the ticket's intent below, and produce a concise, prioritised list \
         of concerns (correctness, edge cases, missing tests, scope creep) and any concrete fixes \
         you'd suggest.{purpose}\n\n\
         {scope}\
         You are in plan mode (read-only): read files and run non-mutating commands (e.g. the test \
         suite, a type-check) to verify your claims — these run without asking, so investigate \
         thoroughly. Do not attempt to edit the repo. Work autonomously — no human is watching, so \
         do not stop to ask for confirmation.\n\n\
         When your investigation is COMPLETE, call `ExitPlanMode` exactly once, passing your full \
         review as the plan: a single, self-contained document (verdict + the prioritised concerns). \
         This is the only way the review is surfaced to the human, so do not end your turn until you \
         have called it.\n\n\
         # {}\n\n{}",
        t.title,
        crate::spec::compose_spec(t)
    )
}

/// Opening prompt for a proof-of-work session. Inlines the full proof methodology (like the grill,
/// nothing is installed in the target repo) so the agent decides the richest feasible evidence for
/// *this* change and repo, captures media to the artifact dir, and writes a grounded report to its
/// plan file (which the hook captures onto the ticket). Runs with execute perms in the worktree.
fn render_proof_prompt(t: &Ticket, artifact_dir: &str) -> String {
    format!(
        "You are producing PROOF OF WORK for a change that has just passed code review, so a human \
         (and teammates on the PR) can review the *output and functionality* instead of reading the \
         diff. Work autonomously and to completion — no human is watching.\n\n\
         The change is on this branch (the worktree you're in). First inspect what it does: read the \
         spec below and run `git diff --merge-base <base>` to see the changes.\n\n\
         Then DECIDE the richest form of proof this change and repo actually support, and produce it \
         — prefer visual, and avoid making the reviewer read code:\n\
         - Runnable web/UI app → capture a short screen recording of the feature working and/or \
         key screenshots. Use a headless browser you drive with `npx playwright` (its browser is \
         cached centrally, so first use may download once) — e.g. `npx --yes playwright screenshot \
         <url> <out.png>`, or a tiny Playwright script that records video. Start the app's dev \
         server first if needed.\n\
         - Desktop/GUI app → record the screen with `ffmpeg` (`-f avfoundation`) or `screencapture` \
         while you exercise the feature.\n\
         - CLI tool → record a terminal cast with `asciinema rec` if available, otherwise run the \
         commands and capture their real output.\n\
         - API/library/backend → run the real endpoints/functions (e.g. `curl`, a test harness) and \
         capture the actual request/response and test output.\n\
         - Nothing runnable applies → produce a grounded written report only.\n\n\
         Write ALL media/evidence files into this directory (it already exists; do not put anything \
         in the repo): {artifact_dir}\n\
         Use clear, descriptive file names (they become captions), e.g. `walkthrough.mp4`, \
         `01-filter-applied.png`, `tests.cast`.\n\n\
         If a capture tool is missing or a step fails, DEGRADE GRACEFULLY — never block; fall back to \
         screenshots, then to a written report. Do not weaken anything to force a capture.\n\n\
         Finally, write your proof report to a file under `.claude/plans/` (this is how the report is \
         surfaced). Ground every claim in REAL output — paste the verbatim output of the commands \
         you actually ran; do not narrate results you didn't produce. Structure it as:\n\
         - **What works now** — one or two plain-language sentences.\n\
         - **How to see it** — the exact steps/commands to reproduce.\n\
         - **Evidence** — reference the media files you captured (by name) and paste verbatim \
         command/test output.\n\n\
         Do NOT edit the repo's code and do NOT run `git commit`/`git push` — this session only \
         produces evidence; harmony handles version control.\n\n\
         # {}\n\n{}",
        t.title,
        crate::spec::compose_spec(t)
    )
}

/// Opening prompt for addressing reviewer feedback. Comments may come from several surfaces
/// (general notes, diff lines, Claude's own `/review`, GitHub PR comments); they're grouped by
/// surface so Claude can locate and act on each, then summarize. The agreed spec is appended with
/// a reconcile instruction: feedback that contradicts the spec must be surfaced as a *proposed*
/// spec update (written to a plan file, captured as `proposed_spec`), not silently implemented.
fn render_feedback_prompt(comments: &[DiffComment], t: &Ticket) -> String {
    let mut out = String::from(
        "The reviewer left feedback on this PR. Address each item below — make the necessary code \
         edits — then briefly summarize what you changed per item.\n\n",
    );
    let (mut general, mut diff, mut review, mut pr) = (vec![], vec![], vec![], vec![]);
    for c in comments {
        let body = c.body.trim().to_string();
        let anchor = c.anchor.trim();
        match c.target.as_str() {
            "diff" => {
                let loc = if c.end_line > c.line {
                    format!("{}:{}-{}", c.file_path, c.line, c.end_line)
                } else {
                    format!("{}:{}", c.file_path, c.line)
                };
                diff.push(format!("`{}` ({}): {}", loc, c.side, body));
            }
            "review" => review.push(if anchor.is_empty() {
                body
            } else {
                format!("re: \"{anchor}\": {body}")
            }),
            "pr_comment" => pr.push(if anchor.is_empty() {
                body
            } else {
                format!("{anchor} — {body}")
            }),
            // "general" and any unknown target
            _ => general.push(body),
        }
    }
    let mut group = |title: &str, items: &[String]| {
        if !items.is_empty() {
            out.push_str(title);
            out.push('\n');
            for i in items {
                out.push_str(&format!("- {i}\n"));
            }
            out.push('\n');
        }
    };
    group("General comments:", &general);
    group("On the diff:", &diff);
    group("On your review:", &review);
    group("On GitHub PR comments:", &pr);

    let spec = crate::spec::compose_spec(t);
    if !spec.trim().is_empty() {
        out.push_str(
            "---\nThe agreed spec is below. If any feedback contradicts it, do NOT silently \
             diverge — it may mean our agreed direction has changed. For any such item, write the \
             full revised spec (with the exact sections `## Acceptance criteria`, \
             `## Relevant paths`, `## Constraints`) to a file under `.claude/plans/`, and clearly \
             note which feedback contradicted which part of the spec and why. Do not implement a \
             spec-contradicting change until the spec update is accepted; implement all \
             non-contradicting feedback now.\n\n# Spec\n",
        );
        out.push_str(&spec);
        out.push('\n');
    }
    out
}

/// Opening prompt for an autonomous CI-fix session: a CI check on this branch's PR is failing and
/// has been attributed to this PR's changes. `context` carries the failing check name, the
/// attribution rationale/proposed fix, and a tail of the failed-job logs. Fix only that failure;
/// harmony handles version control.
fn render_ci_fix_prompt(context: &str) -> String {
    format!(
        "A CI check on this branch's pull request is failing, and triage attributed the failure to \
         this branch's changes. Investigate and fix it autonomously and to completion — no human is \
         watching, so do not ask for confirmation. Make only the changes needed to fix this CI \
         failure; do not refactor or touch unrelated code, and do not weaken or delete tests to make \
         them pass. Run the relevant checks/tests locally to confirm the fix. Do NOT run `git commit` \
         or `git push` — harmony commits and pushes your changes, which re-triggers CI.\n\n\
         # Failing CI context\n\n{context}"
    )
}

/// Opening prompt for an autonomous conflict-resolve session: this branch's PR conflicts with its
/// base (`base`). Merge the base in and resolve every conflict faithfully; harmony completes the
/// merge commit + pushes.
fn render_conflict_resolve_prompt(base: &str) -> String {
    format!(
        "This branch's pull request has merge conflicts with its base branch (`{base}`). Resolve them \
         autonomously and to completion — no human is watching, so do not ask for confirmation.\n\n\
         Steps:\n\
         1. Run `git fetch origin`.\n\
         2. Run `git merge origin/{base}` to merge the latest base into this branch.\n\
         3. Resolve EVERY conflict faithfully — preserve the intent of BOTH sides (this branch's \
         change and the base's change); never discard one side wholesale or delete/weaken tests to \
         sidestep a conflict. Remove all conflict markers.\n\
         4. Make sure it still builds and the relevant tests pass.\n\n\
         Do NOT run `git commit` or `git push` — harmony completes the merge commit and pushes your \
         resolution, which updates the PR. (If the merge turns out to have no real conflicts, just \
         leave the merged state for harmony to commit.)"
    )
}

/// Render the ticket spec (body + structured fields) into Claude's opening prompt (DESIGN Q10).
fn render_prompt(t: &Ticket) -> String {
    let composed = crate::spec::compose_spec(t);
    if composed.trim().is_empty() {
        format!("Work on this task: {}", t.title)
    } else {
        format!("# {}\n\n{}", t.title, composed)
    }
}

/// Opening prompt for the autonomous implement session (first In Progress start). The spec is
/// the agreed plan (from the grill); this run plans *and* executes in one go: first record the
/// task breakdown with TodoWrite (mirrored onto the ticket — the visible "plan pass"), then
/// implement it fully. Launched fully autonomous (`bypassPermissions`); harmony commits the work,
/// so the agent must not git-commit/push itself.
fn render_implement_prompt(t: &Ticket) -> String {
    format!(
        "{}\n\n---\nImplement this task autonomously and to completion — no human is watching, so \
         do not ask for confirmation. First break the spec into a concrete, ordered list of \
         low-level steps and record it with the TodoWrite tool (this saves the plan to the \
         ticket), then carry the steps out, keeping the task list updated as you go. Make all \
         necessary code changes and run whatever you need (tests, build) to be confident it's \
         correct. Do NOT run `git commit` or `git push` — harmony handles version control.",
        render_prompt(t)
    )
}

/// Opening prompt for implementing a just-accepted spec revision. While addressing feedback Claude
/// proposed a revised spec (because feedback contradicted the old one) and deferred implementing it;
/// the user has now reviewed and accepted that revision, so the live spec below is the updated one.
/// Resumed (keeps the address-session context), `bypassPermissions`; harmony handles version control.
fn render_implement_spec_prompt(t: &Ticket) -> String {
    format!(
        "The revised spec you proposed has been reviewed and accepted — the agreed spec is now the \
         version below. Implement the spec change you previously deferred (the one that contradicted \
         the earlier spec), making all necessary code edits to satisfy the updated spec, then briefly \
         summarize what you changed. Work autonomously and to completion — no human is watching, so \
         do not ask for confirmation. Do NOT run `git commit` or `git push` — harmony handles version \
         control.\n\n{}",
        render_prompt(t)
    )
}

/// Map harmony's `permission_mode` setting to a real Claude `--permission-mode`. Default (and
/// the `auto` legacy value) is `bypassPermissions` — fully autonomous in the isolated worktree;
/// an explicit `plan`/`default`/`supervised` setting keeps prompts on (supervised).
fn claude_mode(setting: &str) -> &'static str {
    match setting {
        "plan" | "default" | "supervised" => "default",
        _ => "bypassPermissions",
    }
}

/// Opening prompt for a spec/grill session (phase 1, at ticket creation). Inlines the
/// `grill-me` interview (the skill isn't installed in target repos) and ends by asking
/// Claude to write the finished spec as its plan and present it via ExitPlanMode — which the
/// hook server captures onto the ticket. Launched with `--permission-mode plan` (read-only).
fn render_grill_prompt(t: &Ticket, seed: Option<&str>) -> String {
    // Opening context = the ticket's existing spec/fields plus any transient seed (e.g. a Jira
    // description), whichever are present.
    let mut idea = crate::spec::compose_spec(t);
    if let Some(s) = seed {
        let s = s.trim();
        if !s.is_empty() {
            if !idea.trim().is_empty() {
                idea.push_str("\n\n");
            }
            idea.push_str(s);
        }
    }
    let seed = if idea.trim().is_empty() {
        format!("We're scoping a new task: {}", t.title)
    } else {
        format!(
            "We're scoping a new task — \"{}\".\n\nInitial idea / context:\n{}",
            t.title, idea
        )
    };
    format!(
        "{seed}\n\n\
         Interview me relentlessly about every aspect of this task until we reach a shared \
         understanding. Walk down each branch of the design tree, resolving dependencies \
         between decisions one-by-one. For each question, provide your recommended answer. \
         Ask the questions one at a time. If a question can be answered by exploring the \
         codebase, explore the codebase instead of asking.\n\n\
         When we've reached a shared understanding, write the complete specification for \
         this task as your plan and call ExitPlanMode to present it. Structure the spec as a \
         short body (Goal, Context) followed by these exact markdown sections so it can be \
         parsed into fields: `## Acceptance criteria`, `## Relevant paths`, `## Constraints`. \
         Do not write any code or make changes — this session exists only to produce the spec."
    )
}

/// Render a Claude Code session transcript (JSONL) into a readable plain-text
/// conversation for the "Conversation so far" pane. Best-effort / approximate — the TUI
/// uses the alternate screen, so we can't faithfully rebuild xterm scrollback; this gives
/// the prior conversation instead.
pub fn render_transcript(path: &str) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    let mut out = String::new();
    for line in content.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let msg = v.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or(typ);
        let content_node = msg
            .and_then(|m| m.get("content"))
            .or_else(|| v.get("content"));
        let text = extract_blocks(content_node);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match role {
            "user" => {
                out.push_str("❯ ");
                out.push_str(text);
                out.push_str("\n\n");
            }
            "assistant" => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            _ => {}
        }
    }
    Ok(out.trim_end().to_string())
}

/// One typed block inside a structured transcript message. This is the friendly-view shape: the
/// GUI renders `text` as markdown, `tool_use` as a compact card, and `tool_result` (hidden by
/// default) as the expandable output of the matching `tool_use` (associated by `tool_use_id`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptBlock {
    /// Assistant/user prose.
    Text { text: String },
    /// A tool invocation: the tool name plus a one-line target/summary of its input.
    ToolUse {
        id: String,
        name: String,
        summary: String,
    },
    /// A tool's output, keyed back to its `tool_use` by `tool_use_id`. Capped in size.
    ToolResult {
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
}

/// One structured message in a session's conversation — a role plus its ordered typed blocks.
/// Generalises the flat-string `render_transcript` so the friendly GUI can render each piece
/// natively (markdown text, tool cards, collapsed results) instead of as one plain blob.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptMessage {
    /// `assistant` or `user`.
    pub role: String,
    pub blocks: Vec<TranscriptBlock>,
}

/// Derive a one-line target/summary from a tool's input JSON (e.g. the file path for Edit/Write,
/// the command for Bash, the pattern for Grep). Best-effort: picks the first recognised field,
/// collapsed to a single tidy line. Empty when nothing informative is present.
fn summarize_tool_input(input: Option<&Value>) -> String {
    const MAX: usize = 160;
    let obj = match input.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return String::new(),
    };
    // Ordered by how identifying the field is for the common tools.
    for key in [
        "file_path",
        "filePath",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "prompt",
        "description",
        "notebook_path",
    ] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return collapse(s, MAX);
            }
        }
    }
    String::new()
}

/// Extract a tool_result's textual output. The content may be a bare string, or an array of
/// `{type:"text", text}` blocks (Claude Code's usual shape). Capped so a huge result can't bloat
/// the payload — the GUI hides it behind an expander anyway.
fn extract_tool_result(content: Option<&Value>) -> String {
    const CAP: usize = 10_000;
    let raw = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut s = String::new();
            for b in arr {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    s.push_str(t);
                    s.push('\n');
                } else if let Some(t) = b.as_str() {
                    s.push_str(t);
                    s.push('\n');
                }
            }
            s.trim_end().to_string()
        }
        Some(other) => other.to_string(),
        None => String::new(),
    };
    if raw.chars().count() > CAP {
        let truncated: String = raw.chars().take(CAP).collect();
        format!("{truncated}\n… (truncated)")
    } else {
        raw
    }
}

/// Parse a session's full JSONL transcript into ordered, structured messages for the friendly
/// GUI view. Unlike `render_transcript` (a flat plain-text blob) and the tail-only turn-state
/// helpers, this reads the whole file so the entire conversation renders. Non-message records
/// (summaries, system lines) and empty messages are skipped.
pub fn structured_transcript(path: &str) -> Result<Vec<TranscriptMessage>> {
    let content = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let msg = v.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or(typ);
        if role != "assistant" && role != "user" {
            continue;
        }
        let content_node = msg
            .and_then(|m| m.get("content"))
            .or_else(|| v.get("content"));
        let mut blocks: Vec<TranscriptBlock> = Vec::new();
        match content_node {
            Some(Value::String(s)) => {
                if !s.trim().is_empty() {
                    blocks.push(TranscriptBlock::Text { text: s.clone() });
                }
            }
            Some(Value::Array(arr)) => {
                for b in arr {
                    match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                if !t.trim().is_empty() {
                                    blocks.push(TranscriptBlock::Text {
                                        text: t.to_string(),
                                    });
                                }
                            }
                        }
                        "tool_use" => {
                            blocks.push(TranscriptBlock::ToolUse {
                                id: b
                                    .get("id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                name: b
                                    .get("name")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("tool")
                                    .to_string(),
                                summary: summarize_tool_input(b.get("input")),
                            });
                        }
                        "tool_result" => {
                            blocks.push(TranscriptBlock::ToolResult {
                                tool_use_id: b
                                    .get("tool_use_id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                output: extract_tool_result(b.get("content")),
                                is_error: b
                                    .get("is_error")
                                    .and_then(|x| x.as_bool())
                                    .unwrap_or(false),
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if blocks.is_empty() {
            continue;
        }
        out.push(TranscriptMessage {
            role: role.to_string(),
            blocks,
        });
    }
    Ok(out)
}

/// A snapshot of in-session progress tailed from the live transcript: the latest assistant
/// text and the most recently invoked tool. Richer than the hook-derived working/waiting flag.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TranscriptProgress {
    /// Latest assistant text block (newlines collapsed, capped), if any.
    pub message: Option<String>,
    /// Name of the most recent `tool_use`, if any.
    pub tool: Option<String>,
}

/// Tail a session's JSONL transcript and extract the latest in-session progress without
/// reading the whole file: we seek to the last `TAIL` bytes, drop the (likely partial) first
/// line, then walk the complete assistant lines tracking the most recent text + tool_use.
pub fn latest_progress(path: &str) -> Option<TranscriptProgress> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    const MAX_MSG: usize = 280;

    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    // The seek may land mid-character; lossily decode and discard a partial leading line.
    let buf = String::from_utf8_lossy(&bytes);
    let body = if start > 0 {
        match buf.find('\n') {
            Some(i) => &buf[i + 1..],
            None => "",
        }
    } else {
        &buf[..]
    };

    let mut progress = TranscriptProgress::default();
    for line in body.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = v.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if role != "assistant" {
            continue;
        }
        if let Some(Value::Array(arr)) = msg.and_then(|m| m.get("content")) {
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            let t = t.trim();
                            if !t.is_empty() {
                                progress.message = Some(collapse(t, MAX_MSG));
                            }
                        }
                    }
                    "tool_use" => {
                        if let Some(n) = b.get("name").and_then(|x| x.as_str()) {
                            progress.tool = Some(n.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if progress.message.is_none() && progress.tool.is_none() {
        None
    } else {
        Some(progress)
    }
}

/// Whether a session's most recent turn has come to rest — derived from the transcript so it works
/// even when the `Stop`/plan-file hooks are missed (the stuck-session watchdog's signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// Claude is mid-turn (last assistant message has a pending tool call, or the last record is a
    /// tool result Claude is about to respond to) — do not disturb.
    Working,
    /// Claude finished its turn (last assistant message is text with no pending tool call).
    Finished,
    /// Claude is blocked on an `AskUserQuestion` — genuinely waiting on the user.
    WaitingOnQuestion,
}

/// Parse the last ~64 KB of a transcript JSONL into its complete records (best-effort: drops a
/// partial leading line from the seek, and any unparseable trailing partial line). Shared by the
/// turn-state helpers below and modelled on `latest_progress`'s tail read.
fn tail_records(path: &str) -> Vec<Value> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(_) => return vec![],
    };
    let start = len.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return vec![];
    }
    let mut bytes = Vec::new();
    if f.read_to_end(&mut bytes).is_err() {
        return vec![];
    }
    let buf = String::from_utf8_lossy(&bytes);
    let body = if start > 0 {
        match buf.find('\n') {
            Some(i) => &buf[i + 1..],
            None => "",
        }
    } else {
        &buf[..]
    };
    body.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// Classify a session's turn state from its transcript's last record. Fail-safe: anything
/// unrecognized (empty/unreadable transcript, an in-flight tool call, a trailing tool result) is
/// treated as `Working` so the watchdog never disturbs a session that isn't clearly at rest.
pub fn transcript_turn_state(path: &str) -> TurnState {
    let records = tail_records(path);
    let last = match records.last() {
        Some(v) => v,
        None => return TurnState::Working,
    };
    let msg = last.get("message");
    let role = msg
        .and_then(|m| m.get("role"))
        .and_then(|x| x.as_str())
        .or_else(|| last.get("type").and_then(|x| x.as_str()))
        .unwrap_or("");
    if role != "assistant" {
        // A user record / tool_result — Claude is about to continue.
        return TurnState::Working;
    }
    match msg.and_then(|m| m.get("content")) {
        Some(Value::Array(arr)) => {
            let (mut has_text, mut has_tool, mut asks_question, mut exits_plan) =
                (false, false, false, false);
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "tool_use" => match b.get("name").and_then(|x| x.as_str()) {
                        // User-gated tools: the turn is at rest waiting on the human, not mid-work.
                        Some("AskUserQuestion") => asks_question = true,
                        // Plan-mode sessions (grill/review) end by presenting their plan via
                        // ExitPlanMode; nothing runs after it autonomously, so it IS the finish.
                        Some("ExitPlanMode") => exits_plan = true,
                        _ => has_tool = true,
                    },
                    "text"
                        if b.get("text")
                            .and_then(|x| x.as_str())
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false) =>
                    {
                        has_text = true;
                    }
                    _ => {}
                }
            }
            if asks_question {
                TurnState::WaitingOnQuestion
            } else if exits_plan {
                TurnState::Finished
            } else if has_tool {
                TurnState::Working
            } else if has_text {
                TurnState::Finished
            } else {
                TurnState::Working
            }
        }
        // A plain-string assistant message is finished text.
        Some(Value::String(s)) if !s.trim().is_empty() => TurnState::Finished,
        _ => TurnState::Working,
    }
}

/// The friendly GUI view's read of where a session's turn has come to rest — a superset of
/// `TurnState` that keeps `ExitPlan` distinct from a plain `Finished`. The friendly view can model
/// `Working` (show a spinner), `Finished` (offer the steer box), and `WaitingOnQuestion` (show the
/// `QuestionCard`); `ExitPlan` (a plan-approval / permission-style prompt it cannot render) is the
/// escape-hatch signal that makes the UI auto-switch to the raw terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewState {
    Working,
    Finished,
    WaitingOnQuestion,
    ExitPlan,
}

/// Classify a session's turn state for the friendly view. Same last-record analysis as
/// `transcript_turn_state`, but reports `ExitPlan` separately (where the watchdog-oriented
/// `transcript_turn_state` folds it into `Finished`) so the GUI can auto-switch to the terminal
/// for a plan-approval prompt. Fail-safe to `Working`.
pub fn transcript_view_state(path: &str) -> ViewState {
    let records = tail_records(path);
    let last = match records.last() {
        Some(v) => v,
        None => return ViewState::Working,
    };
    let msg = last.get("message");
    let role = msg
        .and_then(|m| m.get("role"))
        .and_then(|x| x.as_str())
        .or_else(|| last.get("type").and_then(|x| x.as_str()))
        .unwrap_or("");
    if role != "assistant" {
        return ViewState::Working;
    }
    match msg.and_then(|m| m.get("content")) {
        Some(Value::Array(arr)) => {
            let (mut has_text, mut has_tool, mut asks_question, mut exits_plan) =
                (false, false, false, false);
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "tool_use" => match b.get("name").and_then(|x| x.as_str()) {
                        Some("AskUserQuestion") => asks_question = true,
                        Some("ExitPlanMode") => exits_plan = true,
                        _ => has_tool = true,
                    },
                    "text"
                        if b.get("text")
                            .and_then(|x| x.as_str())
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false) =>
                    {
                        has_text = true;
                    }
                    _ => {}
                }
            }
            if asks_question {
                ViewState::WaitingOnQuestion
            } else if exits_plan {
                ViewState::ExitPlan
            } else if has_tool {
                ViewState::Working
            } else if has_text {
                ViewState::Finished
            } else {
                ViewState::Working
            }
        }
        Some(Value::String(s)) if !s.trim().is_empty() => ViewState::Finished,
        _ => ViewState::Working,
    }
}

/// Whether the transcript's last assistant record presented a plan via `ExitPlanMode`. This is the
/// *definitive* completion signal for a plan-mode session (review/grill): unlike a trailing text
/// block (which can be mid-investigation narration), an `ExitPlanMode` means the agent finished and
/// presented its deliverable. The watchdog uses it to recover a review only on a real finish.
pub fn transcript_presented_plan(path: &str) -> bool {
    let records = tail_records(path);
    let last = match records.last() {
        Some(v) => v,
        None => return false,
    };
    let msg = last.get("message");
    if msg
        .and_then(|m| m.get("role"))
        .and_then(|x| x.as_str())
        .or_else(|| last.get("type").and_then(|x| x.as_str()))
        != Some("assistant")
    {
        return false;
    }
    match msg.and_then(|m| m.get("content")) {
        Some(Value::Array(arr)) => arr.iter().any(|b| {
            b.get("type").and_then(|x| x.as_str()) == Some("tool_use")
                && b.get("name").and_then(|x| x.as_str()) == Some("ExitPlanMode")
        }),
        _ => false,
    }
}

/// The deliverable text of the last assistant turn (uncapped) — used to populate a review's text when
/// the plan-file/ExitPlanMode capture hook was missed. Prefers the plan an `ExitPlanMode` presented
/// (for a plan-mode review that IS the full review), else the message's text blocks. `None` if empty.
pub fn final_assistant_message(path: &str) -> Option<String> {
    for rec in tail_records(path).iter().rev() {
        let msg = rec.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if role != "assistant" {
            continue;
        }
        let (mut text, mut plan) = (String::new(), String::new());
        match msg.and_then(|m| m.get("content")) {
            Some(Value::String(s)) => text = s.clone(),
            Some(Value::Array(arr)) => {
                for b in arr {
                    match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                if !text.is_empty() {
                                    text.push_str("\n\n");
                                }
                                text.push_str(t);
                            }
                        }
                        "tool_use"
                            if b.get("name").and_then(|x| x.as_str()) == Some("ExitPlanMode") =>
                        {
                            if let Some(p) = b
                                .get("input")
                                .or_else(|| b.get("tool_input"))
                                .and_then(|i| i.get("plan"))
                                .and_then(|x| x.as_str())
                            {
                                plan = p.to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // The ExitPlanMode plan is the review's deliverable; fall back to the message's prose.
        let chosen = if !plan.trim().is_empty() { plan } else { text };
        let chosen = chosen.trim().to_string();
        if !chosen.is_empty() {
            return Some(chosen);
        }
    }
    None
}

/// Collapse whitespace runs (incl. newlines) into single spaces and cap the length, so a
/// progress line stays a single tidy line in the UI.
fn collapse(s: &str, max: usize) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > max {
        let truncated: String = one_line.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

fn extract_blocks(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut s = String::new();
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            s.push_str(t);
                            s.push('\n');
                        }
                    }
                    "tool_use" => {
                        let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
                        s.push_str(&format!("⏺ {name}\n"));
                    }
                    _ => {}
                }
            }
            s
        }
        _ => String::new(),
    }
}

/// Build the `claude` CLI args for a spawned session. `--strict-mcp-config` (with no `--mcp-config`)
/// loads ZERO MCP servers, so a freshly-created worktree never blocks at startup on the project/user
/// MCP "trust these servers?" prompt or a slow/broken MCP connection. Workers don't use MCP today
/// (see DESIGN.md / BACKLOG "live MCP tools" is deferred); if that changes, pass `--mcp-config <file>`
/// alongside this flag to allow only harmony-curated servers. Pure, for testability.
fn claude_args(prompt: &str, resume: Option<&str>, permission_mode: &str) -> Vec<String> {
    let mut args = vec![
        "--permission-mode".to_string(),
        permission_mode.to_string(),
        "--strict-mcp-config".to_string(),
    ];
    if let Some(id) = resume {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    args.push(prompt.to_string());
    args
}

fn spawn_claude(
    cwd: &str,
    prompt: &str,
    resume: Option<&str>,
    permission_mode: &str,
) -> Result<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)> {
    spawn_claude_with_env(cwd, prompt, resume, permission_mode, &[])
}

/// Spawn `claude` with extra environment variables (used by the proof session to point capture tools
/// at the shared toolchain + the per-ticket artifact dir). `env` entries are set on the child; a
/// `PATH` entry replaces the inherited PATH (the caller composes it including the inherited PATH).
fn spawn_claude_with_env(
    cwd: &str,
    prompt: &str,
    resume: Option<&str>,
    permission_mode: &str,
    env: &[(String, String)],
) -> Result<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    for a in claude_args(prompt, resume, permission_mode) {
        cmd.arg(a);
    }

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    Ok((pair.master, child))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dc(target: &str, anchor: &str, file: &str, line: i64, body: &str) -> DiffComment {
        DiffComment {
            id: 0,
            ticket_id: 1,
            file_path: file.into(),
            line,
            end_line: line,
            side: "new".into(),
            body: body.into(),
            status: "open".into(),
            created_at: 0,
            target: target.into(),
            anchor: anchor.into(),
        }
    }

    fn ticket(acceptance: &str) -> Ticket {
        Ticket {
            id: 1,
            jira_key: None,
            source: "local".into(),
            title: "T".into(),
            spec: "Goal: do the thing.".into(),
            status: crate::status::WAITING.into(),
            repo_id: Some(1),
            created_at: 0,
            updated_at: 0,
            todos: String::new(),
            pending_question: String::new(),
            planned: 1,
            drafting: 0,
            grilled: 1,
            acceptance_criteria: acceptance.into(),
            relevant_paths: String::new(),
            constraints: String::new(),
            reviewed: 0,
            reviewed_sha: String::new(),
            review_text: String::new(),
            ci_triaged_sha: String::new(),
            ci_fix_attempts: 0,
            ci_triage: String::new(),
            proposed_spec: String::new(),
            review_verdict: String::new(),
            review_findings: String::new(),
            judged_sha: String::new(),
            review_fix_attempts: 0,
            activity: String::new(),
            orchestrator_note: String::new(),
            orchestrator_seen: String::new(),
            restart_attempts: 0,
            proof: String::new(),
            proof_artifacts: String::new(),
            proof_sha: String::new(),
            proof_attempts: 0,
            conflict_fix_attempts: 0,
            conflict_fingerprint: String::new(),
            pr_number: 0,
            pr_url: String::new(),
            pr_state: String::new(),
            pr_is_draft: 0,
        }
    }

    #[test]
    fn claude_args_always_disable_mcp() {
        // Every spawned session must pass --strict-mcp-config so a fresh worktree can't hang on the
        // MCP trust prompt / a slow MCP connection at startup.
        let a = claude_args("do it", None, "bypassPermissions");
        assert!(
            a.iter().any(|s| s == "--strict-mcp-config"),
            "missing --strict-mcp-config: {a:?}"
        );
        assert_eq!(
            a.windows(2)
                .find(|w| w[0] == "--permission-mode")
                .map(|w| &w[1]),
            Some(&"bypassPermissions".to_string())
        );
        assert_eq!(
            a.last().unwrap(),
            "do it",
            "prompt must be the final positional arg"
        );
        assert!(!a.iter().any(|s| s == "--resume"));

        // Resume threads the id through and still disables MCP.
        let r = claude_args("go", Some("sess-123"), "plan");
        assert!(r.iter().any(|s| s == "--strict-mcp-config"));
        assert_eq!(
            r.windows(2).find(|w| w[0] == "--resume").map(|w| &w[1]),
            Some(&"sess-123".to_string())
        );
        assert_eq!(r.last().unwrap(), "go");
    }

    #[test]
    fn review_prompt_pre_pr_avoids_pr_skill() {
        // No PR yet → review the branch diff, prefer a working-diff skill, and never invoke `/review`.
        let t = ticket("must pass CI");
        assert!(t.pr_url.is_empty());
        let out = render_review_prompt(&t, None);
        assert!(
            out.contains("/code-review"),
            "should prefer the working-diff skill: {out}"
        );
        assert!(
            out.contains("Do NOT use the `/review` skill"),
            "should steer away from the PR skill pre-PR: {out}"
        );
        assert!(!out.contains("Run the `/review` skill"));
        assert!(out.contains("pre-PR sanity check"));
        // Shared scaffolding still present.
        assert!(out.contains("ExitPlanMode"));
        assert!(out.contains("must pass CI"));
    }

    #[test]
    fn review_prompt_post_pr_uses_pr_skill_with_url() {
        // A PR exists → run `/review` on it, and thread the PR URL through for context.
        let mut t = ticket("must pass CI");
        t.pr_url = "https://github.com/o/r/pull/42".into();
        let out = render_review_prompt(&t, None);
        assert!(
            out.contains("Run the `/review` skill on this branch's open pull request"),
            "should invoke the PR skill post-PR: {out}"
        );
        assert!(out.contains("https://github.com/o/r/pull/42"));
        assert!(!out.contains("Do NOT use the `/review` skill"));
        assert!(out.contains("re-review checks the latest changes"));
        assert!(out.contains("ExitPlanMode"));
    }

    #[test]
    fn review_prompt_incremental_scope_carries_across_both_variants() {
        // The "only the delta since <sha>" focus applies whether or not a PR exists.
        let pre = render_review_prompt(&ticket(""), Some("abc123"));
        assert!(pre.contains("git diff abc123..HEAD"));
        let mut t = ticket("");
        t.pr_url = "https://github.com/o/r/pull/7".into();
        let post = render_review_prompt(&t, Some("abc123"));
        assert!(post.contains("git diff abc123..HEAD"));
    }

    #[test]
    fn feedback_prompt_groups_by_surface() {
        let comments = vec![
            dc("general", "", "", 0, "rename the module"),
            dc("diff", "", "src/x.rs", 42, "off by one"),
            dc(
                "review",
                "the funnel is wrong",
                "",
                0,
                "disagree, see below",
            ),
            dc(
                "pr_comment",
                "alice: \"nit: naming\"",
                "",
                0,
                "ignore this one",
            ),
        ];
        let out = render_feedback_prompt(&comments, &ticket(""));
        assert!(out.contains("General comments:"));
        assert!(out.contains("- rename the module"));
        assert!(out.contains("On the diff:"));
        assert!(out.contains("`src/x.rs:42` (new): off by one"));
        assert!(out.contains("On your review:"));
        assert!(out.contains("re: \"the funnel is wrong\": disagree"));
        assert!(out.contains("On GitHub PR comments:"));
        assert!(out.contains("alice: \"nit: naming\" — ignore this one"));
    }

    #[test]
    fn feedback_prompt_includes_spec_reconcile_block() {
        let out = render_feedback_prompt(
            &[dc("general", "", "", 0, "x")],
            &ticket("Must support CSV export."),
        );
        assert!(out.contains("# Spec"));
        assert!(out.contains("Must support CSV export."));
        assert!(out.contains("do NOT silently"));
        assert!(out.contains("## Acceptance criteria"));
    }

    #[test]
    fn feedback_prompt_omits_spec_block_when_empty() {
        // A ticket whose composed spec is empty → no spec/reconcile block.
        let mut t = ticket("");
        t.spec = String::new();
        let out = render_feedback_prompt(&[dc("general", "", "", 0, "x")], &t);
        assert!(!out.contains("# Spec"));
        assert!(!out.contains("do NOT silently"));
    }

    fn write_transcript(lines: &[&str]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "harmony-transcript-{}-{n}.jsonl",
            std::process::id()
        ));
        std::fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    #[test]
    fn turn_state_finished_when_last_block_is_text() {
        let p = write_transcript(&[
            r#"{"message":{"role":"user","content":[{"type":"text","text":"do it"}]}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit"}]}}"#,
            r#"{"message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"Done — implementation complete."}]}}"#,
        ]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::Finished
        );
        assert_eq!(
            final_assistant_message(p.to_str().unwrap()).as_deref(),
            Some("Done — implementation complete.")
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn turn_state_working_when_last_message_has_pending_tool() {
        // Text preamble + a trailing tool_use → the turn is not done.
        let p = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"running tests"},{"type":"tool_use","name":"Bash"}]}}"#,
        ]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::Working
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn turn_state_waiting_on_ask_user_question() {
        let p = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"which one?"},{"type":"tool_use","name":"AskUserQuestion"}]}}"#,
        ]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::WaitingOnQuestion
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn turn_state_finished_when_review_exits_plan_mode() {
        // A plan-mode review ends by presenting its verdict via ExitPlanMode — that IS the finish,
        // even though it's a trailing tool_use. (This is the review-stuck case.)
        let p = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"Both green."},{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"Verdict: ship it. The change is correct."}}]}}"#,
        ]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::Finished
        );
        // …and the review text comes from the plan, not a text block.
        assert_eq!(
            final_assistant_message(p.to_str().unwrap()).as_deref(),
            Some("Verdict: ship it. The change is correct.")
        );
        // …and it's recognised as a definitive plan finish (watchdog review-recovery gate).
        assert!(transcript_presented_plan(p.to_str().unwrap()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn presented_plan_false_for_trailing_text() {
        // A mid-investigation narration is NOT a definitive finish — the watchdog must not recover a
        // review on it (that was the partial-capture bug).
        let p = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"Let me confirm what the CI gate runs."}]}}"#,
        ]);
        assert!(!transcript_presented_plan(p.to_str().unwrap()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn turn_state_working_when_last_record_is_tool_result() {
        let p = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"message":{"role":"user","content":[{"type":"tool_result","content":"output"}]}}"#,
        ]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::Working
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn structured_transcript_parses_typed_blocks() {
        // A full conversation: user prompt → assistant text + tool_use → tool_result → final text.
        let p = write_transcript(&[
            r#"{"message":{"role":"user","content":[{"type":"text","text":"add a flag"}]}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"On it."},{"type":"tool_use","id":"tu_1","name":"Edit","input":{"file_path":"src/session.rs"}}]}}"#,
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"applied"}],"is_error":false}]}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#,
        ]);
        let msgs = structured_transcript(p.to_str().unwrap()).unwrap();
        assert_eq!(msgs.len(), 4);

        // 1) user prose
        assert_eq!(msgs[0].role, "user");
        assert_eq!(
            msgs[0].blocks,
            vec![TranscriptBlock::Text {
                text: "add a flag".into()
            }]
        );

        // 2) assistant text + tool_use, with the file path summarised from the input.
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(
            msgs[1].blocks,
            vec![
                TranscriptBlock::Text {
                    text: "On it.".into()
                },
                TranscriptBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "Edit".into(),
                    summary: "src/session.rs".into(),
                },
            ]
        );

        // 3) tool_result keyed back to the tool_use, text extracted from the array form.
        assert_eq!(msgs[2].role, "user");
        assert_eq!(
            msgs[2].blocks,
            vec![TranscriptBlock::ToolResult {
                tool_use_id: "tu_1".into(),
                output: "applied".into(),
                is_error: false,
            }]
        );

        // 4) final assistant text
        assert_eq!(
            msgs[3].blocks,
            vec![TranscriptBlock::Text {
                text: "Done.".into()
            }]
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn view_state_distinguishes_exit_plan_from_finished() {
        // ExitPlanMode is its own bucket for the friendly view (auto-switch to terminal)…
        let plan = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"Here's the plan."},{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"do x"}}]}}"#,
        ]);
        assert_eq!(
            transcript_view_state(plan.to_str().unwrap()),
            ViewState::ExitPlan
        );
        // …while a plain trailing text block is an ordinary finish (a friendly steer point).
        let done = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"All done."}]}}"#,
        ]);
        assert_eq!(
            transcript_view_state(done.to_str().unwrap()),
            ViewState::Finished
        );
        // …a pending tool call is still Working, and an AskUserQuestion is WaitingOnQuestion.
        let working = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}"#,
        ]);
        assert_eq!(
            transcript_view_state(working.to_str().unwrap()),
            ViewState::Working
        );
        let asking = write_transcript(&[
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"AskUserQuestion"}]}}"#,
        ]);
        assert_eq!(
            transcript_view_state(asking.to_str().unwrap()),
            ViewState::WaitingOnQuestion
        );
        for p in [plan, done, working, asking] {
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn turn_state_working_on_missing_or_empty_transcript() {
        assert_eq!(
            transcript_turn_state("/no/such/file.jsonl"),
            TurnState::Working
        );
        let p = write_transcript(&[""]);
        assert_eq!(
            transcript_turn_state(p.to_str().unwrap()),
            TurnState::Working
        );
        assert_eq!(final_assistant_message(p.to_str().unwrap()), None);
        let _ = std::fs::remove_file(&p);
    }
}
