# nempm

The **Nemesis Package Manager**.

A desktop app (Windows, macOS, Linux) that lists the group's packages, downloads
them, and puts them onto a USB stick in the layout the Nemesis device reads. A
stick carries **one package at a time**; installing another replaces it once the
user confirms.

```text
Tool package stick                     Core stick (sets up a new device first)
  NEMESIS.data   from the release        ota_package.tar   the Core .tar, as released
  packages/<tool>.zip                    .nempm/manifest.json
  .nempm/manifest.json
```

`NEMESIS.data` is published with each tool's release and is written and removed
with it. A zip can optionally be unpacked straight into the drive root, keeping
its own folders. A release can also be just a zip that carries its own
`NEMESIS.data` at the top: that zip is always unpacked into the root. Removing
or swapping a package also removes the folders it leaves empty. The manifest in
`.nempm/` records what this app wrote, so the stick describes itself.

The package list comes from the [registry repo](../nempm_reg) (a static
`index.json` on GitHub Pages); packages are GitHub Release assets. There is no
server.

## Layout

```text
crates/nempm-core/   Rust core: index, drives, checks, install pipeline, eject. No UI deps.
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
[Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your
OS.

```bash
pnpm install
pnpm tauri dev          # the real app
pnpm dev                # UI only, in a browser, against the mock backend
```

### Try it end to end without GitHub

```bash
pnpm dev:registry       # serves sample packages + index.json on 127.0.0.1:8787
```

Then start the app with `NEMPM_INDEX_URL=http://127.0.0.1:8787/index.json` (or
paste that URL into **Settings → Index URL**), plug in a FAT32 or exFAT stick
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

The integration tests in `crates/nempm-core/tests/install_flow.rs` cover: a Core
stick and a tool stick (`NEMESIS.data` + zip) on FAT32 and exFAT layouts; a zip
that carries its own `NEMESIS.data` (always unpacked, and refused when it has
none); swapping packages only with consent; a failed download or an interrupted
swap leaving the old package intact; a corrupted cache file being detected and
downloaded again; wrong hashes; a dropped connection resuming with an HTTP Range
request; zip-slip archives; the FAT32 4 GB limit and low space (both checked
before downloading); updates replacing the data file too; untracked files
needing consent; profile migration; and the index ETag cache with offline
fallback.

## How it works

The UI never touches the disk or network. It calls these Tauri commands
(`src-tauri/src/commands.rs`) and listens for `progress` events:

| Command                                             | Does                                                                                                     |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `fetch_index` / `cached_index`                      | GET `index.json` with ETag caching; falls back to the saved copy offline                                 |
| `list_drives` / `inspect_folder`                    | Removable volumes (polled every 2 s), or any folder                                                      |
| `read_drive`                                        | Parse `.nempm/manifest.json`; installed list with available updates                                      |
| `check_drive` / `init_drive` / `fix_drive`          | Profile checks with safe fixes (never formats)                                                           |
| `plan_install` / `install` / `cancel_install`       | Plan (including a swap), download and verify, stage, swap out the old package, commit, read back, record |
| `uninstall`                                         | Delete exactly the recorded paths, data file included                                                    |
| `eject`                                             | Windows `CM_Request_Device_EjectW`, macOS `diskutil eject`, Linux `udisksctl`                            |
| `check_update` / `download_update` / `apply_update` | Self-update; refused while a drive is being written                                                      |

**Install pipeline:** pre-flight (slot allows the format, the package on the
stick may be replaced, free space + 5%, FAT32 4 GB limit) → the package and its
`NEMESIS.data` each re-hashed from the cache or downloaded to `<sha256>.part`
(hashed while streaming, resumed after drops); nothing on the stick changes
until both are verified → each file written to `<dest>.nempm-part` (a zip
unpacked into `.nempm/staging`, after checking none of its files would overwrite
one this app didn't write) and flushed (`F_FULLFSYNC` on macOS) → the old
package removed (earlier, if old and new don't fit together) → everything
renamed (an unpacked zip merged file by file) into place → read back from the
device bypassing the OS cache and compared → drive manifest updated atomically.
A failed download or an interruption while staging leaves the old package
intact; pulling the stick later leaves a valid manifest and, at worst,
`.nempm-part` files that **Check drive → Clean up** removes.

**Safety rules** (enforced in Rust, not the UI): ids are lowercase letters and
digits separated by single `.` or `-`; every path from a profile, zip or
manifest is checked for `..`, absolute paths, characters FAT can't store,
reserved device names and the `.nempm` folder (sticks written before the rename
used `.pkgmgr/`; the app renames it on sight); uninstall refuses a manifest path
that escapes the drive; files the app didn't install are never replaced without
consent; only `https://` URLs (and redirects) are followed.

**Window material:** Liquid Glass on macOS 26+ (vibrancy on older macOS), Mica
on Windows 11, solid on Windows 10 and Linux. The frontend reads it from
`app_info` and sets `data-material` on `<html>`.

## Releasing

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

Installed apps check at launch and every 6 hours, download in the background and
offer **Restart**; an update never installs during a drive write.

`createUpdaterArtifacts` is switched on only by
`src-tauri/tauri.release.conf.json` in CI, so `pnpm tauri build` works locally
without the signing key.

## Platform notes

- **macOS:** the first write to a removable volume triggers a system permission
  prompt. The app explains this before the first install.
- **Linux:** drives under `/media/$USER` and `/run/media/$USER` are listed;
  eject needs `udisks2`.
- **Windows:** USB hard drives often report as fixed disks; turn on **Settings →
  Show all drives** to see them.
