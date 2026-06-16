import { useState } from "react";
import { MarkdownField } from "./MarkdownField";
import { api } from "../api";
import type { Ticket } from "../types";

/** The Spec tab's editor: four WYSIWYG markdown fields + Save. Buffers are initialised from the
 * ticket at mount, so render this with `key={ticket.id}` to re-init when the ticket changes. */
export function SpecEditor({ ticket, onSaved }: { ticket: Ticket; onSaved: () => void }) {
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

  return (
    <div className="spec-fields">
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
