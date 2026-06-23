// Shared refractor (Prism) setup for syntax-highlighting diffs. Used by both the PR DiffPane and the
// Spec tab's proposed-spec diff. Uses refractor's empty core and registers only the languages we map
// below — registering a language pulls in its transitive grammars (e.g. tsx → jsx + typescript), so
// diffs still highlight correctly without bundling all ~280 Prism languages.
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
import kotlin from "refractor/kotlin";
import swift from "refractor/swift";
import c from "refractor/c";
import cpp from "refractor/cpp";
import csharp from "refractor/csharp";
import php from "refractor/php";
import json from "refractor/json";
import css from "refractor/css";
import scss from "refractor/scss";
import less from "refractor/less";
import markup from "refractor/markup";
import markdown from "refractor/markdown";
import yaml from "refractor/yaml";
import toml from "refractor/toml";
import bash from "refractor/bash";
import sql from "refractor/sql";

for (const lang of [
  typescript, tsx, javascript, jsx, rust, python, ruby, go, java, kotlin, swift,
  c, cpp, csharp, php, json, css, scss, less, markup, markdown, yaml, toml, bash, sql,
]) {
  refractor.register(lang as any);
}

const REGISTERED = new Set(refractor.listLanguages());

// react-diff-view 3.x expects `refractor.highlight` to return an array of hast nodes
// (the refractor v3 shape); refractor v5 returns a hast root, so unwrap `.children`.
export const refractorAdapter = {
  highlight: (text: string, language: string) =>
    (refractor.highlight(text, language) as any).children,
  listLanguages: () => refractor.listLanguages(),
};

const LANG_BY_EXT: Record<string, string> = {
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "jsx",
  rs: "rust",
  py: "python",
  rb: "ruby",
  go: "go",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  cs: "csharp",
  php: "php",
  json: "json",
  css: "css",
  scss: "scss",
  less: "less",
  html: "markup",
  xml: "markup",
  svg: "markup",
  md: "markdown",
  markdown: "markdown",
  yml: "yaml",
  yaml: "yaml",
  toml: "toml",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  sql: "sql",
};

// The refractor language id for a file path's extension, or null if unmapped/unregistered.
export function langFor(path: string): string | null {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const lang = LANG_BY_EXT[ext];
  return lang && REGISTERED.has(lang) ? lang : null;
}
