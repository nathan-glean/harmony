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
  review_text: string; // latest `/review` prose (Claude's final message), "" when never reviewed
  ci_triaged_sha: string; // HEAD sha the last CI triage ran against ("" when never)
  ci_fix_attempts: number; // auto CI-fix attempts made for this PR (capped)
  ci_triage: string; // JSON of the latest CiTriage, "" when none
  proposed_spec: string; // markdown of a spec update Claude proposed (propose & confirm), "" when none
  activity: string; // JSON of the derived Activity (what's happening), "" until first computed
  orchestrator_note: string; // the orchestrator's last action + why (audit line), "" when none
  proof: string; // proof-of-work report (markdown), "" until a proof run completes
  proof_artifacts: string; // JSON array of ProofArtifact (captured media), "" when none
  proof_sha: string; // HEAD the proof last evidenced ("" when never)
  pr_number: number; // GitHub PR number (0 when none)
  pr_url: string; // GitHub PR html URL ("" when none)
  pr_state: string; // "open" | "closed" | "merged" | "" — persisted PR snapshot
  pr_is_draft: number; // 0/1 — the open PR is a draft
};

// One captured proof-of-work artifact (matches harmony_core::proof::ProofArtifact).
export type ProofArtifact = {
  kind: "image" | "video" | "cast" | "file";
  path: string; // absolute local path under ~/.harmony/proof/<ticket> (served via the asset protocol)
  caption: string;
  url: string; // teammate-reachable URL once hosted for the PR comment ("" locally)
};

/** Parse a ticket's `proof_artifacts` JSON into a list; [] when empty/unparseable. */
export function parseProofArtifacts(json: string): ProofArtifact[] {
  if (!json) return [];
  try {
    const v = JSON.parse(json);
    return Array.isArray(v) ? (v as ProofArtifact[]) : [];
  } catch {
    return [];
  }
}

// The backend-derived "what's happening" status (matches harmony_core::activity::Activity).
export type ActivityCategory = "working" | "waiting_on_you" | "waiting_external" | "idle";
export type Activity = {
  category: ActivityCategory;
  label: string;
  detail: string | null;
};

/** Parse a ticket's `activity` JSON; null when empty/unparseable. */
export function parseActivity(json: string): Activity | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as Activity;
  } catch {
    return null;
  }
}

// A GitHub PR comment normalized for display (matches harmony_core::github::PrComment).
export type PrComment = {
  author: string;
  body: string;
  created_at: string; // ISO8601
  kind: "conversation" | "review" | "inline";
  state: string; // review state (APPROVED/CHANGES_REQUESTED/COMMENTED) or ""
  path: string; // inline file path or ""
  line: number; // inline line or 0
  url: string;
  diff_hunk: string; // inline: the unified diff context the comment is on; "" otherwise
  priority: string; // "high" | "medium" | "low" | "" — parsed from the comment (e.g. Copilot severity)
  start_line: number; // inline multi-line: first line of the range (line is the last); 0 otherwise
};

// The LLM's attribution of a CI failure (matches harmony_core::ci::CiVerdict).
export type CiVerdict = {
  category: "pr_caused" | "unrelated_infra" | "flaky" | "undetermined";
  confidence: number; // 0..1
  rationale: string;
  proposed_fix: string;
};

// Full CI triage for a ticket's PR (matches harmony_core::ci::CiTriage; parsed from Ticket.ci_triage).
export type CiTriage = {
  head_sha: string;
  failing_checks: string[];
  base_red_checks: string[];
  required_checks: string[] | null;
  verdict: CiVerdict | null;
  actionable: boolean;
  reason: string;
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

// One typed block of a structured transcript message (matches harmony_core::session::TranscriptBlock).
// The friendly session view renders `text` as markdown, `tool_use` as a compact card, and
// `tool_result` (hidden by default) as the expandable output of its matching `tool_use`.
export type TranscriptBlock =
  | { type: "text"; text: string }
  | { type: "tool_use"; id: string; name: string; summary: string }
  | { type: "tool_result"; tool_use_id: string; output: string; is_error: boolean };

// One structured message in a session's conversation (matches harmony_core::session::TranscriptMessage).
export type TranscriptMessage = { role: "assistant" | "user"; blocks: TranscriptBlock[] };

// The friendly view's turn-state read (matches harmony_core::session::ViewState). `exit_plan` is
// the escape-hatch signal that auto-switches the UI to the raw terminal.
export type SessionViewState = "working" | "finished" | "waiting_on_question" | "exit_plan";

// How a live session renders: the new chat-style GUI ("friendly") or the raw embedded terminal.
export type SessionViewMode = "friendly" | "terminal";

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

// A reviewer comment for a ticket (matches harmony_core::models::DiffComment).
// `status`: "open" (will be sent to Claude on next "send feedback"), "sent", or "resolved".
// `target`: which surface — "diff" (file:line), "general", "review" (on Claude's /review),
// or "pr_comment" (on a GitHub PR comment). `anchor` carries context for non-diff targets.
export type CommentTarget = "general" | "diff" | "review" | "pr_comment";
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
  target: CommentTarget;
  anchor: string;
};

// One row of the immutable action log (matches harmony_core::models::TicketAction). Backs both
// idempotency (state-stamped completions) and the Orchestrator tab's audit feed.
export type TicketAction = {
  id: number;
  ticket_id: number;
  kind: string; // proof|review|judge|ci_triage|conflict | dispatch|restart|answer|spec|pr|escalate|info
  head_sha: string; // branch HEAD it acted on ("" for state-less notes)
  base_sha: string; // base merged (conflict actions only)
  note: string;
  created_at: number; // unix seconds
  ticket_title: string;
  jira_key: string | null;
};

// Live orchestrator status (matches the get_orchestrator_status command).
export type OrchestratorStatus = {
  enabled: boolean;
  max_concurrent: number;
  last_tick_at: number | null; // unix seconds
  deciding: { ticket_id: number; what: string } | null;
};

export const COLUMNS = ["todo", "working", "waiting", "in_review", "done"] as const;

export const COLUMN_LABELS: Record<string, string> = {
  todo: "Todo",
  working: "In Progress",
  waiting: "For Your Review",
  in_review: "In PR Review",
  done: "Done",
};
