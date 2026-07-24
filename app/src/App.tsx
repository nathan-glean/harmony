import { useEffect, useState, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import { onAction } from "@tauri-apps/plugin-notification";
import { Board } from "./components/Board";
import { Sessions } from "./components/Sessions";
import { Orchestrator } from "./components/Orchestrator";
import { Worktrees } from "./components/Worktrees";
import { Settings } from "./components/Settings";
import { DiffPane } from "./components/DiffPane";
import { JiraInfo } from "./components/JiraInfo";
import { Tasks } from "./components/Tasks";
import { QuestionCard } from "./components/QuestionCard";
import { TranscriptPane } from "./components/TranscriptPane";
import { ProgressLine } from "./components/ProgressLine";
import { TerminalView } from "./components/Terminal";
import { FriendlySession } from "./components/FriendlySession";
import { SpecEditor } from "./components/SpecEditor";
import { ProofPane } from "./components/ProofPane";
import { PrComments } from "./components/PrComments";
import { ReviewFeedback } from "./components/ReviewFeedback";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { api } from "./api";
import type { Ticket, Repo, SessionView, WorktreeView, PendingQuestion, SessionProgress, SessionExit, PrDone, SessionViewMode } from "./types";
import { parseActivity } from "./types";

export function App() {
  const [view, setView] = useState<
    "board" | "sessions" | "orchestrator" | "worktrees" | "settings"
  >("board");
  const [tickets, setTickets] = useState<Ticket[]>([]);
  const [sessions, setSessions] = useState<SessionView[]>([]);
  const [worktrees, setWorktrees] = useState<WorktreeView[]>([]);
  const [liveSessionIds, setLiveSessionIds] = useState<Set<number>>(new Set());
  // Tickets whose PR is being created in the background (show a loading indicator).
  const [openingPr, setOpeningPr] = useState<Set<number>>(new Set());
  // Active tab in the ticket modal.
  const [tab, setTab] = useState<"description" | "spec" | "proof" | "review" | "session">("spec");
  // How a live session renders: the friendly chat-style GUI or the raw terminal. Global +
  // persisted (default Friendly); the last user choice is restored on next launch.
  const [viewMode, setViewMode] = useState<SessionViewMode>("friendly");
  // Sub-tab within the Review tab (Diff + PR comments are nested under Review).
  const [reviewSub, setReviewSub] = useState<"review" | "diff" | "pr">("review");
  // Bumped whenever review feedback is added/sent, so the feedback list reloads across components.
  const [fbVersion, setFbVersion] = useState(0);
  // Open-feedback count + send state for the always-visible "Send to Claude" button in the subtabs.
  const [openFeedback, setOpenFeedback] = useState(0);
  const [sendingFeedback, setSendingFeedback] = useState(false);
  const [selected, setSelected] = useState<Ticket | null>(null);
  // Live terminals keyed by ticket id → session id (supports several at once).
  const [liveSessions, setLiveSessions] = useState<Record<number, number>>({});
  // Live in-session progress (last assistant message + current tool), keyed by ticket id.
  const [progress, setProgress] = useState<Record<number, SessionProgress>>({});
  // Attention notifications: track prior session states to fire on entry into "waiting".
  const prevStates = useRef<Map<number, string>>(new Map());
  const seeded = useRef(false);
  const lastAttention = useRef<number | null>(null);
  const autoSyncing = useRef(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [jiraSite, setJiraSite] = useState<string | null>(null);
  const [acliInstalled, setAcliInstalled] = useState(true);
  const [showConnect, setShowConnect] = useState(false);
  const [repos, setRepos] = useState<Repo[]>([]);
  const [showNew, setShowNew] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  const [newSpec, setNewSpec] = useState("");
  const [newRepo, setNewRepo] = useState("");
  const [toast, setToast] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const tks = await api.listTickets();
      setTickets(tks);
      const sess = await api.listSessions();
      setSessions(sess);
      setWorktrees(await api.listWorktrees());
      setRepos(await api.listRepos());
      // Backend is the source of truth for what's actually live.
      const live = await api.liveSessions();
      setLiveSessions(Object.fromEntries(live.map(([tid, sid]) => [tid, sid])));
      setLiveSessionIds(new Set(live.map(([, sid]) => sid)));

      // Richer in-session progress tailed from each live transcript (board/detail line).
      const prog = await api.liveProgress();
      setProgress(Object.fromEntries(prog.map((p) => [p.ticket_id, p])));

      // Track the most recent ticket to enter "waiting" so a clicked notification can focus it.
      // The desktop notification itself is fired by the backend on the activity → "waiting on you"
      // transition (the single owner of "needs you" alerts — covers every reason, not just a
      // session going idle). Skip the first pass so we don't seed on launch.
      const liveSess = sess.filter((s) => !s.ended_at);
      for (const s of liveSess) {
        const prev = prevStates.current.get(s.id);
        if (seeded.current && s.state === "waiting" && prev !== "waiting") {
          lastAttention.current = s.ticket_id;
        }
      }
      const next = new Map<number, string>();
      liveSess.forEach((s) => next.set(s.id, s.state));
      prevStates.current = next;
      seeded.current = true;

      // (Grill→work handoff is now owned by the backend flow executor via the hook event bus —
      // the frontend no longer auto-stops the grill or auto-starts work on the drafting flip.)
    } catch (e) {
      console.error(e);
    }
  }, []);


  // Open (and start/resume if needed) a live terminal for a ticket.
  const openTerminal = async (ticket: Ticket) => {
    setSelected(ticket);
    setView("board");
    if (liveSessions[ticket.id]) return; // already attached in this run
    const sid = await api.startSession(ticket.id, null);
    setLiveSessions((m) => ({ ...m, [ticket.id]: sid }));
    await refresh();
  };

  // Switch the session view. A user click persists the choice globally; the auto-switch escape
  // hatch (persist=false) flips the view for this run without overwriting the saved preference.
  const changeViewMode = useCallback((mode: SessionViewMode, persist = true) => {
    setViewMode(mode);
    if (persist) api.setSessionViewMode(mode).catch(() => {});
  }, []);

  const clearEndedSessions = async () => {
    try {
      const n = await api.clearEndedSessions();
      await refresh();
      flash(`Cleared ${n} ended session(s)`);
    } catch (e) {
      flash(String(e));
    }
  };

  const deleteWorktreeSessions = async (worktreeId: number) => {
    try {
      await api.deleteWorktreeSessions(worktreeId);
      await refresh();
    } catch (e) {
      flash(String(e));
    }
  };

  const addRepo = async (name: string, path: string, project: string | null) => {
    try {
      await api.addRepo(name, path, project);
      await refresh();
      flash(`Added repo ${name}`);
    } catch (e) {
      flash(String(e));
    }
  };

  const renameRepo = async (id: number, name: string) => {
    try {
      await api.renameRepo(id, name);
      await refresh();
    } catch (e) {
      flash(String(e));
    }
  };

  const deleteRepo = async (id: number) => {
    const ok = await confirm("Remove this repo from harmony? Your files on disk are untouched.", {
      title: "Remove repo",
      kind: "warning",
    });
    if (!ok) return;
    try {
      await api.deleteRepo(id);
      await refresh();
    } catch (e) {
      flash(String(e));
    }
  };

  // Run a destructive worktree op that may refuse with a "DIRTY: …" error when there are
  // uncommitted changes. On refusal, confirm discarding the work and retry forced. Returns
  // false if the user cancelled. Non-DIRTY errors propagate to the caller.
  const withDirtyConfirm = async (
    action: (force: boolean) => Promise<unknown>,
    what: string
  ): Promise<boolean> => {
    try {
      await action(false);
      return true;
    } catch (e) {
      const msg = String(e);
      const i = msg.indexOf("DIRTY:");
      if (i === -1) throw e;
      const ok = await confirm(
        `${what} has uncommitted changes (${msg.slice(i + 6).trim()}). Discard and continue?`,
        { title: "Uncommitted changes", kind: "warning" }
      );
      if (!ok) return false;
      await action(true);
      return true;
    }
  };

  const deleteWorktree = async (w: WorktreeView) => {
    try {
      const dirty = await api.worktreeDirty(w.id);
      const ok = await confirm(
        dirty
          ? `Worktree ${w.branch} has uncommitted changes. Discard them and delete?`
          : `Delete worktree ${w.branch}? Removes it from disk and forgets its sessions.`,
        { title: "Delete worktree", kind: "warning" }
      );
      if (!ok) return;
      await api.deleteWorktree(w.id, dirty);
      await refresh();
      flash("Worktree deleted");
    } catch (e) {
      flash(String(e));
    }
  };

  // From the Sessions table: attach to a live one, else just open its ticket.
  const openTicketFromSession = async (ticketId: number, live: boolean) => {
    const t = await api.getTicket(ticketId);
    if (!t) return;
    if (live) {
      try {
        await openTerminal(t);
      } catch (e) {
        flash(String(e));
      }
    } else {
      setSelected(t);
      setView("board");
    }
  };

  const refreshJira = useCallback(async () => {
    try {
      const env = await api.jiraEnv();
      setAcliInstalled(env.acli_installed);
      setJiraSite(env.site);
      return env;
    } catch {
      return null;
    }
  }, []);

  // Poll the board for live state (hooks update it out-of-band).
  useEffect(() => {
    refresh();
    refreshJira();
    api.listRepos().then(setRepos).catch(() => {});
    const t = setInterval(refresh, 1500);
    return () => clearInterval(t);
  }, [refresh, refreshJira]);

  // Periodically pull assigned Jira issues while connected (silent, non-overlapping).
  // Runs once on connect/launch, then every 60s. Manual "Sync Jira" still works.
  useEffect(() => {
    if (!jiraSite) return;
    const tick = async () => {
      if (autoSyncing.current) return;
      autoSyncing.current = true;
      try {
        await api.jiraSync();
        await refresh();
      } catch {
        /* not connected / acli unavailable — ignore */
      } finally {
        autoSyncing.current = false;
      }
    };
    const id = setInterval(tick, 60000);
    tick();
    return () => clearInterval(id);
  }, [jiraSite, refresh]);

  // Restore the persisted Friendly/Terminal preference on launch (default Friendly on first use).
  useEffect(() => {
    api.getSessionViewMode().then(setViewMode).catch(() => {});
  }, []);

  // Reattach on launch: resume sessions that were live when the app last closed.
  useEffect(() => {
    api
      .pendingReattach()
      .then(async (ids) => {
        for (const id of ids) {
          try {
            const sid = await api.startSession(id, null);
            setLiveSessions((m) => ({ ...m, [id]: sid }));
          } catch {
            /* couldn't reattach this one */
          }
        }
        if (ids.length) refresh();
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Clicking an attention notification → focus the app and open that ticket. Its live
  // terminal is already attached (kept current by the poll), so it just shows.
  useEffect(() => {
    let listener: { unregister: () => void } | undefined;
    onAction(async () => {
      const tid = lastAttention.current;
      if (tid == null) return;
      try {
        await getCurrentWindow().setFocus();
      } catch {
        /* ignore */
      }
      const t = await api.getTicket(tid);
      if (t) {
        setSelected(t);
        setView("board");
      }
    })
      .then((l) => {
        listener = l;
      })
      .catch(() => {});
    return () => listener?.unregister();
  }, []);

  // A finished session clears the terminal.
  useEffect(() => {
    const un = listen<SessionExit>("session-exit", (e) => {
      const { session_id, ticket_id, ok, code } = e.payload;
      setLiveSessions((m) => {
        const entry = Object.entries(m).find(([, sid]) => sid === session_id);
        if (!entry) return m;
        const next = { ...m };
        delete next[Number(entry[0])];
        return next;
      });
      // Abnormal exit (a crash, not a user stop) — surface it; the session also shows an
      // 'error' badge in the Sessions view.
      if (!ok) flash(`Session for #${ticket_id} exited unexpectedly (code ${code})`);
      refresh();
    });
    return () => {
      un.then((u) => u());
    };
  }, [refresh]);

  // Background PR creation: show a loading indicator on the card while it's opening, and revert
  // (backend already moved it back to Human Review) + toast on failure.
  useEffect(() => {
    const unOpening = listen<number>("pr-opening", (e) => {
      const id = e.payload;
      setOpeningPr((s) => new Set(s).add(id));
      // Optimistically jump the card to the PR column (the backend already set it).
      setTickets((ts) => ts.map((t) => (t.id === id ? { ...t, status: "in_review" } : t)));
    });
    const unDone = listen<PrDone>("pr-done", (e) => {
      const { ticket_id, ok, error } = e.payload;
      setOpeningPr((s) => {
        const next = new Set(s);
        next.delete(ticket_id);
        return next;
      });
      if (!ok) flash(error ?? `Opening the PR for #${ticket_id} failed`);
      refresh();
    });
    return () => {
      unOpening.then((u) => u());
      unDone.then((u) => u());
    };
  }, [refresh]);

  // Surface errors that React error boundaries can't catch — those thrown in event handlers,
  // timers, and unhandled promise rejections (e.g. an xterm/WebGL failure in an async callback, or
  // a rejected Tauri command). Without this they'd vanish silently or, worse, leave the UI wedged.
  useEffect(() => {
    const onError = (e: ErrorEvent) => {
      console.error("Uncaught error:", e.error ?? e.message);
      flash(`Error: ${e.error?.message ?? e.message}`);
    };
    const onRejection = (e: PromiseRejectionEvent) => {
      console.error("Unhandled rejection:", e.reason);
      flash(`Error: ${e.reason?.message ?? String(e.reason)}`);
    };
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);
    return () => {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
    };
  }, []);

  // Esc closes the ticket modal.
  useEffect(() => {
    if (!selected) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSelected(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [selected]);

  // On opening a ticket (or switching to a different one), pick a sensible default tab: the live
  // Session if one's running, else the Jira Description, else the Spec.
  useEffect(() => {
    if (!selected) return;
    setTab(liveSessions[selected.id] ? "session" : selected.jira_key ? "description" : "spec");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.id]);

  // Count of open feedback comments for the selected ticket (drives the always-visible
  // "Send to Claude" button in the Review sub-tab strip). Reloads on feedback changes (fbVersion)
  // and on sub-tab switches (so diff comments added in the Diff sub-tab are reflected).
  useEffect(() => {
    if (!selected) {
      setOpenFeedback(0);
      return;
    }
    let cancelled = false;
    api
      .listDiffComments(selected.id)
      .then((cs) => {
        if (!cancelled) setOpenFeedback(cs.filter((c) => c.status === "open").length);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.id, fbVersion, reviewSub]);

  const sendFeedbackToClaude = async () => {
    if (!selected) return;
    setSendingFeedback(true);
    try {
      await api.addressFeedback(selected.id);
      flash("Sending feedback to Claude…");
    } catch (e) {
      flash(String(e));
    } finally {
      setSendingFeedback(false);
      setFbVersion((v) => v + 1);
    }
  };

  const flash = (m: string) => {
    setToast(m);
    setTimeout(() => setToast(null), 4000);
  };

  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(label);
    try {
      await fn();
    } catch (e) {
      flash(String(e));
    } finally {
      setBusy(null);
    }
  };

  // Drag-and-drop: move a ticket to a column (optimistic, then persist).
  // Into "In Progress" → auto-open a session; out of it → stop any live session.
  // A column move is just a request to the backend flow executor: it decides (and may block or
  // redirect), starts/stops sessions, runs /review, opens/merges PRs, and cleans up worktrees.
  // The frontend only relays the request and re-renders. `withDirtyConfirm` handles the
  // discard-uncommitted-work confirmation when a Done/cleanup would lose changes.
  const moveTicket = async (id: number, status: string) => {
    run("move", async () => {
      try {
        await withDirtyConfirm(
          (force) => api.transitionTicket(id, status, force),
          "This ticket's worktree"
        );
      } catch (e) {
        // Blocked transition ("assign a repo first", "Done is terminal", "no changes to open a
        // PR for", …) or a real error — surface it; the board stays as the backend left it.
        flash(String(e));
      }
      await refresh();
    });
  };

  // The selected ticket, but kept fresh from the poll (so live todos/status update).
  const liveTicket = selected ? tickets.find((t) => t.id === selected.id) ?? selected : null;

  // Claude's pending AskUserQuestion for the selected ticket, parsed for the question card. Only
  // shown while the session is actually live — a question can't be answered once its PTY is gone, so
  // a stale one left by an ended/crashed session must not linger (the backend also clears it on exit).
  const pendingQuestion: PendingQuestion | null = (() => {
    if (!liveTicket?.pending_question) return null;
    if (!selected || !liveSessions[selected.id]) return null;
    try {
      return JSON.parse(liveTicket.pending_question);
    } catch {
      return null;
    }
  })();

  return (
    <div className="app">
      <header className="topbar">
        <div className="logo">harmony</div>
        <div className="nav">
          <button className={view === "board" ? "active" : ""} onClick={() => setView("board")}>
            Board
          </button>
          <button
            className={view === "sessions" ? "active" : ""}
            onClick={() => setView("sessions")}
          >
            Sessions
          </button>
          <button
            className={view === "orchestrator" ? "active" : ""}
            onClick={() => setView("orchestrator")}
          >
            Orchestrator
          </button>
          <button
            className={view === "worktrees" ? "active" : ""}
            onClick={() => setView("worktrees")}
          >
            Worktrees
          </button>
          <button
            className={view === "settings" ? "active" : ""}
            onClick={() => setView("settings")}
          >
            Settings
          </button>
        </div>
        <button onClick={() => setShowNew((v) => !v)}>+ New ticket</button>
        <div className="spacer" />
        {jiraSite ? (
          <>
            <span className="jira-site" title={jiraSite}>
              Jira: {jiraSite.replace(/^https?:\/\//, "")}
            </span>
            <button
              disabled={busy !== null}
              onClick={() =>
                run("sync", async () => {
                  const n = await api.jiraSync();
                  flash(`Synced ${n} Jira issue(s)`);
                  await refresh();
                })
              }
            >
              {busy === "sync" ? "Syncing…" : "Sync Jira"}
            </button>
            <button
              disabled={busy !== null}
              onClick={() => run("logout", async () => { await api.jiraLogout(); setJiraSite(null); })}
            >
              Disconnect
            </button>
          </>
        ) : (
          <button onClick={() => setShowConnect((v) => !v)}>Connect Jira</button>
        )}
      </header>

      {showNew && (
        <div className="connect">
          <p>New ticket — pick a repo, then Claude grills you to build the spec:</p>
          <input
            className="grow"
            placeholder="Title"
            value={newTitle}
            onChange={(e) => setNewTitle(e.target.value)}
          />
          <select value={newRepo} onChange={(e) => setNewRepo(e.target.value)}>
            <option value="" disabled>
              choose a repo (required)…
            </option>
            {repos.map((r) => (
              <option key={r.id} value={r.name}>
                {r.name}
              </option>
            ))}
          </select>
          <textarea
            className="spec"
            placeholder="Initial idea / seed for the interview (optional, markdown)…"
            value={newSpec}
            onChange={(e) => setNewSpec(e.target.value)}
          />
          <button
            disabled={!newTitle.trim() || !newRepo || busy !== null}
            onClick={() =>
              run("create", async () => {
                const id = await api.addLocalTicket(newTitle.trim(), newSpec, newRepo);
                setShowNew(false);
                setNewTitle("");
                setNewSpec("");
                setNewRepo("");
                // Kick off the grill via the flow executor, then open the ticket so the user
                // can answer the interview in the live terminal / question card.
                await api.grillTicket(id);
                await refresh();
                const t = await api.getTicket(id);
                if (t) setSelected(t);
                flash(`Drafting spec for #${id}…`);
              })
            }
          >
            {busy === "create" ? "Starting…" : "Create & build spec"}
          </button>
        </div>
      )}

      {showConnect && !jiraSite && (
        <div className="connect">
          {!acliInstalled ? (
            <>
              <p>
                harmony uses the <strong>Atlassian CLI (acli)</strong> for Jira — it's not
                installed. Install it with Homebrew, or copy the commands:
              </p>
              <pre className="cmd">
                brew tap atlassian/homebrew-acli{"\n"}brew install acli
              </pre>
              <button
                disabled={busy !== null}
                onClick={() =>
                  run("install", async () => {
                    const v = await api.installAcli();
                    await refreshJira();
                    flash(`Installed ${v || "acli"}`);
                  })
                }
              >
                {busy === "install" ? "Installing… (may take a minute)" : "Install with Homebrew"}
              </button>
              <a
                className="link"
                href="https://developer.atlassian.com/cloud/acli/guides/install-macos/"
                target="_blank"
                rel="noreferrer"
              >
                manual install
              </a>
              <button disabled={busy !== null} onClick={() => run("recheck", async () => { await refreshJira(); })}>
                Re-check
              </button>
            </>
          ) : (
            <>
              <p>
                acli is installed. In a terminal, run <code>acli jira auth login</code> once
                (opens a browser — no API key, no app registration), then re-check:
              </p>
              <button
                disabled={busy !== null}
                onClick={() =>
                  run("recheck", async () => {
                    const env = await refreshJira();
                    if (env?.site) {
                      setShowConnect(false);
                      flash(`Connected to ${env.site}`);
                    } else {
                      flash("Still not connected — run `acli jira auth login` first");
                    }
                  })
                }
              >
                {busy === "recheck" ? "Checking…" : "Re-check connection"}
              </button>
            </>
          )}
        </div>
      )}

      <div className="main">
        <ErrorBoundary
          title="Something went wrong — the app hit an unexpected error"
          showReload
        >
        {view === "settings" ? (
          <Settings repos={repos} onAdd={addRepo} onRename={renameRepo} onDelete={deleteRepo} />
        ) : view === "worktrees" ? (
          <Worktrees
            worktrees={worktrees}
            isLive={(tid) => !!liveSessions[tid]}
            onOpen={(tid) => openTicketFromSession(tid, false)}
            onDelete={deleteWorktree}
          />
        ) : view === "sessions" ? (
          <Sessions
            sessions={sessions}
            liveSessionIds={liveSessionIds}
            onOpen={openTicketFromSession}
            onClearEnded={clearEndedSessions}
            onDeleteGroup={deleteWorktreeSessions}
          />
        ) : view === "orchestrator" ? (
          <Orchestrator onOpen={(tid) => openTicketFromSession(tid, false)} />
        ) : (
        <>
        <Board
          tickets={tickets}
          selectedId={selected?.id ?? null}
          progress={progress}
          openingPr={openingPr}
          onSelect={setSelected}
          onMove={moveTicket}
        />

        {selected && (
          <div
            className="modal-backdrop"
            onMouseDown={(e) => {
              if (e.target === e.currentTarget) setSelected(null);
            }}
          >
            <div className="modal">
              <ErrorBoundary resetKey={selected.id} onClose={() => setSelected(null)}>
              <div className="modal-head">
                <span className="badge">{selected.status}</span>
                {(() => {
                  const act = parseActivity((liveTicket ?? selected).activity);
                  return act && act.category !== "idle" ? (
                    <span className={"modal-activity act-" + act.category} title={act.detail ?? ""}>
                      {act.label}
                      {act.detail ? ` — ${act.detail}` : ""}
                    </span>
                  ) : null;
                })()}
                <strong>{selected.jira_key ?? `local #${selected.id}`}</strong>
                <span className="modal-title">{selected.title}</span>
                {selected.jira_key && (
                  <button
                    className="jira-open"
                    title="Open in Jira"
                    onClick={() => api.openInJira(selected.id).catch((e) => flash(String(e)))}
                  >
                    ↗ Jira
                  </button>
                )}
                {(liveTicket ?? selected).pr_url && (
                  <>
                    <button
                      className="pr-open"
                      title={`Open PR #${(liveTicket ?? selected).pr_number} on GitHub`}
                      onClick={() =>
                        api.openPrInBrowser(selected.id).catch((e) => flash(String(e)))
                      }
                    >
                      ↗ PR
                    </button>
                    {(() => {
                      const t = liveTicket ?? selected;
                      const state =
                        t.pr_state === "open" && t.pr_is_draft ? "draft" : t.pr_state;
                      return state ? (
                        <span className={"pr-chip pr-" + state} title={`PR #${t.pr_number} — ${state}`}>
                          PR {state}
                        </span>
                      ) : null;
                    })()}
                  </>
                )}
                {(liveTicket ?? selected).repo_id == null && (
                  <select
                    className="modal-repo-assign"
                    value=""
                    title="Assign a repo to this ticket"
                    onChange={(e) => {
                      const rid = Number(e.target.value);
                      if (!rid) return;
                      api
                        .assignTicketRepo(selected.id, rid)
                        .then(() =>
                          flash(`Assigned repo to ${selected.jira_key ?? `#${selected.id}`}`),
                        )
                        .catch((err) => flash(String(err)));
                    }}
                  >
                    <option value="" disabled>
                      {repos.length ? "⚠ Assign repo…" : "⚠ no repo — add one in Settings"}
                    </option>
                    {repos.map((r) => (
                      <option key={r.id} value={r.id}>
                        {r.name}
                      </option>
                    ))}
                  </select>
                )}
                <button className="close" title="Close (Esc)" onClick={() => setSelected(null)}>
                  ×
                </button>
              </div>

              {(liveTicket ?? selected).orchestrator_note ? (
                <div className="orchestrator-note" title="The orchestrator's last autonomous action on this ticket">
                  🤖 {(liveTicket ?? selected).orchestrator_note}
                </div>
              ) : null}

              <div className="actions">
                {selected.status === "todo" && !liveSessions[selected.id] && (
                  <button
                    disabled={busy !== null}
                    title="Interview to build the ticket's spec"
                    onClick={() =>
                      run("grill", async () => {
                        try {
                          await api.grillTicket(selected.id);
                          flash(`Building spec for #${selected.id}…`);
                        } catch (e) {
                          flash(String(e));
                        }
                        await refresh();
                      })
                    }
                  >
                    {busy === "grill"
                      ? "Starting…"
                      : selected.jira_key
                      ? "Build spec from Jira"
                      : "Build spec"}
                  </button>
                )}
                <button
                  disabled={busy !== null || !!liveSessions[selected.id]}
                  title={liveSessions[selected.id] ? "Terminal is open in the Session tab" : "Open a live Claude terminal"}
                  onClick={() =>
                    run("start", async () => {
                      await openTerminal(selected);
                      setTab("session");
                    })
                  }
                >
                  {liveSessions[selected.id]
                    ? "● Session live"
                    : busy === "start"
                    ? "Opening…"
                    : "Open terminal"}
                </button>
                {liveSessions[selected.id] && (
                  <button
                    disabled={busy !== null}
                    title="Kill the running Claude process"
                    onClick={() =>
                      run("stop", async () => {
                        await api.stopSession(liveSessions[selected.id]);
                      })
                    }
                  >
                    {busy === "stop" ? "Stopping…" : "Stop session"}
                  </button>
                )}
                <button
                  disabled={busy !== null}
                  title="Move to PR — opens a PR ready for review (must have changes and have been reviewed)"
                  onClick={() =>
                    run("pr", async () => {
                      // Go through the flow: blocks if there are no changes, redirects to Human
                      // review if it hasn't been reviewed yet, else opens the PR for review.
                      try {
                        await api.transitionTicket(selected.id, "in_review", false);
                      } catch (e) {
                        flash(String(e));
                      }
                      await refresh();
                    })
                  }
                >
                  Open PR
                </button>
                <button
                  className="danger"
                  disabled={busy !== null || !!liveSessions[selected.id]}
                  title={
                    liveSessions[selected.id] ? "Finish the running session first" : "Delete this ticket"
                  }
                  onClick={() => {
                    const t = liveTicket ?? selected;
                    const hasOpenPr = !!t.pr_url && (t.pr_state === "open" || t.pr_state === "");
                    run("delete", async () => {
                      const ok = await confirm(
                        `Delete "${t.title}"? Removes its worktree and harmony record` +
                          (t.jira_key ? " (not the Jira issue — it'll return on next Sync)." : ".") +
                          (hasOpenPr ? `\n\nIts open PR #${t.pr_number} will be closed on GitHub.` : ""),
                        { title: "Delete ticket", kind: "warning" }
                      );
                      if (!ok) return;
                      const done = await withDirtyConfirm(
                        (force) => api.deleteTicket(t.id, force),
                        `"${t.title}"`
                      );
                      if (!done) return;
                      setSelected(null);
                      await refresh();
                      flash("Ticket deleted");
                    });
                  }}
                >
                  {busy === "delete" ? "Deleting…" : "Delete"}
                </button>
              </div>

              <div className="tabs">
                {selected.jira_key && (
                  <button
                    className={"tab" + (tab === "description" ? " active" : "")}
                    onClick={() => setTab("description")}
                  >
                    Description
                  </button>
                )}
                <button
                  className={"tab" + (tab === "spec" ? " active" : "")}
                  onClick={() => setTab("spec")}
                >
                  Spec
                </button>
                <button
                  className={"tab" + (tab === "proof" ? " active" : "")}
                  onClick={() => setTab("proof")}
                >
                  Proof
                </button>
                <button
                  className={"tab" + (tab === "review" ? " active" : "")}
                  onClick={() => setTab("review")}
                >
                  Review
                  {(liveTicket ?? selected).review_text ? (
                    <span className="tab-dot" title="A review is available" />
                  ) : null}
                </button>
                <button
                  className={"tab" + (tab === "session" ? " active" : "")}
                  onClick={() => {
                    setTab("session");
                    // The terminal may have mounted while hidden — nudge it to refit.
                    setTimeout(() => window.dispatchEvent(new Event("resize")), 0);
                  }}
                >
                  Session
                  {pendingQuestion ? (
                    <span className="tab-dot" title="Claude is waiting for an answer" />
                  ) : null}
                </button>
              </div>

              {selected.jira_key && (
                <div className={"tabpanel" + (tab === "description" ? " active" : "")}>
                  <JiraInfo key={selected.id} ticketId={selected.id} />
                </div>
              )}

              <div className={"tabpanel" + (tab === "spec" ? " active" : "")}>
                <SpecEditor
                  key={selected.id}
                  ticket={liveTicket ?? selected}
                  onSaved={refresh}
                  onImplement={async () => {
                    const id = (liveTicket ?? selected).id;
                    const sid = await api.acceptProposedSpecAndImplement(id);
                    setLiveSessions((m) => ({ ...m, [id]: sid }));
                    setTab("session");
                    await refresh();
                  }}
                />
              </div>

              <div className={"tabpanel" + (tab === "proof" ? " active" : "")}>
                <ProofPane key={selected.id} ticket={liveTicket ?? selected} />
              </div>

              <div className={"tabpanel" + (tab === "review" ? " active" : "")}>
                <div className="subtabs">
                  <button
                    className={"subtab" + (reviewSub === "review" ? " active" : "")}
                    onClick={() => setReviewSub("review")}
                  >
                    Review
                  </button>
                  <button
                    className={"subtab" + (reviewSub === "diff" ? " active" : "")}
                    onClick={() => setReviewSub("diff")}
                  >
                    Diff
                  </button>
                  <button
                    className={"subtab" + (reviewSub === "pr" ? " active" : "")}
                    onClick={() => setReviewSub("pr")}
                  >
                    PR comments
                  </button>
                  {openFeedback > 0 && (
                    <button
                      className="send-claude subtab-send"
                      onClick={sendFeedbackToClaude}
                      disabled={sendingFeedback}
                    >
                      {sendingFeedback
                        ? "Sending…"
                        : `Send ${openFeedback} comment${openFeedback === 1 ? "" : "s"} to Claude`}
                    </button>
                  )}
                </div>

                <div className={"subtabpanel" + (reviewSub === "review" ? " active" : "")}>
                  <div className="review-head">
                    <button
                      disabled={busy !== null || !!liveSessions[selected.id]}
                      title={
                        liveSessions[selected.id]
                          ? "Finish the running session first"
                          : "Re-run /review on the current changes"
                      }
                      onClick={() =>
                        run("review", async () => {
                          try {
                            await api.requestReview(selected.id);
                            flash(`Reviewing #${selected.id}…`);
                            setTab("session");
                          } catch (e) {
                            flash(String(e));
                          }
                          await refresh();
                        })
                      }
                    >
                      {busy === "review"
                        ? "Starting…"
                        : (liveTicket ?? selected).review_text
                        ? "Re-request review"
                        : "Request review"}
                    </button>
                  </div>
                  <ReviewFeedback
                    key={selected.id}
                    ticketId={selected.id}
                    reviewText={(liveTicket ?? selected).review_text}
                    version={fbVersion}
                    onChanged={() => setFbVersion((v) => v + 1)}
                  />
                </div>

                <div className={"subtabpanel" + (reviewSub === "diff" ? " active" : "")}>
                  {worktrees.some((w) => w.ticket_id === selected.id) ? (
                    <DiffPane key={selected.id} ticketId={selected.id} />
                  ) : (
                    <p className="empty">No worktree yet — move the ticket to In Progress to start work and see a diff.</p>
                  )}
                </div>

                <div className={"subtabpanel" + (reviewSub === "pr" ? " active" : "")}>
                  <PrComments
                    key={`pr-${selected.id}`}
                    ticketId={selected.id}
                    version={fbVersion}
                    onCommentAdded={() => setFbVersion((v) => v + 1)}
                  />
                </div>
              </div>

              <div className={"tabpanel" + (tab === "session" ? " active" : "")}>
                {liveSessions[selected.id] ? (
                  <div className="session-live">
                    {/* Friendly ↔ Terminal toggle — both drive the SAME live PTY; switching never
                        respawns Claude. Offered for every session kind (rendering only). */}
                    <div className="session-view-toggle">
                      <button
                        className={viewMode === "friendly" ? "active" : ""}
                        onClick={() => changeViewMode("friendly")}
                        title="Chat-style view of the session"
                      >
                        Friendly
                      </button>
                      <button
                        className={viewMode === "terminal" ? "active" : ""}
                        onClick={() => changeViewMode("terminal")}
                        title="Raw Claude terminal"
                      >
                        Terminal
                      </button>
                    </div>
                    {viewMode === "friendly" ? (
                      <FriendlySession
                        key={liveSessions[selected.id]}
                        sessionId={liveSessions[selected.id]}
                        ticketId={selected.id}
                        pendingQuestion={pendingQuestion}
                        progress={progress[selected.id]}
                        todosJson={liveTicket?.todos ?? ""}
                        onAnswered={() =>
                          setTickets((ts) =>
                            ts.map((t) =>
                              t.id === selected.id ? { ...t, pending_question: "" } : t
                            )
                          )
                        }
                        onNeedTerminal={() => changeViewMode("terminal", false)}
                      />
                    ) : (
                      <>
                        {progress[selected.id] && (
                          <ProgressLine p={progress[selected.id]} className="detail-progress" />
                        )}
                        {pendingQuestion && (
                          <QuestionCard
                            pq={pendingQuestion}
                            onAnswered={() =>
                              setTickets((ts) =>
                                ts.map((t) =>
                                  t.id === selected.id ? { ...t, pending_question: "" } : t
                                )
                              )
                            }
                          />
                        )}
                        {liveTicket?.todos && <Tasks todosJson={liveTicket.todos} />}
                        <TranscriptPane ticketId={selected.id} />
                        <div className="term-head">Live terminal — type to steer Claude</div>
                        <TerminalView sessionId={liveSessions[selected.id]} />
                      </>
                    )}
                  </div>
                ) : (
                  <>
                    {progress[selected.id] && (
                      <ProgressLine p={progress[selected.id]} className="detail-progress" />
                    )}
                    {liveTicket?.todos && <Tasks todosJson={liveTicket.todos} />}
                    <TranscriptPane ticketId={selected.id} />
                    <div className="session-start">
                      <button
                        disabled={busy !== null}
                        title="Start a live Claude session in this ticket's worktree"
                        onClick={() =>
                          run("start", async () => {
                            await openTerminal(selected);
                            setTab("session");
                          })
                        }
                      >
                        {busy === "start" ? "Starting…" : "Start Claude"}
                      </button>
                    </div>
                  </>
                )}
              </div>
              </ErrorBoundary>
            </div>
          </div>
        )}
        </>
        )}
        </ErrorBoundary>
      </div>

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
