#!/usr/bin/env node
// One-time setup helpers.
//
//   node scripts/configure.mjs owner <github-owner>
//       Points the default index URL (<owner>.github.io/nempm_reg), the updater
//       endpoints (github.com/<owner>/nempm, <owner>.github.io/nempm) and the
//       docs at another GitHub owner, for a fork.
//
//   node scripts/configure.mjs pubkey <path-to-key.pub>
//       Puts the updater public key (from `pnpm tauri signer generate`) into
//       tauri.conf.json, which turns on update checks.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const [command, arg] = process.argv.slice(2);

const OWNER_FILES = [
  "src-tauri/tauri.conf.json",
  "src-tauri/src/settings.rs",
  "src-tauri/src/updates.rs",
  "src/lib/mock.ts",
  "README.md",
];

/** The owner the links point at now. */
function currentOwner() {
  const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
  return conf.plugins.updater.endpoints[0].match(/github\.com\/([^/]+)\//)?.[1];
}

if (command === "owner" && arg && /^[A-Za-z0-9-]+$/.test(arg)) {
  const from = currentOwner();
  if (!from) {
    console.error("✗ Couldn't find the current owner in tauri.conf.json.");
    process.exit(1);
  }
  for (const rel of OWNER_FILES) {
    const file = join(root, rel);
    if (!existsSync(file)) continue;
    const text = readFileSync(file, "utf8");
    const next = text
      .replaceAll(`github.com/${from}/`, `github.com/${arg}/`)
      .replaceAll(`${from}.github.io`, `${arg}.github.io`);
    if (next === text) continue;
    writeFileSync(file, next);
    console.log(`updated ${rel}`);
  }
  console.log(`✓ GitHub owner set to ${arg}.`);
} else if (command === "pubkey" && arg) {
  const key = readFileSync(arg, "utf8").trim();
  if (!key || key.includes("PRIVATE")) {
    console.error("✗ That doesn't look like a public key. Pass the .pub file, never the private key.");
    process.exit(1);
  }
  const confPath = join(root, "src-tauri", "tauri.conf.json");
  const conf = JSON.parse(readFileSync(confPath, "utf8"));
  conf.plugins.updater.pubkey = key;
  writeFileSync(confPath, `${JSON.stringify(conf, null, 2)}\n`);
  console.log("✓ Updater public key set. Update checks are now on.");
} else {
  console.error("Usage: configure.mjs owner <github-owner> | pubkey <path-to-key.pub>");
  process.exit(2);
}
