#!/usr/bin/env node
// The release tag is the version's source of truth. This keeps the three
// places that repeat it in step.
//
//   node scripts/version.mjs check            all three files agree
//   node scripts/version.mjs check v1.2.3     ...and match this tag (CI)
//   node scripts/version.mjs set 1.2.3        write it everywhere
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const files = {
  "package.json": join(root, "package.json"),
  "src-tauri/tauri.conf.json": join(root, "src-tauri", "tauri.conf.json"),
  "Cargo.toml": join(root, "Cargo.toml"),
};
const CARGO_RE = /(\[workspace\.package\][^[]*?\nversion\s*=\s*")([^"]+)(")/;
const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

function read() {
  return {
    "package.json": JSON.parse(readFileSync(files["package.json"], "utf8")).version,
    "src-tauri/tauri.conf.json": JSON.parse(readFileSync(files["src-tauri/tauri.conf.json"], "utf8")).version,
    "Cargo.toml": readFileSync(files["Cargo.toml"], "utf8").match(CARGO_RE)?.[2],
  };
}

const [command, arg] = process.argv.slice(2);

if (command === "check") {
  const versions = read();
  const distinct = new Set(Object.values(versions));
  const expected = arg?.replace(/^v/, "");
  for (const [file, v] of Object.entries(versions)) console.log(`${file}: ${v}`);
  if (distinct.size !== 1) {
    console.error("✗ The versions differ. Run: pnpm version:set <version>");
    process.exit(1);
  }
  if (expected && !distinct.has(expected)) {
    console.error(`✗ Tag ${arg} doesn't match version ${[...distinct][0]}. Run: pnpm version:set ${expected}`);
    process.exit(1);
  }
  console.log("✓ Versions agree.");
} else if (command === "set" && arg && SEMVER_RE.test(arg)) {
  const pkg = JSON.parse(readFileSync(files["package.json"], "utf8"));
  pkg.version = arg;
  writeFileSync(files["package.json"], `${JSON.stringify(pkg, null, 2)}\n`);
  const conf = JSON.parse(readFileSync(files["src-tauri/tauri.conf.json"], "utf8"));
  conf.version = arg;
  writeFileSync(files["src-tauri/tauri.conf.json"], `${JSON.stringify(conf, null, 2)}\n`);
  const cargo = readFileSync(files["Cargo.toml"], "utf8");
  writeFileSync(files["Cargo.toml"], cargo.replace(CARGO_RE, `$1${arg}$3`));
  console.log(`✓ Set version ${arg}. Commit, then tag v${arg} and push the tag to release.`);
} else {
  console.error("Usage: version.mjs check [vX.Y.Z] | set X.Y.Z");
  process.exit(2);
}
