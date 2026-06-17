import {
  MDXEditor,
  headingsPlugin,
  listsPlugin,
  quotePlugin,
  thematicBreakPlugin,
  linkPlugin,
  linkDialogPlugin,
  markdownShortcutPlugin,
  codeBlockPlugin,
  codeMirrorPlugin,
  tablePlugin,
  toolbarPlugin,
  UndoRedo,
  BoldItalicUnderlineToggles,
  ListsToggle,
  BlockTypeSelect,
  CreateLink,
  CodeToggle,
  InsertTable,
} from "@mdxeditor/editor";
import "@mdxeditor/editor/style.css";

/** A labelled WYSIWYG markdown editor (renders markdown as rich text). Round-trips to a markdown
 * string via `onChange`. Used for the ticket spec fields. */
export function MarkdownField({
  label,
  value,
  onChange,
  placeholder,
  tall = false,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  tall?: boolean;
}) {
  return (
    <div className="md-field">
      <label className="field-label">{label}</label>
      <MDXEditor
        markdown={value}
        onChange={(md) => onChange(md ?? "")}
        placeholder={placeholder}
        className="dark-theme"
        contentEditableClassName={"md-content" + (tall ? " tall" : "")}
        // Don't let odd markdown crash the editor.
        onError={(e) => console.warn("mdxeditor:", e?.error ?? e)}
        plugins={[
          headingsPlugin(),
          listsPlugin(),
          quotePlugin(),
          thematicBreakPlugin(),
          linkPlugin(),
          linkDialogPlugin(),
          // Render fenced code blocks (common in spec bodies) without erroring.
          codeBlockPlugin({ defaultCodeBlockLanguage: "" }),
          codeMirrorPlugin({
            codeBlockLanguages: { "": "Plain text", text: "Plain text", ts: "TypeScript", js: "JavaScript", rust: "Rust", py: "Python", sql: "SQL", sh: "Shell", json: "JSON" },
          }),
          // Render and round-trip GitHub-flavoured markdown tables.
          tablePlugin(),
          markdownShortcutPlugin(),
          toolbarPlugin({
            toolbarContents: () => (
              <>
                <UndoRedo />
                <BoldItalicUnderlineToggles />
                <CodeToggle />
                <ListsToggle />
                <BlockTypeSelect />
                <CreateLink />
                <InsertTable />
              </>
            ),
          }),
        ]}
      />
    </div>
  );
}
