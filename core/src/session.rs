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
    /// changes against the spec and prints suggestions, then ends (the executor stops it on the
    /// review session's Stop). Plan mode keeps it strictly read-only — it never edits.
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

        let prompt = render_review_prompt(&ticket);
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

/// Opening prompt for a `/review` session (pre-PR human-review sanity check): run the project's
/// review skill over this branch's changes and surface concrete suggestions for the user to read
/// before they open a PR. Read-only (plan mode).
fn render_review_prompt(t: &Ticket) -> String {
    format!(
        "Run the `/review` skill on the changes this branch makes versus its base. Review against \
         the ticket's intent below, and produce a concise, prioritised list of concerns \
         (correctness, edge cases, missing tests, scope creep) and any concrete fixes you'd \
         suggest — this is a pre-PR sanity check for the human. Write your full review to your \
         plan file as a single, complete document (this is how the review is surfaced to the \
         human). Do not make any edits to the repo.\n\n\
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
}
