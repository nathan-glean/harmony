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

export const COLUMNS = ["todo", "working", "waiting", "in_review", "done"] as const;

export const COLUMN_LABELS: Record<string, string> = {
  todo: "Todo",
  working: "In Progress",
  waiting: "For Your Review",
  in_review: "In PR Review",
  done: "Done",
};
