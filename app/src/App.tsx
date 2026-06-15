import { useEffect, useState, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
  onAction,
} from "@tauri-apps/plugin-notification";
import { Board } from "./components/Board";
import { Sessions } from "./components/Sessions";
import { Worktrees } from "./components/Worktrees";
import { Settings } from "./components/Settings";
import { DiffPane } from "./components/DiffPane";
import { JiraInfo } from "./components/JiraInfo";
import { Tasks } from "./components/Tasks";
import { QuestionCard } from "./components/QuestionCard";
import { TranscriptPane } from "./components/TranscriptPane";
import { ProgressLine } from "./components/ProgressLine";
import { TerminalView } from "./components/Terminal";
import { api } from "./api";
import type { Ticket, Repo, SessionView, WorktreeView, PendingQuestion, SessionProgress, SessionExit } from "./types";

export function App() {
  const [view, setView] = useState<"board" | "sessions" | "worktrees" | "settings">("board");
  const [tickets, setTickets] = useState<Ticket[]>([]);
  const [sessions, setSessions] = useState<SessionView[]>([]);
  const [worktrees, setWorktrees] = useState<WorktreeView[]>([]);
  const [liveSessionIds, setLiveSessionIds] = useState<Set<number>>(new Set());
  const [selected, setSelected] = useState<Ticket | null>(null);
  const [spec, setSpec] = useState("");
  // Live terminals keyed by ticket id → session id (supports several at once).
  const [liveSessions, setLiveSessions] = useState<Record<number, number>>({});
  // Live in-session progress (last assistant message + current tool), keyed by ticket id.
  const [progress, setProgress] = useState<Record<number, SessionProgress>>({});
  const detailRef = useRef<HTMLElement>(null);
  // Attention notifications: track prior session states to fire on entry into "waiting".
  const prevStates = useRef<Map<number, string>>(new Map());
  const prevDrafting = useRef<Map<number, number>>(new Map());
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

      // Notify when a live session enters "waiting" (Claude wants input). Skip the first
      // pass so we don't fire for sessions that were already waiting on launch.
      const liveSess = sess.filter((s) => !s.ended_at);
      for (const s of liveSess) {
        const prev = prevStates.current.get(s.id);
        if (seeded.current && s.state === "waiting" && prev !== "waiting") {
          lastAttention.current = s.ticket_id;
          notifyAttention(s, questionText(tks, s.ticket_id));
        }
      }
      const next = new Map<number, string>();
      liveSess.forEach((s) => next.set(s.id, s.state));
      prevStates.current = next;
      seeded.current = true;

      // Grill finished: when a ticket's `drafting` flips 1 → 0 (spec captured by the
      // ExitPlanMode hook), auto-stop the grill session. If the ticket is already in
      // In Progress (grilled on entry), auto-continue into the work session
      // (plan-tasks → execute); a new-ticket grill (status "todo") just stops.
      const liveByTicket = new Map(live.map(([tid, sid]) => [tid, sid]));
      for (const t of tks) {
        const was = prevDrafting.current.get(t.id);
        if (was === 1 && t.drafting === 0) {
          const sid = liveByTicket.get(t.id);
          if (sid != null) api.stopSession(sid).catch(() => {});
          if (t.status === "working") {
            api
              .startSession(t.id, null)
              .then((workSid) => setLiveSessions((m) => ({ ...m, [t.id]: workSid })))
              .catch((e) => flash(String(e)));
            flash(`Spec ready — starting work on #${t.id}`);
          } else {
            flash(`Spec ready for #${t.id}`);
          }
        }
      }
      prevDrafting.current = new Map(tks.map((t) => [t.id, t.drafting]));
    } catch (e) {
      console.error(e);
    }
  }, []);

  // First pending-question text for a ticket (if Claude asked a structured question), so the
  // notification can carry it. Returns null when there's no captured question.
  function questionText(tks: Ticket[], ticketId: number): string | null {
    const t = tks.find((x) => x.id === ticketId);
    if (!t?.pending_question) return null;
    try {
      const pq: PendingQuestion = JSON.parse(t.pending_question);
      return pq.questions[0]?.question ?? null;
    } catch {
      return null;
    }
  }

  async function notifyAttention(s: SessionView, question: string | null) {
    try {
      let granted = await isPermissionGranted();
      if (!granted) granted = (await requestPermission()) === "granted";
      if (!granted) return;
      const who = s.jira_key ?? `local #${s.ticket_id}`;
      sendNotification({
        title: question ? `Claude is asking — ${who}` : "Claude needs your input",
        body: question ?? `${who} — ${s.ticket_title}`,
      });
    } catch {
      /* notifications unavailable */
    }
  }

  // Open (and start/resume if needed) a live terminal for a ticket.
  const openTerminal = async (ticket: Ticket) => {
    setSelected(ticket);
    setView("board");
    if (liveSessions[ticket.id]) return; // already attached in this run
    const sid = await api.startSession(ticket.id, null);
    setLiveSessions((m) => ({ ...m, [ticket.id]: sid }));
    await refresh();
  };

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

  // Click outside the open detail panel (on empty board space) closes it.
  // Clicking another card switches instead; topbar/forms are left alone.
  useEffect(() => {
    if (!selected) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (detailRef.current?.contains(t)) return; // inside the panel
      if (t.closest(".card")) return; // a card → let it switch selection
      if (!t.closest(".main")) return; // outside the board area (topbar/forms)
      setSelected(null);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [selected]);

  // Keep the spec editor in sync with the selected ticket.
  useEffect(() => {
    setSpec(selected?.spec ?? "");
  }, [selected?.id]);

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
  const moveTicket = async (id: number, status: string) => {
    if (status === "working") {
      const t = tickets.find((x) => x.id === id);
      // A ticket still being grilled has no finished spec yet — block it from starting work.
      if (t?.drafting) {
        flash("Still drafting the spec — finish the interview first");
        return;
      }
      // Never grilled (e.g. a Jira-synced ticket): grill the spec first. When the spec is
      // captured, `refresh` auto-continues into the work session (plan-tasks → execute).
      if (t && !t.grilled) {
        setTickets((prev) => prev.map((x) => (x.id === id ? { ...x, status } : x)));
        if (selected?.id === id) setSelected((s) => (s ? { ...s, status } : s));
        try {
          await api.setStatus(id, status);
          api.jiraApplyColumn(id, status).catch(() => {});
          await api.startSpecSession(id, null);
          setSelected({ ...t, status });
          setView("board");
          flash(`Grilling spec for #${id} before work…`);
        } catch (e) {
          flash(String(e));
        }
        await refresh();
        return;
      }
    }
    setTickets((prev) => prev.map((t) => (t.id === id ? { ...t, status } : t)));
    if (selected?.id === id) setSelected((s) => (s ? { ...s, status } : s));
    try {
      // Leaving In Progress: stop the session first so its dying hooks can't
      // re-stamp the status back to "working".
      if (status !== "working" && liveSessions[id]) {
        await api.stopSession(liveSessions[id]);
      }
      await api.setStatus(id, status);
      // Mirror the move onto Jira (best-effort; backend skips non-Jira tickets and
      // statuses with no Jira equivalent, and only transitions if the status exists).
      api.jiraApplyColumn(id, status).catch(() => {});
      if (status === "working" && !liveSessions[id]) {
        const t = tickets.find((x) => x.id === id);
        if (t) await openTerminal({ ...t, status });
      }
      // Done → tidy up: remove the worktree(s) (branch/PR are untouched). Confirm first if
      // there are uncommitted changes that would be discarded.
      if (status === "done") {
        await withDirtyConfirm((force) => api.cleanupTicketWorktrees(id, force), "This ticket's worktree");
      }
    } catch (e) {
      flash(String(e));
    }
    await refresh();
  };

  // The selected ticket, but kept fresh from the poll (so live todos/status update).
  const liveTicket = selected ? tickets.find((t) => t.id === selected.id) ?? selected : null;

  // Claude's pending AskUserQuestion for the selected ticket, parsed for the question card.
  const pendingQuestion: PendingQuestion | null = (() => {
    if (!liveTicket?.pending_question) return null;
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
                // Kick off the grill/spec session, then open the ticket so the user can
                // answer the interview in the live terminal / question card.
                await api.startSpecSession(id, null);
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
        ) : (
        <>
        <Board
          tickets={tickets}
          selectedId={selected?.id ?? null}
          progress={progress}
          onSelect={setSelected}
          onMove={moveTicket}
        />

        {selected && (
          <aside className="detail" ref={detailRef}>
            <div className="detail-head">
              <span className="badge">{selected.status}</span>
              <strong>{selected.jira_key ?? `local #${selected.id}`}</strong>
              {selected.jira_key && (
                <button
                  className="jira-open"
                  title="Open in Jira"
                  onClick={() => api.openInJira(selected.id).catch((e) => flash(String(e)))}
                >
                  ↗ Jira
                </button>
              )}
              <button className="close" title="Close" onClick={() => setSelected(null)}>
                ×
              </button>
            </div>
            <h2>{selected.title}</h2>

            {progress[selected.id] && (
              <ProgressLine p={progress[selected.id]} className="detail-progress" />
            )}

            <div className="actions">
              {selected.jira_key && (
                <button
                  disabled={busy !== null}
                  onClick={() =>
                    run("draft", async () => {
                      const s = await api.draftTicket(selected.id);
                      setSpec(s);
                      await refresh();
                    })
                  }
                >
                  {busy === "draft" ? "Drafting…" : "Draft from Jira"}
                </button>
              )}
              <button
                disabled={busy !== null}
                onClick={() => run("save", async () => { await api.setSpec(selected.id, spec); await refresh(); })}
              >
                Save spec
              </button>
              <button
                disabled={busy !== null || !!liveSessions[selected.id]}
                title={liveSessions[selected.id] ? "Terminal is open below" : "Open a live Claude terminal"}
                onClick={() => run("start", async () => { await openTerminal(selected); })}
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
                onClick={() =>
                  run("pr", async () => {
                    const url = await api.openPr(selected.id);
                    flash(`PR: ${url}`);
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
                  const t = selected;
                  run("delete", async () => {
                    const ok = await confirm(
                      `Delete "${t.title}"? Removes its worktree and harmony record` +
                        (t.jira_key ? " (not the Jira issue — it'll return on next Sync)." : "."),
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

            {selected.jira_key && <JiraInfo ticketId={selected.id} />}

            <textarea
              className="spec"
              value={spec}
              placeholder="Agent spec (markdown) — write or Draft from Jira…"
              onChange={(e) => setSpec(e.target.value)}
            />

            {liveTicket?.todos && <Tasks todosJson={liveTicket.todos} />}

            <TranscriptPane ticketId={selected.id} />

            {liveSessions[selected.id] && (
              <>
                <div className="term-head">Live terminal — type to steer Claude</div>
                <TerminalView sessionId={liveSessions[selected.id]} />
              </>
            )}

            {worktrees.some((w) => w.ticket_id === selected.id) && (
              <DiffPane ticketId={selected.id} />
            )}
          </aside>
        )}
        </>
        )}
      </div>

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
