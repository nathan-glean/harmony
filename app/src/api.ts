import { invoke } from "@tauri-apps/api/core";
import type { Ticket, Repo, SessionView, WorktreeView } from "./types";

// Tauri converts camelCase JS arg keys to snake_case Rust params.
export const api = {
  listTickets: () => invoke<Ticket[]>("list_tickets"),
  listRepos: () => invoke<Repo[]>("list_repos"),
  addRepo: (name: string, path: string, project: string | null) =>
    invoke<number>("add_repo", { name, path, project }),
  getPermissionMode: () => invoke<string>("get_permission_mode"),
  setPermissionMode: (mode: string) => invoke<void>("set_permission_mode", { mode }),
  renameRepo: (id: number, name: string) => invoke<void>("rename_repo", { id, name }),
  deleteRepo: (id: number) => invoke<void>("delete_repo", { id }),
  getTicket: (id: number) => invoke<Ticket | null>("get_ticket", { id }),
  listSessions: () => invoke<SessionView[]>("list_sessions"),
  liveSessions: () => invoke<[number, number][]>("live_sessions"),
  pendingReattach: () => invoke<number[]>("pending_reattach"),
  sessionTranscript: (ticketId: number) =>
    invoke<string>("session_transcript", { ticketId }),
  clearEndedSessions: () => invoke<number>("clear_ended_sessions"),
  deleteSession: (id: number) => invoke<void>("delete_session", { id }),
  deleteWorktreeSessions: (worktreeId: number) =>
    invoke<number>("delete_worktree_sessions", { worktreeId }),
  listWorktrees: () => invoke<WorktreeView[]>("list_worktrees"),
  deleteWorktree: (id: number) => invoke<void>("delete_worktree", { id }),
  cleanupTicketWorktrees: (ticketId: number) =>
    invoke<void>("cleanup_ticket_worktrees", { ticketId }),
  addLocalTicket: (title: string, spec: string, repo: string | null) =>
    invoke<number>("add_local_ticket", { title, spec, repo }),
  setSpec: (id: number, spec: string) => invoke<void>("set_spec", { id, spec }),
  setStatus: (id: number, status: string) =>
    invoke<void>("set_ticket_status", { id, status }),
  jiraApplyColumn: (ticketId: number, status: string) =>
    invoke<void>("jira_apply_column", { ticketId, status }),
  deleteTicket: (id: number) => invoke<void>("delete_ticket", { ticketId: id }),
  jiraEnv: () => invoke<{ acli_installed: boolean; site: string | null }>("jira_env"),
  installAcli: () => invoke<string>("install_acli"),
  jiraLogout: () => invoke<void>("jira_logout"),
  jiraSync: () => invoke<number>("jira_sync"),
  draftTicket: (id: number) => invoke<string>("draft_ticket", { id }),
  jiraDetail: (ticketId: number) =>
    invoke<{ description: string; comments: { author: string; created: string; body: string }[] }>(
      "jira_detail",
      { ticketId }
    ),
  openPr: (ticketId: number) => invoke<string>("open_pr", { ticketId }),
  openInJira: (ticketId: number) => invoke<void>("open_in_jira", { ticketId }),
  ticketDiff: (ticketId: number) => invoke<string>("ticket_diff", { ticketId }),
  ticketPr: (ticketId: number) =>
    invoke<{ pr: any | null; checks: any[] }>("ticket_pr", { ticketId }),
  startSession: (ticketId: number, repo: string | null) =>
    invoke<number>("start_session", { ticketId, repo }),
  sendInput: (sessionId: number, data: string) =>
    invoke<void>("send_input", { sessionId, data }),
  answerQuestion: (
    sessionId: number,
    optionCount: number,
    selected: number[],
    customText: string | null,
    multiSelect: boolean
  ) =>
    invoke<void>("answer_question", {
      sessionId,
      optionCount,
      selected,
      customText,
      multiSelect,
    }),
  stopSession: (sessionId: number) => invoke<void>("stop_session", { sessionId }),
  resize: (sessionId: number, cols: number, rows: number) =>
    invoke<void>("resize", { sessionId, cols, rows }),
};
