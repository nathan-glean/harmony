import { invoke } from "@tauri-apps/api/core";
import type { Ticket, Repo, SessionView, WorktreeView, SessionProgress, SpecFields, DiffComment, PrComment, CommentTarget } from "./types";

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
  liveProgress: () => invoke<SessionProgress[]>("live_progress"),
  pendingReattach: () => invoke<number[]>("pending_reattach"),
  sessionTranscript: (ticketId: number) =>
    invoke<string>("session_transcript", { ticketId }),
  clearEndedSessions: () => invoke<number>("clear_ended_sessions"),
  deleteSession: (id: number) => invoke<void>("delete_session", { id }),
  deleteWorktreeSessions: (worktreeId: number) =>
    invoke<number>("delete_worktree_sessions", { worktreeId }),
  listWorktrees: () => invoke<WorktreeView[]>("list_worktrees"),
  worktreeDirty: (id: number) => invoke<boolean>("worktree_dirty", { id }),
  deleteWorktree: (id: number, force: boolean) =>
    invoke<void>("delete_worktree", { id, force }),
  cleanupTicketWorktrees: (ticketId: number, force: boolean) =>
    invoke<void>("cleanup_ticket_worktrees", { ticketId, force }),
  addLocalTicket: (title: string, spec: string, repo: string | null) =>
    invoke<number>("add_local_ticket", { title, spec, repo }),
  setSpec: (id: number, spec: string) => invoke<void>("set_spec", { id, spec }),
  setSpecFields: (id: number, fields: SpecFields) =>
    invoke<void>("set_spec_fields", {
      id,
      spec: fields.spec,
      acceptanceCriteria: fields.acceptance_criteria,
      relevantPaths: fields.relevant_paths,
      constraints: fields.constraints,
    }),
  setStatus: (id: number, status: string) =>
    invoke<void>("set_ticket_status", { id, status }),
  // Lifecycle transitions go through the backend flow state machine (executor).
  transitionTicket: (id: number, status: string, force: boolean) =>
    invoke<void>("transition_ticket", { ticketId: id, status, force }),
  grillTicket: (id: number) => invoke<void>("grill_ticket", { ticketId: id }),
  requestReview: (id: number) => invoke<void>("request_review", { ticketId: id }),
  requestCiFix: (id: number) => invoke<string>("request_ci_fix", { ticketId: id }),
  getCiAutofix: () => invoke<boolean>("get_ci_autofix"),
  setCiAutofix: (enabled: boolean) => invoke<void>("set_ci_autofix", { enabled }),
  getAutoReview: () => invoke<boolean>("get_auto_review"),
  setAutoReview: (enabled: boolean) => invoke<void>("set_auto_review", { enabled }),
  getReviewLoop: () => invoke<boolean>("get_review_loop"),
  setReviewLoop: (enabled: boolean) => invoke<void>("set_review_loop", { enabled }),
  getAutoMerge: () => invoke<boolean>("get_auto_merge"),
  setAutoMerge: (enabled: boolean) => invoke<void>("set_auto_merge", { enabled }),
  getAutoEndIdle: () => invoke<boolean>("get_auto_end_idle"),
  setAutoEndIdle: (enabled: boolean) => invoke<void>("set_auto_end_idle", { enabled }),
  getPrDescAutoupdate: () => invoke<boolean>("get_pr_desc_autoupdate"),
  setPrDescAutoupdate: (enabled: boolean) => invoke<void>("set_pr_desc_autoupdate", { enabled }),
  updatePrDescription: (id: number) => invoke<void>("update_pr_description_now", { ticketId: id }),
  jiraApplyColumn: (ticketId: number, status: string) =>
    invoke<void>("jira_apply_column", { ticketId, status }),
  deleteTicket: (id: number, force: boolean) =>
    invoke<void>("delete_ticket", { ticketId: id, force }),
  jiraEnv: () => invoke<{ acli_installed: boolean; site: string | null }>("jira_env"),
  installAcli: () => invoke<string>("install_acli"),
  jiraLogout: () => invoke<void>("jira_logout"),
  jiraSync: () => invoke<number>("jira_sync"),
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
  ticketPrComments: (ticketId: number) =>
    invoke<PrComment[]>("ticket_pr_comments", { ticketId }),
  listDiffComments: (ticketId: number) =>
    invoke<DiffComment[]>("list_diff_comments", { ticketId }),
  addDiffComment: (
    ticketId: number,
    filePath: string,
    line: number,
    endLine: number,
    side: "new" | "old",
    body: string
  ) =>
    invoke<number>("add_diff_comment", {
      ticketId,
      target: "diff",
      anchor: "",
      filePath,
      line,
      endLine,
      side,
      body,
    }),
  // General / on-review / on-PR-comment feedback (no diff anchor).
  addComment: (ticketId: number, target: CommentTarget, anchor: string, body: string) =>
    invoke<number>("add_diff_comment", {
      ticketId,
      target,
      anchor,
      filePath: "",
      line: 0,
      endLine: 0,
      side: "new",
      body,
    }),
  deleteDiffComment: (id: number) => invoke<void>("delete_diff_comment", { id }),
  resolveDiffComment: (id: number) => invoke<void>("resolve_diff_comment", { id }),
  addressFeedback: (ticketId: number) => invoke<number>("address_feedback", { ticketId }),
  acceptProposedSpec: (ticketId: number) => invoke<void>("accept_proposed_spec", { ticketId }),
  acceptProposedSpecAndImplement: (ticketId: number) =>
    invoke<number>("accept_proposed_spec_and_implement", { ticketId }),
  proposedSpecDiff: (ticketId: number) => invoke<string>("proposed_spec_diff", { ticketId }),
  rejectProposedSpec: (ticketId: number) => invoke<void>("reject_proposed_spec", { ticketId }),
  startSession: (ticketId: number, repo: string | null) =>
    invoke<number>("start_session", { ticketId, repo }),
  startSpecSession: (ticketId: number, repo: string | null) =>
    invoke<number>("start_spec_session", { ticketId, repo }),
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
