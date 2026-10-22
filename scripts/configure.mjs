#!/usr/bin/env node
// One-time setup helpers.
//
//   node scripts/configure.mjs owner <github-owner>
//       Replaces the OWNER placeholder in the default index URL, the updater
//       endpoints and the docs (and in ../registry, if it sits next to this repo).
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
  "../registry/.github/CODEOWNERS",
  "../registry/schema/package.schema.json",
  "../registry/schema/profile.schema.json",
  "../registry/README.md",
];

if (command === "owner" && arg && /^[A-Za-z0-9-]+$/.test(arg)) {
  for (const rel of OWNER_FILES) {
    const file = join(root, rel);
    if (!existsSync(file)) continue;
    const text = readFileSync(file, "utf8");
    if (!text.includes("OWNER")) continue;
    writeFileSync(file, text.replaceAll("OWNER", arg));
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
