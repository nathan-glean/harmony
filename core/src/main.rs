//! `harmony` CLI — exercises the Phase 1 core headlessly (no UI yet).
//!
//!   harmony repo add <name> <path> [--project KEY]
//!   harmony repo list
//!   harmony ticket add --title T [--key JIRA-1] [--spec ...] [--repo NAME]
//!   harmony ticket list
//!   harmony start <ticket_id> [--repo NAME] [--port 8787]   # spawn/resume + supervise
//!   harmony serve [--port 8787]                             # hook server only (debug)

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use portable_pty::PtySize;

use harmony_core::hooks;
use harmony_core::session::{SessionHandle, SessionManager};
use harmony_core::store::Store;

#[derive(Parser)]
#[command(name = "harmony", about = "harmony core (Phase 1, headless)")]
struct Cli {
    /// SQLite DB path (default: ~/.harmony/harmony.db)
    #[arg(long, global = true)]
    db: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage registered repositories
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Manage tickets
    Ticket {
        #[command(subcommand)]
        cmd: TicketCmd,
    },
    /// Start or resume a supervised Claude session for a ticket
    Start {
        ticket_id: i64,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Skip the Jira "→ In Progress" writeback on start
        #[arg(long)]
        no_jira: bool,
    },
    /// Run only the hook server (debug)
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// List all Claude Code sessions harmony has run
    Sessions {
        /// Delete all ended sessions instead of listing
        #[arg(long)]
        clear_ended: bool,
    },
    /// List worktrees, or delete one with --delete <id>
    Worktrees {
        #[arg(long)]
        delete: Option<i64>,
    },
    /// Jira Cloud integration
    Jira {
        #[command(subcommand)]
        cmd: JiraCmd,
    },
    /// Push the ticket's branch and open a PR ready for review (+ optional Jira writeback)
    Pr {
        ticket_id: i64,
        #[arg(long)]
        title: Option<String>,
        /// Skip Jira writeback (status → In Review + PR-link comment)
        #[arg(long)]
        no_writeback: bool,
        /// Skip the Claude-generated diff summary; use the spec as the PR body
        #[arg(long)]
        no_summary: bool,
    },
}

#[derive(Subcommand)]
enum JiraCmd {
    /// Install the Atlassian CLI via Homebrew (macOS)
    Install,
    /// Log in via the Atlassian CLI (opens a browser; no app registration / API key)
    Login,
    /// Log out of the Atlassian CLI
    Logout,
    /// Show whether Jira (acli) is connected and to which site
    Status,
    /// Sync assigned-to-me issues into the board
    Sync,
}

#[derive(Subcommand)]
enum RepoCmd {
    /// Register a local git repo
    Add {
        name: String,
        path: String,
        /// Default repo for a Jira project key (e.g. PROJ)
        #[arg(long)]
        project: Option<String>,
    },
    /// List registered repos
    List,
    /// Rename a repo
    Rename { id: i64, name: String },
}

#[derive(Subcommand)]
enum TicketCmd {
    /// Create a ticket (local, or linked to Jira via --key)
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        spec: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// List tickets
    List,
    /// Delete a ticket (removes its worktrees + record; not the Jira issue)
    Delete { id: i64 },
    /// Move a ticket to a column: todo | working | waiting | in_review | done
    Move { id: i64, status: String },
}

fn default_db() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.harmony/harmony.db")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(default_db);
    let store = Arc::new(Store::open(&db).await?);

    match cli.cmd {
        Cmd::Repo { cmd } => match cmd {
            RepoCmd::Add {
                name,
                path,
                project,
            } => {
                let id = store.add_repo(&name, &path, project.as_deref()).await?;
                println!("added repo #{id}: {name}");
            }
            RepoCmd::List => {
                for r in store.list_repos().await? {
                    println!(
                        "#{:<3} {:<16} {}  [{}]",
                        r.id,
                        r.name,
                        r.path,
                        r.default_project_key.unwrap_or_default()
                    );
                }
            }
            RepoCmd::Rename { id, name } => {
                store.rename_repo(id, &name).await?;
                println!("renamed repo #{id} → {name}");
            }
        },
        Cmd::Ticket { cmd } => match cmd {
            TicketCmd::Add {
                title,
                key,
                spec,
                repo,
            } => {
                let repo_id = match repo {
                    Some(n) => Some(
                        store
                            .get_repo_by_name(&n)
                            .await?
                            .ok_or_else(|| anyhow!("no repo named {n}"))?
                            .id,
                    ),
                    None => None,
                };
                let source = if key.is_some() { "jira" } else { "local" };
                let id = store
                    .add_ticket(
                        key.as_deref(),
                        source,
                        &title,
                        spec.as_deref().unwrap_or(""),
                        repo_id,
                    )
                    .await?;
                println!("added ticket #{id}");
            }
            TicketCmd::List => {
                for t in store.list_tickets().await? {
                    println!(
                        "#{:<3} {:<10} {:<10} {}",
                        t.id,
                        t.status,
                        t.jira_key.unwrap_or_else(|| "(local)".into()),
                        t.title
                    );
                }
            }
            TicketCmd::Delete { id } => {
                harmony_core::worktree::cleanup_for_ticket(&store, id).await;
                store.delete_ticket(id).await?;
                println!("deleted ticket #{id}");
            }
            TicketCmd::Move { id, status } => {
                if !harmony_core::status::is_valid(&status) {
                    anyhow::bail!(
                        "invalid status '{status}' (todo|working|waiting|in_review|done)"
                    );
                }
                store.set_ticket_status(id, &status).await?;
                println!("moved #{id} → {status}");
            }
        },
        Cmd::Serve { port } => {
            hooks::serve_forever(store, port, None).await?;
        }
        Cmd::Sessions { clear_ended } => {
            if clear_ended {
                let n = store.delete_ended_sessions().await?;
                println!("cleared {n} ended session(s)");
                return Ok(());
            }
            for s in store.list_sessions().await? {
                let state = if s.ended_at.is_some() {
                    "ended"
                } else {
                    s.state.as_str()
                };
                let key = s
                    .jira_key
                    .clone()
                    .unwrap_or_else(|| format!("local#{}", s.ticket_id));
                let sid = s
                    .claude_session_id
                    .as_deref()
                    .map(|c| &c[..8.min(c.len())])
                    .unwrap_or("-");
                println!(
                    "#{:<3} {:<8} {:<12} {}  [{}]",
                    s.id, state, key, s.ticket_title, sid
                );
            }
        }
        Cmd::Worktrees { delete } => {
            if let Some(id) = delete {
                if let Some(wt) = store.get_worktree(id).await? {
                    if let Some(repo) = store.get_repo(wt.repo_id).await? {
                        let _ = harmony_core::worktree::remove(
                            &repo.path,
                            std::path::Path::new(&wt.path),
                        );
                    }
                }
                store.delete_worktree(id).await?;
                println!("deleted worktree #{id}");
                return Ok(());
            }
            for w in store.list_worktrees().await? {
                let key = w
                    .jira_key
                    .clone()
                    .unwrap_or_else(|| format!("local#{}", w.ticket_id));
                println!(
                    "#{:<3} {:<12} {:<10} {:<28} {}",
                    w.id, key, w.repo_name, w.branch, w.path
                );
            }
        }
        Cmd::Start {
            ticket_id,
            repo,
            port,
            no_jira,
        } => {
            start_flow(store, ticket_id, repo, port, no_jira).await?;
        }
        Cmd::Jira { cmd } => match cmd {
            JiraCmd::Install => {
                let v = harmony_core::jira::install_via_brew()?;
                println!("installed {v}");
            }
            JiraCmd::Login => {
                harmony_core::jira::login_passthrough()?;
                match harmony_core::jira::connected_site().await {
                    Some(site) => println!("connected: {site}"),
                    None => println!("login finished, but acli reports not connected"),
                }
            }
            JiraCmd::Logout => {
                harmony_core::jira::logout().await?;
                println!("disconnected from Jira");
            }
            JiraCmd::Status => match harmony_core::jira::connected_site().await {
                Some(site) => println!("connected: {site}"),
                None => println!("not connected — run `harmony jira login`"),
            },
            JiraCmd::Sync => {
                let issues = harmony_core::jira::search_assigned().await?;
                let (mut new, mut upd) = (0, 0);
                for issue in &issues {
                    let (_, inserted) =
                        store.upsert_jira_ticket(&issue.key, &issue.summary).await?;
                    if inserted {
                        new += 1;
                    } else {
                        upd += 1;
                    }
                    println!("  {} {}", issue.key, issue.summary);
                }
                println!("synced {} issue(s): {new} new, {upd} updated", issues.len());
            }
        },
        Cmd::Pr {
            ticket_id,
            title,
            no_writeback,
            no_summary,
        } => {
            pr_flow(&store, ticket_id, title, !no_writeback, !no_summary).await?;
        }
    }
    Ok(())
}

/// A reference to the originating Jira issue for the PR body — a browse URL when the connected
/// site is known, else the bare key. `None` for local tickets.
async fn jira_ref(ticket: &harmony_core::models::Ticket) -> Option<String> {
    let key = ticket.jira_key.as_deref()?;
    match harmony_core::jira::connected_site().await {
        Some(site) => Some(format!("https://{site}/browse/{key}")),
        None => Some(key.to_string()),
    }
}

/// Push the branch, open a PR ready for review, set ticket → In Review, and (opt-in) write back to
/// Jira (status + PR-link comment). DESIGN Q7/Q11. With `summary`, the PR body is a
/// Claude-generated diff summary (else the composed spec).
async fn pr_flow(
    store: &Store,
    ticket_id: i64,
    title: Option<String>,
    writeback: bool,
    summary: bool,
) -> Result<()> {
    let ticket = store
        .get_ticket(ticket_id)
        .await?
        .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
    let wt = store
        .primary_worktree_for_ticket(ticket_id)
        .await?
        .ok_or_else(|| {
            anyhow!("ticket #{ticket_id} has no worktree — run `harmony start` first")
        })?;
    let repo = store
        .get_repo(wt.repo_id)
        .await?
        .ok_or_else(|| anyhow!("repo #{} missing", wt.repo_id))?;

    let pr_title = title.unwrap_or_else(|| ticket.title.clone());
    let composed = harmony_core::spec::compose_spec(&ticket);
    let fallback = if composed.trim().is_empty() {
        format!("Ticket: {}", ticket.title)
    } else {
        composed
    };
    let body = if summary {
        let tref = jira_ref(&ticket).await;
        println!("[pr] summarizing diff via `claude -p` …");
        harmony_core::github::generated_pr_body(&wt.path, &repo.path, tref.as_deref(), &fallback)
    } else {
        fallback
    };

    println!("[pr] pushing {} …", wt.branch);
    harmony_core::github::push_branch(&wt.path, &wt.branch)?;
    println!("[pr] opening PR …");
    let url = harmony_core::github::create_pr(&wt.path, &pr_title, &body, &wt.branch)?;
    store
        .set_ticket_status(ticket_id, harmony_core::status::IN_REVIEW)
        .await?;
    println!("[pr] {url}");

    if writeback {
        if let Some(key) = ticket.jira_key.as_deref() {
            match harmony_core::jira::transition(key, "In Review").await {
                Ok(()) => println!("[jira] {key} → In Review"),
                Err(e) => eprintln!("[jira] transition skipped: {e}"),
            }
            match harmony_core::jira::add_comment(key, &format!("PR opened by harmony: {url}"))
                .await
            {
                Ok(()) => println!("[jira] PR link commented on {key}"),
                Err(e) => eprintln!("[jira] comment skipped: {e}"),
            }
        }
    }
    Ok(())
}

async fn start_flow(
    store: Arc<Store>,
    ticket_id: i64,
    repo: Option<String>,
    port: u16,
    no_jira: bool,
) -> Result<()> {
    let ticket = store
        .get_ticket(ticket_id)
        .await?
        .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;

    // Opt-in Jira writeback: → In Progress when work starts (DESIGN Q7). Best-effort.
    if !no_jira {
        if let Some(key) = ticket.jira_key.as_deref() {
            match harmony_core::jira::transition(key, "In Progress").await {
                Ok(()) => println!("[jira] {key} → In Progress"),
                Err(e) => eprintln!("[jira] transition skipped: {e}"),
            }
        }
    }

    // Resolve a repo if the ticket doesn't have one yet: explicit --repo, else the
    // default repo for the Jira project key (DESIGN Q8).
    if ticket.repo_id.is_none() {
        let repo_id = if let Some(name) = repo {
            store
                .get_repo_by_name(&name)
                .await?
                .ok_or_else(|| anyhow!("no repo named {name}"))?
                .id
        } else if let Some(key) = ticket.jira_key.as_deref().and_then(|k| k.split('-').next()) {
            store
                .default_repo_for_key(key)
                .await?
                .ok_or_else(|| anyhow!("no default repo for project {key}; pass --repo NAME"))?
                .id
        } else {
            return Err(anyhow!("ticket has no repo; pass --repo NAME"));
        };
        store.set_ticket_repo(ticket_id, repo_id).await?;
    }

    hooks::spawn_server(store.clone(), port, None).await?;
    println!("[harmony] hook server on http://127.0.0.1:{port}");

    let mgr = SessionManager::new(store.clone(), port);
    let handle = mgr.start(ticket_id).await?;
    println!(
        "[harmony] session #{} started — bridging terminal (type to interact; /exit to finish)\n",
        handle.session_id
    );

    bridge_and_wait(&mgr, handle).await?;
    Ok(())
}

/// Restores the terminal to cooked mode on drop, even on early return.
struct RawGuard;
impl RawGuard {
    fn enable() -> Self {
        let _ = crossterm::terminal::enable_raw_mode();
        RawGuard
    }
}
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Bridge this terminal to the session PTY and wait for the child to exit, then mark
/// the session ended (process-exit = session end, per Phase 0 findings).
///
/// The controlling terminal is put into raw mode so escape sequences (arrows,
/// Shift+Tab, Ctrl-C, etc.) pass straight through to Claude instead of being
/// interpreted/echoed by the shell. (The Phase 3 UI does this via xterm.js.)
async fn bridge_and_wait(mgr: &SessionManager, handle: SessionHandle) -> Result<()> {
    let SessionHandle {
        session_id,
        master,
        child,
    } = handle;

    // Size the PTY to the real terminal, then go raw for the duration of the bridge.
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    let _raw = RawGuard::enable();

    // PTY -> stdout (detached; must not be joined or shutdown can hang)
    let mut reader = master.try_clone_reader()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut stdout = std::io::stdout();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&buf[..n]);
                    let _ = stdout.flush();
                }
            }
        }
    });

    // stdin -> PTY
    let mut writer = master.take_writer()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = writer.write_all(&buf[..n]);
                    let _ = writer.flush();
                }
            }
        }
    });

    let status = tokio::task::spawn_blocking(move || {
        let mut child = child;
        child.wait()
    })
    .await??;

    mgr.end_session(session_id).await?;
    drop(master);
    println!("\n[harmony] session #{session_id} ended ({status:?})");
    Ok(())
}
