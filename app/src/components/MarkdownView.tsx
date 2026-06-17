import React from "react";
import {
  MDXEditor,
  headingsPlugin,
  listsPlugin,
  quotePlugin,
  thematicBreakPlugin,
  linkPlugin,
  codeBlockPlugin,
  type CodeBlockEditorDescriptor,
} from "@mdxeditor/editor";
import "@mdxeditor/editor/style.css";
// Reuse the same refractor core + language set DiffPane uses, so review code blocks highlight
// with the app's existing Prism token theme (see `.token.*` rules in styles.css).
import { refractor } from "refractor/core";
import typescript from "refractor/typescript";
import tsx from "refractor/tsx";
import javascript from "refractor/javascript";
import jsx from "refractor/jsx";
import rust from "refractor/rust";
import python from "refractor/python";
import ruby from "refractor/ruby";
import go from "refractor/go";
import java from "refractor/java";
import c from "refractor/c";
import cpp from "refractor/cpp";
import csharp from "refractor/csharp";
import php from "refractor/php";
import json from "refractor/json";
import css from "refractor/css";
import markup from "refractor/markup";
import yaml from "refractor/yaml";
import toml from "refractor/toml";
import bash from "refractor/bash";
import sql from "refractor/sql";

for (const lang of [
  typescript, tsx, javascript, jsx, rust, python, ruby, go, java, c, cpp, csharp,
  php, json, css, markup, yaml, toml, bash, sql,
]) {
  refractor.register(lang as any);
}
const REGISTERED = new Set(refractor.listLanguages());

// Common fence-language aliases → registered refractor language.
const LANG_ALIAS: Record<string, string> = {
  ts: "typescript", js: "javascript", py: "python", rb: "ruby", rs: "rust",
  sh: "bash", shell: "bash", zsh: "bash", yml: "yaml", html: "markup", xml: "markup",
  "c++": "cpp", "c#": "csharp", cs: "csharp",
};

// Convert a refractor (hast) tree into React nodes, carrying Prism `token …` classNames through
// so the existing `.token.*` CSS colors them. No `dangerouslySetInnerHTML`.
function hastToReact(nodes: any[]): React.ReactNode {
  return nodes.map((n, i) => {
    if (n.type === "text") return n.value;
    if (n.type === "element") {
      const cls = n.properties?.className;
      return React.createElement(
        n.tagName,
        { key: i, className: Array.isArray(cls) ? cls.join(" ") : cls },
        hastToReact(n.children ?? [])
      );
    }
    return null;
  });
}

// Read-only, syntax-highlighted code block — replaces MDXEditor's CodeMirror editor (which leaks
// a language picker + delete button and a light background into the read-only view).
const ReadOnlyCodeBlock: React.ComponentType<{ code: string; language: string }> = ({ code, language }) => {
  const lang = LANG_ALIAS[language?.toLowerCase()] ?? language?.toLowerCase() ?? "";
  const highlighted = lang && REGISTERED.has(lang);
  return (
    <pre className="cb">
      <code>{highlighted ? hastToReact((refractor.highlight(code, lang) as any).children) : code}</code>
    </pre>
  );
};

const codeBlockDescriptor: CodeBlockEditorDescriptor = {
  priority: 100,
  match: () => true,
  Editor: ReadOnlyCodeBlock as any,
};

/** Read-only rendered markdown (no toolbar, not editable). Used to display Claude's `/review`
 * output as rich text with highlighted code blocks. */
export function MarkdownView({ markdown }: { markdown: string }) {
  return (
    <MDXEditor
      readOnly
      markdown={markdown}
      className="dark-theme"
      contentEditableClassName="md-content"
      // Claude's prose can contain odd markdown — don't let it crash the view.
      onError={(e) => console.warn("mdxeditor:", e?.error ?? e)}
      plugins={[
        headingsPlugin(),
        listsPlugin(),
        quotePlugin(),
        thematicBreakPlugin(),
        linkPlugin(),
        codeBlockPlugin({
          defaultCodeBlockLanguage: "",
          codeBlockEditorDescriptors: [codeBlockDescriptor],
        }),
      ]}
    />
  );
}
