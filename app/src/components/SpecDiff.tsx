import { useEffect, useMemo, useState } from "react";
import { parseDiff, Diff, Hunk, Decoration, tokenize } from "react-diff-view";
import "react-diff-view/style/index.css";
import { refractorAdapter, langFor } from "../lib/refractor";
import { api } from "../api";
import { MarkdownView } from "./MarkdownView";
import type { Ticket } from "../types";

/** Read-only unified diff of the live spec → Claude's proposed spec, shown in the Spec tab so it's
 * clear what the proposal actually changes. Falls back to rendering the full proposed markdown if
 * the diff can't be produced (e.g. backend error). */
export function SpecDiff({ ticket }: { ticket: Ticket }) {
  const [diffText, setDiffText] = useState<string | null>(null);
  const [err, setErr] = useState(false);

  useEffect(() => {
    let alive = true;
    setDiffText(null);
    setErr(false);
    api
      .proposedSpecDiff(ticket.id)
      .then((d) => alive && setDiffText(d))
      .catch(() => alive && setErr(true));
    return () => {
      alive = false;
    };
  }, [ticket.id]);

  const files = useMemo(() => {
    if (!diffText) return [];
    try {
      return parseDiff(diffText);
    } catch {
      return [];
    }
  }, [diffText]);

  const tokens = useMemo(() => {
    const file = files[0];
    if (!file) return undefined;
    const lang = langFor("spec.md");
    if (!lang) return undefined;
    try {
      return tokenize(file.hunks, { highlight: true, refractor: refractorAdapter, language: lang } as any);
    } catch {
      return undefined;
    }
  }, [files]);

  // Backend error, or a diff that parsed to nothing — fall back to the full proposed text.
  if (err || (diffText !== null && files.length === 0)) {
    return <MarkdownView markdown={ticket.proposed_spec} />;
  }
  if (diffText === null) return <div className="muted">Loading diff…</div>;

  const file = files[0];
  return (
    <div className="spec-diff">
      <Diff viewType="unified" diffType={file.type} hunks={file.hunks} tokens={tokens}>
        {(hunks) =>
          hunks.flatMap((hunk: any) => [
            <Decoration key={"deco-" + hunk.content}>
              <span className="hunk-header">{hunk.content}</span>
            </Decoration>,
            <Hunk key={hunk.content} hunk={hunk} />,
          ])
        }
      </Diff>
    </div>
  );
}
