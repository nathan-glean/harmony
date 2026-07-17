#!/usr/bin/env node
// Deliberate semantic-version bump for harmony.
//
//   node scripts/release.mjs <patch|minor|major|X.Y.Z>
//   (usually via `task release -- <patch|minor|major>`)
//
// Bumps the version in every place it lives — kept in lockstep so the git tag, the bundle, and the
// updater manifest never disagree — then commits "Release vX.Y.Z" and tags vX.Y.Z. It does NOT push;
// review, then run the printed `git push --follow-tags`. That tag push fires .github/workflows/release.yml.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const p = (rel) => join(ROOT, rel);

// The authoritative version source (what the bundle + updater manifest use).
const TAURI_CONF = "app/src-tauri/tauri.conf.json";

function die(msg) {
  console.error(`release: ${msg}`);
  process.exit(1);
}

function git(...args) {
  return execFileSync("git", args, { cwd: ROOT, encoding: "utf8" }).trim();
}

function readJSON(rel) {
  return JSON.parse(readFileSync(p(rel), "utf8"));
}

function writeJSON(rel, obj) {
  writeFileSync(p(rel), JSON.stringify(obj, null, 2) + "\n");
}

// Replace only the FIRST match (regex is non-global) and require it to change something.
function replaceOnce(rel, regex, repl) {
  const before = readFileSync(p(rel), "utf8");
  const after = before.replace(regex, repl);
  if (after === before) die(`no version match in ${rel} (pattern ${regex})`);
  writeFileSync(p(rel), after);
}

function nextVersion(cur, bump) {
  if (/^\d+\.\d+\.\d+$/.test(bump)) return bump; // explicit version
  const [maj, min, pat] = cur.split(".").map(Number);
  switch (bump) {
    case "major":
      return `${maj + 1}.0.0`;
    case "minor":
      return `${maj}.${min + 1}.0`;
    case "patch":
      return `${maj}.${min}.${pat + 1}`;
    default:
      die(`invalid bump "${bump}" — use patch | minor | major | X.Y.Z`);
  }
}

// ---- guards ----
const bump = process.argv[2];
if (!bump) die("usage: node scripts/release.mjs <patch|minor|major|X.Y.Z>");
if (git("status", "--porcelain") !== "") {
  die("working tree is not clean — commit or stash first so the Release commit is just the bump");
}
const branch = git("rev-parse", "--abbrev-ref", "HEAD");
if (branch !== "main") {
  console.warn(`release: warning — on branch "${branch}", not "main"`);
}

const current = readJSON(TAURI_CONF).version;
if (!/^\d+\.\d+\.\d+$/.test(current)) die(`current version "${current}" in ${TAURI_CONF} is not X.Y.Z`);
const version = nextVersion(current, bump);
if (version === current) die(`version is already ${version}`);
const tag = `v${version}`;
try {
  git("rev-parse", "--verify", "--quiet", `refs/tags/${tag}`);
  die(`tag ${tag} already exists`);
} catch {
  /* good — tag is free */
}

// ---- bump every version spot in lockstep ----
// JSON: tauri.conf.json + package.json (single `.version`); package-lock.json (top-level + root pkg).
for (const rel of [TAURI_CONF, "app/package.json"]) {
  const j = readJSON(rel);
  j.version = version;
  writeJSON(rel, j);
}
const lock = readJSON("app/package-lock.json");
lock.version = version;
if (lock.packages && lock.packages[""]) lock.packages[""].version = version;
writeJSON("app/package-lock.json", lock);

// TOML: the `[package]` version is the first `version = "..."` line in each manifest.
for (const rel of ["app/src-tauri/Cargo.toml", "core/Cargo.toml"]) {
  replaceOnce(rel, /^version = "[^"]*"/m, `version = "${version}"`);
}

// Cargo.lock: the `version` line right after each workspace crate's `name`.
for (const crate of ["harmony-app", "harmony-core"]) {
  replaceOnce("Cargo.lock", new RegExp(`(name = "${crate}"\\nversion = )"[^"]*"`), `$1"${version}"`);
}

// ---- commit + tag (no push) ----
git("add", "-A");
git("commit", "-m", `Release ${tag}`);
git("tag", tag);

console.log(`\n✓ Bumped ${current} → ${version}, committed, and tagged ${tag}.`);
console.log(`  Review, then publish the release with:\n`);
console.log(`      git push --follow-tags origin ${branch}\n`);
