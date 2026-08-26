// Compare this project's source against another implementation.
//
//   node scripts/similarity.mjs <path-to-other-project> [label]
//
// Reports, for each of our files, the closest file in the other project: the
// share of identical lines, and the longest run of consecutive identical ones.
//
// The second number is the one that matters. Scattered identical lines are
// shared vocabulary — imports, `return {`, closing braces, the same Figma API
// calls in the only order they can be called. A long *consecutive* run is what
// distinguishes copied code from two people solving the same problem.
//
// Lines are normalised before comparing: whitespace collapsed, blank and
// comment-only lines dropped. That makes the result look worse than reality,
// since comments are where independent authors differ most — a deliberate
// choice, because a check meant as evidence should not flatter its subject.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { extname, join, relative } from "node:path";

const SKIP = new Set([".git", "node_modules", "dist", "target", "build", ".vite"]);
const EXTS = new Set([".rs", ".ts", ".tsx", ".go", ".js", ".mjs"]);
const COMMENT = /^\s*(\/\/|#|\/\*|\*|--)/;

function normalise(path) {
  let raw;
  try {
    raw = readFileSync(path, "utf8");
  } catch {
    return [];
  }
  return raw
    .split(/\r?\n/)
    .filter((line) => !COMMENT.test(line))
    .map((line) => line.trim().replace(/\s+/g, " "))
    .filter((line) => line.length > 2);
}

function collect(root) {
  const files = new Map();
  const walk = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir);
    } catch {
      return;
    }
    for (const entry of entries) {
      if (SKIP.has(entry)) continue;
      const full = join(dir, entry);
      const stat = statSync(full, { throwIfNoEntry: false });
      if (!stat) continue;
      if (stat.isDirectory()) walk(full);
      else if (EXTS.has(extname(entry))) {
        const lines = normalise(full);
        if (lines.length >= 15) files.set(relative(root, full), lines);
      }
    }
  };
  walk(root);
  return files;
}

/** Longest run of consecutive identical lines, and total identical lines. */
function compare(a, b) {
  const index = new Map();
  b.forEach((line, i) => {
    if (!index.has(line)) index.set(line, []);
    index.get(line).push(i);
  });

  let previous = new Map();
  let longest = 0;
  let total = 0;

  for (const line of a) {
    const current = new Map();
    for (const j of index.get(line) ?? []) {
      const run = (previous.get(j - 1) ?? 0) + 1;
      current.set(j, run);
      if (run > longest) longest = run;
    }
    if (current.size > 0) total++;
    previous = current;
  }

  return { pct: (100 * total) / Math.max(1, Math.min(a.length, b.length)), longest };
}

const [target, label = target] = process.argv.slice(2);
if (!target) {
  console.error("usage: node scripts/similarity.mjs <path-to-other-project> [label]");
  process.exit(1);
}

const ours = collect(process.cwd());
const theirs = collect(target);

if (theirs.size === 0) {
  console.error(`no comparable source files found in ${target}`);
  process.exit(1);
}

const rows = [];
for (const [ourName, ourLines] of ours) {
  let best = { longest: 0, pct: 0, match: "" };
  for (const [theirName, theirLines] of theirs) {
    const { pct, longest } = compare(ourLines, theirLines);
    if (longest > best.longest || (longest === best.longest && pct > best.pct)) {
      best = { longest, pct, match: theirName };
    }
  }
  rows.push({ ...best, ourName });
}

rows.sort((x, y) => y.longest - x.longest || y.pct - x.pct);

console.log(`\n${label} — ${theirs.size} files compared against our ${ours.size}\n`);
console.log(`${"run".padStart(4)}  ${"ident".padStart(6)}  ${"our file".padEnd(34)} closest match`);
for (const row of rows.slice(0, 8)) {
  console.log(
    `${String(row.longest).padStart(4)}  ${row.pct.toFixed(1).padStart(5)}%  ` +
      `${row.ourName.padEnd(34)} ${row.match}`,
  );
}

const worst = rows[0];
console.log(
  `\nworst case: ${worst.longest} consecutive identical lines ` +
    `(${worst.ourName} vs ${worst.match})\n`,
);
