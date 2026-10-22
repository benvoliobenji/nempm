# Nemesis Package Manager

A desktop app (Windows, macOS, Linux) that lists the group's packages, downloads
them, and installs them onto a USB drive in the layout the Nemesis device reads:

```
<drive root>/
  NEMESIS.data            core package (.tar, copied as-is, never unpacked)
  packages/<tool>.zip     one zip per tool (optionally unpacked to packages/<tool>/)
  .pkgmgr/manifest.json   what this app installed, so the drive describes itself
```

The package list comes from the [registry repo](../registry) (a static
`index.json` on GitHub Pages); packages are GitHub Release assets. There is no
server.

## Layout

```
crates/pkgmgr-core/   Rust core: index, drives, checks, install pipeline, eject. No UI deps.
  tests/              End-to-end tests (local HTTP server + temp folder as the drive)
  examples/           dev_registry: a local registry with real sample packages
src-tauri/            Tauri 2 shell: commands, settings, window material, updater
src/                  React 19 + TypeScript UI (shadcn-style components on Base UI, Tailwind v4)
  bindings/           TypeScript types generated from the Rust types (ts-rs). Don't edit.
  lib/mock.ts         In-browser stand-in for the core, used by `pnpm dev`
.github/workflows/    ci.yml (lint, test on 3 OSes), release.yml (v* tags)
scripts/              version.mjs, configure.mjs
```

## Develop

Prerequisites: Rust stable (1.90+), Node 20+, pnpm 10, and the
[Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your OS.

```bash
pnpm install
pnpm tauri dev          # the real app
pnpm dev                # UI only, in a browser, against the mock backend
```

### Try it end to end without GitHub

```bash
pnpm dev:registry       # serves sample packages + index.json on 127.0.0.1:8787
```

Then start the app with `PKGMGR_INDEX_URL=http://127.0.0.1:8787/index.json`
(or paste that URL into **Settings → Index URL**), plug in a FAT32 or exFAT stick
(or use **Choose folder** on the Drive panel), run **Check drive → Initialize**,
and install. Plain `http://` is only accepted for this machine (localhost).

### Tests and checks

```bash
cargo test --workspace          # 100+ tests, including the full install flow
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm lint && pnpm typecheck
pnpm bindings                   # regenerate src/bindings after changing Rust types
```

The integration tests in `crates/pkgmgr-core/tests/install_flow.rs` cover:
installing the core and a tool onto FAT32 and exFAT layouts; a corrupted cache
file being detected and downloaded again; a wrong hash; a connection dropped
mid-download resuming with an HTTP Range request; zip-slip archives; the FAT32
4 GB limit and low space (both checked before downloading); updates; untracked
files needing consent; an interrupted copy leaving the drive valid; profile
migration; and the index ETag cache with offline fallback.

## How it works

The UI never touches the disk or network. It calls these Tauri commands
(`src-tauri/src/commands.rs`) and listens for `progress` events:

| Command | Does |
|---|---|
| `fetch_index` / `cached_index` | GET `index.json` with ETag caching; falls back to the saved copy offline |
| `list_drives` / `inspect_folder` | Removable volumes (polled every 2 s), or any folder |
| `read_drive` | Parse `.pkgmgr/manifest.json`; installed list with available updates |
| `check_drive` / `init_drive` / `fix_drive` | Profile checks with safe fixes (never formats) |
| `plan_install` / `install` / `cancel_install` | Resolve deps (core first), download, verify, copy, read back, record |
| `uninstall` | Delete exactly the recorded paths |
| `eject` | Windows `CM_Request_Device_EjectW`, macOS `diskutil eject`, Linux `udisksctl` |
| `check_update` / `download_update` / `apply_update` | Self-update; refused while a drive is being written |

**Install pipeline, per package:** pre-flight (slot allows the format, free
space + 5%, FAT32 4 GB limit, uncompressed size for unpacking) → cached copy
re-hashed or downloaded to `<sha256>.part` (hashed while streaming, resumed
after drops) → written to `<dest>.pkgmgr-part` on the drive, flushed
(`F_FULLFSYNC` on macOS), renamed → read back from the device bypassing the
OS cache and compared → (zip) unpacked in `.pkgmgr/staging`, renamed into place
→ drive manifest updated atomically. Pulling the stick mid-copy leaves a valid
manifest and, at worst, a `.pkgmgr-part` file that **Check drive → Clean up**
removes.

**Safety rules** (enforced in Rust, not the UI): ids are `[a-z0-9-]`; every
path from a profile, zip or manifest is checked for `..`, absolute paths,
characters FAT can't store, reserved device names and the `.pkgmgr` folder;
uninstall refuses a manifest path that escapes the drive; files the app didn't
install are never replaced without consent; only `https://` URLs (and redirects)
are followed.

**Window material:** Liquid Glass on macOS 26+ (vibrancy on older macOS),
Mica on Windows 11, solid on Windows 10 and Linux. The frontend reads it from
`app_info` and sets `data-material` on `<html>`.

## Releasing

### One-time setup

1. Push this repo to GitHub (e.g. `OWNER/pkgmgr-app`) and fill in the owner:
   ```bash
   pnpm configure owner <github-owner>
   ```
   This updates the default index URL, the updater endpoints and the registry
   files next door.
2. Create the updater signing key. **Back up the private key and its password
   offline in two places: losing it means no installed copy can ever update.**
   ```bash
   pnpm tauri signer generate -w ~/.tauri/pkgmgr.key
   pnpm configure pubkey ~/.tauri/pkgmgr.key.pub
   ```
3. Repo **Settings → Secrets and variables → Actions**: add
   `TAURI_SIGNING_PRIVATE_KEY` (the key file's contents) and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
4. **Settings → General → Releases → Enable release immutability.**
5. **Settings → Pages → Source: GitHub Actions** (serves the beta channel's
   `latest.json`).

Optional OS code signing (separate from updater signing):

- **Windows** (Azure Artifact Signing, Basic plan): secrets `AZURE_CLIENT_ID`,
  `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`; variables `AZURE_SIGNING_ENDPOINT`,
  `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE`.
- **macOS** (Developer ID + notarization): secrets `APPLE_CERTIFICATE`
  (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
  `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID`.

Without them, releases still build; installers are just unsigned.

### Cutting a release

```bash
pnpm version:set 0.2.0
git commit -am "Release 0.2.0" && git tag v0.2.0 && git push --follow-tags
```

`release.yml` checks the tag matches the version, builds macOS (Apple silicon
and Intel), Windows and Linux (AppImage for self-update, plus .deb) as a draft,
publishes it once every platform has uploaded, and updates the beta channel.
Tags with a pre-release suffix (`v0.3.0-beta.1`) are published as prereleases:
only the Beta channel sees them.

Installed apps check at launch and every 6 hours, download in the background
and offer **Restart**; an update never installs during a drive write.

`createUpdaterArtifacts` is switched on only by `src-tauri/tauri.release.conf.json`
in CI, so `pnpm tauri build` works locally without the signing key.

## Platform notes

- **macOS:** the first write to a removable volume triggers a system permission
  prompt. The app explains this before the first install.
- **Linux:** drives under `/media/$USER` and `/run/media/$USER` are listed;
  eject needs `udisks2`.
- **Windows:** USB hard drives often report as fixed disks; turn on
  **Settings → Show all drives** to see them.
