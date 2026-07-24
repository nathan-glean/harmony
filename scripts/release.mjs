#!/usr/bin/env node
// Deliberate semantic-version release for harmony — PR-based, so it lands against a branch-protected
// `main`.
//
//   node scripts/release.mjs <patch|minor|major|X.Y.Z>
//   (usually via `task release -- <patch|minor|major>`)
//
// Flow: bump the version in every place it lives (kept in lockstep so the git tag, the bundle, and
// the updater manifest never disagree) on a fresh `release-vX.Y.Z` branch, open a PR with `gh`, merge
// it, fast-forward local `main`, then tag the merged commit `vX.Y.Z` and push the tag — which fires
// .github/workflows/release.yml to build + publish the bundle. Unlike a direct push to `main`, this
// works when `main` is branch-protected.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const APP = join(ROOT, "app");
const p = (rel) => join(ROOT, rel);

// The authoritative version source (what the bundle + updater manifest use).
const TAURI_CONF = "app/src-tauri/tauri.conf.json";
// PR mergeability is computed asynchronously by GitHub, so a merge right after `gh pr create` often
// isn't ready yet — retry a few times before giving up.
const MAX_MERGE_ATTEMPTS = 10;
const MERGE_RETRY_MS = 3000;

function die(msg) {
  console.error(`release: ${msg}`);
  process.exit(1);
}

function git(...args) {
  return execFileSync("git", args, { cwd: ROOT, encoding: "utf8" }).trim();
}

// Run a command, capturing stdout/stderr and never throwing — returns { ok, out, err }.
function capture(cmd, args, opts = {}) {
  try {
    const out = execFileSync(cmd, args, {
      cwd: ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      ...opts,
    });
    return { ok: true, out: (out || "").trim(), err: "" };
  } catch (e) {
    return {
      ok: false,
      out: (e.stdout || "").toString().trim(),
      err: (e.stderr || e.message || "").toString().trim(),
    };
  }
}

// Synchronous sleep (no async in this straight-line script).
function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function firstLine(s) {
  return (s || "").split("\n").find((l) => l.trim() !== "") || "";
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

// ---- preconditions (loud, fail fast) ----
const bump = process.argv[2];
if (!bump) die("usage: node scripts/release.mjs <patch|minor|major|X.Y.Z>");

// `gh` is required to open + merge the release PR.
if (!capture("gh", ["--version"]).ok) {
  die("GitHub CLI `gh` is not installed or not on PATH — required to open + merge the release PR");
}
if (!capture("gh", ["auth", "status"]).ok) {
  die("`gh` is not authenticated — run `gh auth login` first");
}

// Must be on a clean `main`, in sync with origin, so the release branch is just the version bump.
const branch = git("rev-parse", "--abbrev-ref", "HEAD");
if (branch !== "main") die(`must be on "main" to release, but on "${branch}"`);
if (git("status", "--porcelain") !== "") {
  die("working tree is not clean — commit or stash first so the release branch is just the bump");
}
if (!capture("git", ["fetch", "origin", "main", "--tags"]).ok) {
  die("could not `git fetch origin` — check your network / remote");
}
const behind = git("rev-list", "--count", "HEAD..origin/main");
if (behind !== "0") die(`local main is ${behind} commit(s) behind origin/main — run \`git pull\` first`);
const ahead = git("rev-list", "--count", "origin/main..HEAD");
if (ahead !== "0") {
  die(`local main has ${ahead} unpushed commit(s) — push or reset them before releasing`);
}

// ---- compute the target version + tag ----
const current = readJSON(TAURI_CONF).version;
if (!/^\d+\.\d+\.\d+$/.test(current)) die(`current version "${current}" in ${TAURI_CONF} is not X.Y.Z`);
const version = nextVersion(current, bump);
if (version === current) die(`version is already ${version}`);
const tag = `v${version}`;
const relBranch = `release-${tag}`;

try {
  git("rev-parse", "--verify", "--quiet", `refs/tags/${tag}`);
  die(`tag ${tag} already exists locally`);
} catch {
  /* good — local tag is free */
}
if (git("ls-remote", "--tags", "origin", tag) !== "") die(`tag ${tag} already exists on origin`);
if (git("ls-remote", "--heads", "origin", relBranch) !== "") {
  die(`branch ${relBranch} already exists on origin — delete it or finish that release first`);
}
try {
  git("rev-parse", "--verify", "--quiet", `refs/heads/${relBranch}`);
  die(`branch ${relBranch} already exists locally — delete it (\`git branch -D ${relBranch}\`) first`);
} catch {
  /* good — local branch name is free */
}

// ---- bump every version spot in lockstep, on a fresh release branch ----
git("checkout", "-b", relBranch);

// JSON: tauri.conf.json + package.json (single `.version`).
for (const rel of [TAURI_CONF, "app/package.json"]) {
  const j = readJSON(rel);
  j.version = version;
  writeJSON(rel, j);
}

// package-lock.json: regenerate from the bumped package.json so it can't drift out of lockstep — CI's
// `npm ci` validates it strictly and fails on any mismatch. `--package-lock-only` rewrites only the
// lockfile (no node_modules install).
const lockGen = capture("npm", ["install", "--package-lock-only"], { cwd: APP, stdio: ["ignore", "inherit", "inherit"] });
if (!lockGen.ok) {
  die("`npm install --package-lock-only` failed — can't regenerate app/package-lock.json in lockstep");
}
const lock = readJSON("app/package-lock.json");
if (lock.version !== version || (lock.packages && lock.packages[""] && lock.packages[""].version !== version)) {
  die(`app/package-lock.json did not update to ${version} — lockfile would drift, aborting`);
}

// TOML: the `[package]` version is the first `version = "..."` line in each manifest.
for (const rel of ["app/src-tauri/Cargo.toml", "core/Cargo.toml"]) {
  replaceOnce(rel, /^version = "[^"]*"/m, `version = "${version}"`);
}

// Cargo.lock: the `version` line right after each workspace crate's `name`.
for (const crate of ["harmony-app", "harmony-core"]) {
  replaceOnce("Cargo.lock", new RegExp(`(name = "${crate}"\\nversion = )"[^"]*"`), `$1"${version}"`);
}

// ---- commit + push the release branch ----
git("add", "-A");
git("commit", "-m", `Release ${tag}`);
if (!capture("git", ["push", "-u", "origin", relBranch]).ok) {
  die(`could not push ${relBranch} to origin`);
}

// ---- open the release PR ----
const prBody =
  `Automated version bump **${current} → ${version}** in lockstep: ` +
  `\`${TAURI_CONF}\`, both \`Cargo.toml\`, \`app/package.json\`, \`Cargo.lock\`, and ` +
  `\`app/package-lock.json\`.\n\nMerging this and pushing the \`${tag}\` tag builds + publishes the ` +
  `release (\`.github/workflows/release.yml\`). Opened by \`scripts/release.mjs\`.`;
const created = capture("gh", [
  "pr", "create",
  "--base", "main",
  "--head", relBranch,
  "--title", `Release ${tag}`,
  "--body", prBody,
]);
if (!created.ok) die(`\`gh pr create\` failed: ${created.err || created.out}`);
const prUrl = (created.out.split("\n").find((l) => l.startsWith("http")) || relBranch).trim();
console.log(`release: opened ${prUrl}`);

// ---- merge (retry: mergeability lags right after creation) ----
let merged = false;
for (let attempt = 1; attempt <= MAX_MERGE_ATTEMPTS; attempt++) {
  const r = capture("gh", ["pr", "merge", prUrl, "--squash", "--admin", "--delete-branch"]);
  if (r.ok) {
    merged = true;
    break;
  }
  console.warn(
    `release: merge attempt ${attempt}/${MAX_MERGE_ATTEMPTS} not ready (${firstLine(r.err || r.out)}); retrying in ${MERGE_RETRY_MS / 1000}s…`
  );
  sleep(MERGE_RETRY_MS);
}
if (!merged) {
  die(
    `could not merge ${prUrl} after ${MAX_MERGE_ATTEMPTS} attempts. Merge it yourself, then on main run:\n` +
      `      git pull --ff-only && git tag ${tag} && git push origin ${tag}`
  );
}

// ---- fast-forward main to the merged commit, then tag it ----
git("checkout", "main");
if (!capture("git", ["fetch", "origin", "main"]).ok) die("could not fetch origin/main after merge");
try {
  git("merge", "--ff-only", "origin/main");
} catch {
  die("could not fast-forward main to origin/main after merge — resolve manually, then tag + push");
}
const landed = readJSON(TAURI_CONF).version;
if (landed !== version) die(`main landed at version ${landed}, expected ${version} — not tagging`);

git("tag", tag);
if (!capture("git", ["push", "origin", tag]).ok) {
  die(`tagged ${tag} locally but the tag push failed — run \`git push origin ${tag}\` to publish`);
}

// Best-effort local cleanup of the merged release branch (remote copy was deleted by --delete-branch).
capture("git", ["branch", "-D", relBranch]);

console.log(`\n✓ Released ${current} → ${version}: PR merged, main fast-forwarded, and ${tag} pushed.`);
console.log(`  The tag push is building the release — watch .github/workflows/release.yml.\n`);
