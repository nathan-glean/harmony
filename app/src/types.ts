export type Ticket = {
  id: number;
  jira_key: string | null;
  source: string;
  title: string;
  spec: string;
  status: string;
  repo_id: number | null;
  created_at: number;
  updated_at: number;
  todos: string; // JSON array of { content, status }
  pending_question: string; // JSON { session_id, questions:[…] } or "" when none
  planned: number; // 0/1 — initial plan-mode run has happened (one-time, at first start)
  drafting: number; // 0/1 — a grill/spec session is building this ticket's spec
  grilled: number; // 0/1 — this ticket has been through a grill interview
  acceptance_criteria: string; // first-class spec field (markdown)
  relevant_paths: string; // first-class spec field (one path per line / markdown)
  constraints: string; // first-class spec field (markdown)
};

// Payload of the `pr-done` event: a background PR creation finished. `ok` false means it was
// reverted to Human Review and `error` should be surfaced.
export type PrDone = { ticket_id: number; ok: boolean; error: string | null };

// The structured spec, as composed/parsed on the backend (matches harmony_core::spec::SpecFields).
export type SpecFields = {
  spec: string;
  acceptance_criteria: string;
  relevant_paths: string;
  constraints: string;
};

// Shape of `Ticket.pending_question` once parsed (from an AskUserQuestion tool call).
export type QuestionOption = { label: string; description: string };
export type Question = {
  question: string;
  header: string;
  multiSelect: boolean;
  options: QuestionOption[];
};
export type PendingQuestion = { session_id: number; questions: Question[] };

export type Repo = {
  id: number;
  name: string;
  path: string;
  default_project_key: string | null;
};

export type WorktreeView = {
  id: number;
  ticket_id: number;
  ticket_title: string;
  jira_key: string | null;
  repo_name: string;
  repo_path: string;
  branch: string;
  path: string;
  is_alternate: number;
  created_at: number;
};

export type SessionView = {
  id: number;
  ticket_id: number;
  worktree_id: number;
  ticket_title: string;
  jira_key: string | null;
  branch: string;
  state: string;
  last_tool: string | null;
  claude_session_id: string | null;
  started_at: number;
  ended_at: number | null;
};

// Payload of the `session-exit` event. `ok` is false when the Claude process exited
// abnormally (a crash) and it wasn't a user-initiated stop.
export type SessionExit = {
  session_id: number;
  ticket_id: number;
  ok: boolean;
  code: number;
};

// Live in-session progress tailed from a session's transcript (last assistant message +
// current tool), keyed by ticket. Richer than SessionView.state's working/waiting flag.
export type SessionProgress = {
  ticket_id: number;
  session_id: number;
  message: string | null;
  tool: string | null;
};

// A reviewer comment left on a diff line (matches harmony_core::models::DiffComment).
// `status`: "open" (will be sent to Claude on next resume), "sent", or "resolved".
export type DiffComment = {
  id: number;
  ticket_id: number;
  file_path: string;
  line: number; // start line of the range
  end_line: number; // end line; == line for single-line comments
  side: "new" | "old";
  body: string;
  status: "open" | "sent" | "resolved";
  created_at: number;
};

export const COLUMNS = ["todo", "working", "waiting", "in_review", "done"] as const;

export const COLUMN_LABELS: Record<string, string> = {
  todo: "Todo",
  working: "In Progress",
  waiting: "For Your Review",
  in_review: "In PR Review",
  done: "Done",
};
