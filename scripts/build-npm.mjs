// Assemble the npm packages for a release.
//
//   node scripts/build-npm.mjs <version> <artifacts-dir> <out-dir>
//
// <artifacts-dir> holds one directory per target, each containing the built
// binary — the layout actions/download-artifact produces.
//
// Produces one package per platform plus the launcher, all stamped with the
// same version. Version skew between the launcher's optionalDependencies and
// the packages they name is the failure mode this script exists to prevent:
// npm would silently install nothing and the launcher would report a missing
// binary.

import { cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [version, artifacts, outDir] = process.argv.slice(2);
if (!version || !artifacts || !outDir) {
  console.error("usage: build-npm.mjs <version> <artifacts-dir> <out-dir>");
  process.exit(1);
}

/** target triple → npm platform package, and the os/cpu npm matches on. */
const TARGETS = [
  { triple: "aarch64-apple-darwin", pkg: "darwin-arm64", os: "darwin", cpu: "arm64" },
  { triple: "x86_64-apple-darwin", pkg: "darwin-x64", os: "darwin", cpu: "x64" },
  { triple: "aarch64-unknown-linux-gnu", pkg: "linux-arm64", os: "linux", cpu: "arm64" },
  { triple: "x86_64-unknown-linux-gnu", pkg: "linux-x64", os: "linux", cpu: "x64" },
  { triple: "aarch64-pc-windows-msvc", pkg: "win32-arm64", os: "win32", cpu: "arm64" },
  { triple: "x86_64-pc-windows-msvc", pkg: "win32-x64", os: "win32", cpu: "x64" },
];

const REPO = "https://github.com/Spiritsurge/rusty-figma-mcp";
const SCOPE = "@spiritsurge";
mkdirSync(outDir, { recursive: true });

const built = [];

for (const target of TARGETS) {
  const windows = target.os === "win32";
  const exe = windows ? "figma-mcp.exe" : "figma-mcp";
  const source = join(artifacts, target.triple, exe);

  if (!existsSync(source)) {
    // A missing target is reported and skipped rather than failing the run: a
    // release of five platforms beats no release because one runner died.
    console.warn(`  skip  ${target.pkg} — no binary at ${source}`);
    continue;
  }

  // Scoped on npm, flat on disk: a directory named for the scope would
  // nest, and the publish step globs these by prefix.
  const name = `${SCOPE}/figma-mcp-${target.pkg}`;
  const dir = join(outDir, `figma-mcp-${target.pkg}`);
  mkdirSync(join(dir, "bin"), { recursive: true });
  cpSync(source, join(dir, "bin", exe));

  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify(
      {
        name,
        version,
        description: `Prebuilt figma-mcp binary for ${target.os} ${target.cpu}.`,
        // npm reads these and installs this package only on a matching
        // machine, which is what makes the optionalDependencies trick work.
        os: [target.os],
        cpu: [target.cpu],
        files: ["bin/"],
        license: "MIT",
        repository: { type: "git", url: `git+${REPO}.git` },
        homepage: `${REPO}#readme`,
        // Scoped packages default to restricted. One private platform package
        // breaks installs on that platform only, which is a miserable bug to
        // find, so it is stated here as well as on the publish command.
        publishConfig: { access: "public" },
        preferUnplugged: true,
      },
      null,
      2,
    ) + "\n",
  );

  built.push(name);
  console.log(`  built ${name}`);
}

if (built.length === 0) {
  console.error("no binaries found — nothing to publish");
  process.exit(1);
}

// The launcher, with its optionalDependencies narrowed to what actually built
// and every version pinned to this release.
const launcherDir = join(outDir, "figma-mcp");
mkdirSync(join(launcherDir, "bin"), { recursive: true });
cpSync("npm/figma-mcp/bin/cli.js", join(launcherDir, "bin", "cli.js"));
cpSync("npm/figma-mcp/README.md", join(launcherDir, "README.md"));

const launcher = JSON.parse(readFileSync("npm/figma-mcp/package.json", "utf8"));
launcher.version = version;
launcher.optionalDependencies = Object.fromEntries(built.map((name) => [name, version]));
writeFileSync(join(launcherDir, "package.json"), JSON.stringify(launcher, null, 2) + "\n");

console.log(`  built ${launcher.name}@${version} → ${built.length} platform package(s)`);
