import { useState } from "react";
import { MarkdownField } from "./MarkdownField";
import { SpecDiff } from "./SpecDiff";
import { api } from "../api";
import type { Ticket } from "../types";

/** The Spec tab's editor: four WYSIWYG markdown fields + Save. Buffers are initialised from the
 * ticket at mount, so render this with `key={ticket.id}` to re-init when the ticket changes.
 * `onImplement` (when provided) accepts the proposed spec and starts a session to implement it —
 * the parent owns that so it can surface the new session (switch to the Session tab). */
export function SpecEditor({
  ticket,
  onSaved,
  onImplement,
}: {
  ticket: Ticket;
  onSaved: () => void;
  onImplement?: () => void | Promise<void>;
}) {
  const [spec, setSpec] = useState(ticket.spec ?? "");
  const [acceptance, setAcceptance] = useState(ticket.acceptance_criteria ?? "");
  const [paths, setPaths] = useState(ticket.relevant_paths ?? "");
  const [constraints, setConstraints] = useState(ticket.constraints ?? "");
  const [saving, setSaving] = useState(false);

  const save = async () => {
    setSaving(true);
    try {
      await api.setSpecFields(ticket.id, {
        spec,
        acceptance_criteria: acceptance,
        relevant_paths: paths,
        constraints,
      });
      onSaved();
    } finally {
      setSaving(false);
    }
  };

  const [specBusy, setSpecBusy] = useState(false);
  const acceptProposed = async () => {
    setSpecBusy(true);
    try {
      await api.acceptProposedSpec(ticket.id);
      onSaved();
    } finally {
      setSpecBusy(false);
    }
  };
  const rejectProposed = async () => {
    setSpecBusy(true);
    try {
      await api.rejectProposedSpec(ticket.id);
      onSaved();
    } finally {
      setSpecBusy(false);
    }
  };
  const acceptAndImplement = async () => {
    setSpecBusy(true);
    try {
      await onImplement?.();
    } finally {
      setSpecBusy(false);
    }
  };

  return (
    <div className="spec-fields">
      {ticket.proposed_spec?.trim() && (
        <div className="proposed-spec">
          <div className="proposed-spec-head">
            <strong>Claude proposed a spec update</strong>
            <span className="muted">from review feedback that contradicted the current spec</span>
            <div className="proposed-spec-actions">
              {onImplement && (
                <button className="primary" disabled={specBusy} onClick={acceptAndImplement}>
                  {specBusy ? "…" : "Accept & implement"}
                </button>
              )}
              <button disabled={specBusy} onClick={acceptProposed}>
                {specBusy ? "…" : "Accept"}
              </button>
              <button disabled={specBusy} onClick={rejectProposed}>
                Reject
              </button>
            </div>
          </div>
          <div className="review-text proposed-spec-body">
            <SpecDiff ticket={ticket} />
          </div>
        </div>
      )}
      <MarkdownField
        label="Spec"
        value={spec}
        onChange={setSpec}
        tall
        placeholder="Agent spec body (markdown) — Goal, Context… or Build spec from Jira…"
      />
      <MarkdownField
        label="Acceptance criteria"
        value={acceptance}
        onChange={setAcceptance}
        placeholder="What must be true to call this done…"
      />
      <MarkdownField
        label="Relevant paths"
        value={paths}
        onChange={setPaths}
        placeholder="Files/dirs the agent should focus on…"
      />
      <MarkdownField
        label="Constraints"
        value={constraints}
        onChange={setConstraints}
        placeholder="Boundaries / non-goals / must-nots…"
      />
      <button className="save-spec" disabled={saving} onClick={save}>
        {saving ? "Saving…" : "Save spec"}
      </button>
    </div>
  );
}
