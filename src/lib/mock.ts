// In-memory stand-in for the Rust core, used only when the UI runs in a plain
// browser. It mimics the real commands closely enough to exercise every
// screen: drives come and go, installs report each stage, errors look real.

import type { Backend } from "./api";
import type { CheckReport } from "@/bindings/CheckReport";
import type { DriveInfo } from "@/bindings/DriveInfo";
import type { DriveManifest } from "@/bindings/DriveManifest";
import type { DriveState } from "@/bindings/DriveState";
import type { Index } from "@/bindings/Index";
import type { InstallPlan } from "@/bindings/InstallPlan";
import type { InstalledPackage } from "@/bindings/InstalledPackage";
import type { Issue } from "@/bindings/Issue";
import type { Package } from "@/bindings/Package";
import type { PlanStep } from "@/bindings/PlanStep";
import type { Progress } from "@/bindings/Progress";
import type { Settings } from "@/bindings/Settings";
import type { Stage } from "@/bindings/Stage";
import { compareVersions } from "./utils";

const GB = 1_000_000_000;
const MB = 1_000_000;
const sha = (n: number) => n.toString(16).padStart(64, "a");

const pkg = (p: Partial<Package> & Pick<Package, "id" | "name" | "size">): Package => ({
  description: "",
  version: "1.0.0",
  format: "zip",
  core: false,
  url: `https://github.com/example/${p.id}/releases/download/v1.0.0/${p.id}.zip`,
  sha256: sha(p.size),
  extract: false,
  slot: "packages",
  profiles: ["nemesis"],
  deps: ["nemesis-core"],
  ...p,
});

const index: Index = {
  schemaVersion: 1,
  generatedAt: new Date(Date.now() - 3 * 3600_000).toISOString(),
  profiles: [
    {
      id: "nemesis",
      name: "Nemesis",
      version: 1,
      filesystems: ["exfat", "fat32"],
      maxFileBytes: { fat32: 4294967295 },
      requiredDirs: ["packages"],
      slots: {
        core: { path: "", file: "NEMESIS.data", allow: ["tar"], extract: "never", max: 1 },
        packages: { path: "packages", file: "{id}.zip", allow: ["zip"], extract: "optional", extractDir: "packages/{id}" },
      },
      naming: { pattern: "^[a-z0-9-]+$", case: "lower" },
      reserved: [".pkgmgr"],
    },
  ],
  packages: [
    pkg({
      id: "nemesis-core",
      name: "Nemesis Core",
      description: "The core data the device boots from. Written to the drive root as NEMESIS.data.",
      version: "1.1.0",
      format: "tar",
      core: true,
      slot: "core",
      deps: [],
      size: 1.4 * GB,
      url: "https://github.com/example/nemesis-core/releases/download/v1.1.0/nemesis-core.tar",
      homepage: "https://github.com/example/nemesis-core",
    }),
    pkg({ id: "map-pack", name: "Map Pack", description: "Offline maps for every supported region.", size: 820 * MB, version: "2.3.0" }),
    pkg({ id: "audio-tools", name: "Audio Tools", description: "Recording, playback and level meters.", size: 46 * MB }),
    pkg({ id: "diagnostics", name: "Diagnostics", description: "Self-tests and log collection for support.", size: 12 * MB, extract: true }),
    pkg({ id: "firmware-helper", name: "Firmware Helper", description: "Checks and stages firmware updates.", size: 98 * MB, version: "0.9.2" }),
    pkg({ id: "themes", name: "Themes", description: "Extra colour themes and fonts for the device UI.", size: 5.2 * MB, deps: ["nemesis-core", "audio-tools"] }),
  ],
};

interface MockDrive {
  info: DriveInfo;
  manifest: DriveManifest | null;
  untracked: Set<string>;
}

const record = (p: Package, version: string): InstalledPackage => ({
  id: p.id,
  version,
  sha256: p.sha256,
  slot: p.slot,
  format: p.format,
  extracted: false,
  paths: [p.slot === "core" ? "NEMESIS.data" : `packages/${p.id}.zip`],
  dirs: [],
  deps: p.deps,
  size: p.size,
  installedAt: new Date(Date.now() - 86400_000 * 4).toISOString(),
});

const byId = (id: string) => index.packages.find((p) => p.id === id)!;

const drives: MockDrive[] = [
  {
    info: {
      mountPoint: "E:\\",
      label: "NEMESIS",
      filesystem: "exfat",
      filesystemRaw: "exFAT",
      totalBytes: 32 * GB,
      freeBytes: 27.6 * GB,
      removable: true,
      readOnly: false,
    },
    manifest: {
      schemaVersion: 1,
      profile: { id: "nemesis", version: 1 },
      installed: [record(byId("nemesis-core"), "1.0.0"), record(byId("audio-tools"), "1.0.0")],
      writtenBy: "pkgmgr 0.1.0",
    },
    untracked: new Set(),
  },
  {
    info: {
      mountPoint: "F:\\",
      label: "KINGSTON",
      filesystem: "fat32",
      filesystemRaw: "FAT32",
      totalBytes: 16 * GB,
      freeBytes: 15.2 * GB,
      removable: true,
      readOnly: false,
    },
    manifest: null,
    untracked: new Set(["NEMESIS.data"]),
  },
];

let settings: Settings = {
  theme: "system",
  indexUrl: "https://OWNER.github.io/pkgmgr-registry/index.json",
  updateChannel: "stable",
  showAllDrives: false,
  writePermissionExplained: false,
  lastProfile: "nemesis",
};

const listeners = new Set<(p: Progress) => void>();
const emit = (p: Progress) => listeners.forEach((l) => l(p));
let busy = false;
let cancelled = false;
let cacheBytes = 1.2 * GB;
const cached = new Set<string>(["audio-tools"]);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const fail = (kind: string, message: string) => Promise.reject({ kind, message });

/** Folders picked with Choose folder; not listed as drives. */
const folders: MockDrive[] = [];

function drive(path: string): MockDrive {
  const d = [...drives, ...folders].find((x) => x.info.mountPoint === path);
  if (!d) throw { kind: "invalidData", message: `Can't open ${path}: the drive was removed.` };
  return d;
}

function targetOf(p: Package, extract: boolean) {
  if (p.slot === "core") return "NEMESIS.data";
  return extract ? `packages/${p.id}` : `packages/${p.id}.zip`;
}

function check(d: MockDrive): CheckReport {
  const issues: Issue[] = [];
  const profile = index.profiles[0]!;
  if (!profile.filesystems.includes(d.info.filesystem)) {
    issues.push({ code: "filesystemNotAllowed", severity: "error", message: `This drive is ${d.info.filesystemRaw}; Nemesis needs exFAT or FAT32.` });
  }
  if (!d.manifest) {
    issues.push({ code: "notInitialized", severity: "warning", message: "This drive isn't set up for Nemesis yet.", fix: "initialize" });
    issues.push({ code: "missingDir", severity: "error", message: 'The "packages" folder is missing.', path: "packages", fix: "initialize" });
  }
  for (const path of d.untracked) {
    issues.push({
      code: "untrackedFile",
      severity: "warning",
      message: `${path} is already on the drive but wasn't installed by this app. Installing Nemesis Core will ask before replacing it.`,
      path,
    });
  }
  if (d.info.filesystem === "fat32") {
    for (const p of index.packages.filter((p) => p.size > 4294967295)) {
      issues.push({ code: "tooLargeForFilesystem", severity: "warning", message: `${p.name} is too large for FAT32.` });
    }
  }
  return { profile: { id: "nemesis", version: 1 }, drive: d.info, issues, ready: !issues.some((i) => i.severity === "error") };
}

function readDrive(d: MockDrive): DriveState {
  if (!d.manifest) {
    return { path: d.info.mountPoint, status: "uninitialized", installed: [], updatesAvailable: 0, profileOutdated: false };
  }
  const installed = d.manifest.installed.map((rec) => {
    const p = index.packages.find((x) => x.id === rec.id);
    const updateAvailable = !!p && compareVersions(p.version, rec.version) > 0;
    return { record: rec, name: p?.name ?? rec.id, latest: p?.version, updateAvailable, inIndex: !!p, filesPresent: true };
  });
  return {
    path: d.info.mountPoint,
    status: "ready",
    profile: d.manifest.profile,
    installed,
    updatesAvailable: installed.filter((i) => i.updateAvailable).length,
    profileOutdated: false,
  };
}

function plan(d: MockDrive, ids: string[], options: { extract: Record<string, boolean>; reinstall: boolean }): InstallPlan {
  if (!d.manifest) throw { kind: "notInitialized", message: "This drive hasn't been set up yet. Run Check drive and choose Initialize." };
  const order: Package[] = [];
  const seen = new Set<string>();
  const visit = (id: string) => {
    if (seen.has(id)) return;
    seen.add(id);
    const p = index.packages.find((x) => x.id === id);
    if (!p) throw { kind: "unknownPackage", message: `Unknown package: ${id}` };
    p.deps.forEach(visit);
    order.push(p);
  };
  [...ids].sort((a, b) => Number(byId(b)?.core) - Number(byId(a)?.core)).forEach(visit);
  const steps: PlanStep[] = [];
  const upToDate: string[] = [];
  for (const p of order) {
    const rec = d.manifest.installed.find((r) => r.id === p.id);
    const requested = ids.includes(p.id);
    if (rec && !requested) continue;
    const cmp = rec ? compareVersions(p.version, rec.version) : 1;
    const extract = p.format === "zip" && (options.extract[p.id] ?? p.extract);
    if (rec && cmp === 0 && !options.reinstall && rec.extracted === extract) {
      upToDate.push(p.id);
      continue;
    }
    const target = targetOf(p, extract);
    steps.push({
      id: p.id,
      name: p.name,
      version: p.version,
      fromVersion: rec?.version,
      action: !rec ? "install" : cmp > 0 ? "update" : cmp < 0 ? "downgrade" : "reinstall",
      reason: requested ? "requested" : "dependency",
      slot: p.slot,
      extract,
      size: p.size,
      target,
      untracked: !rec && d.untracked.has(target),
    });
  }
  return {
    steps,
    upToDate,
    totalBytes: steps.reduce((n, s) => n + s.size, 0),
    untracked: steps.filter((s) => s.untracked).map((s) => s.target),
  };
}

async function runStage(id: string, stage: Stage, total: number, ms: number) {
  const ticks = Math.max(4, Math.round(ms / 120));
  for (let i = 0; i <= ticks; i++) {
    if (cancelled) throw { kind: "cancelled", message: "Cancelled" };
    emit({ id, stage, bytesDone: Math.round((total * i) / ticks), bytesTotal: total });
    await sleep(ms / ticks);
  }
}

export function createMockBackend(): Backend {
  return {
    appInfo: async () => ({
      version: "0.1.0",
      platform: navigator.userAgent.includes("Mac") ? "macos" : navigator.userAgent.includes("Win") ? "windows" : "linux",
      material: "solid",
      updaterConfigured: false,
      defaultIndexUrl: "https://OWNER.github.io/pkgmgr-registry/index.json",
    }),
    getSettings: async () => settings,
    setSettings: async (s) => (settings = s),
    cachedIndex: async () => ({ index, source: "cache", fetchedAt: index.generatedAt, offline: false, warnings: [] }),
    fetchIndex: async () => {
      await sleep(400);
      return { index, source: "network", fetchedAt: new Date().toISOString(), offline: false, warnings: [] };
    },
    listDrives: async () => drives.map((d) => d.info),
    inspectFolder: async (path) => {
      let folder = folders.find((f) => f.info.mountPoint === path);
      if (!folder) {
        folder = {
          info: {
            mountPoint: path,
            label: path.split(/[\\/]/).filter(Boolean).pop(),
            filesystem: "exfat",
            filesystemRaw: "exFAT",
            totalBytes: 64 * GB,
            freeBytes: 40 * GB,
            removable: false,
            readOnly: false,
          },
          manifest: null,
          untracked: new Set(),
        };
        folders.push(folder);
      }
      return folder.info;
    },
    readDrive: async (path) => readDrive(drive(path)),
    checkDrive: async (path) => {
      await sleep(250);
      return check(drive(path));
    },
    initDrive: async (path) => {
      const d = drive(path);
      d.manifest ??= { schemaVersion: 1, profile: { id: "nemesis", version: 1 }, installed: [], writtenBy: "pkgmgr 0.1.0" };
      return check(d);
    },
    fixDrive: async (path, _profile, fixes) => {
      const d = drive(path);
      if (fixes.includes("initialize")) {
        d.manifest ??= { schemaVersion: 1, profile: { id: "nemesis", version: 1 }, installed: [], writtenBy: "pkgmgr 0.1.0" };
      }
      return check(d);
    },
    planInstall: async (path, _profile, ids, options) => plan(drive(path), ids, options),
    install: async (path, _profile, ids, options) => {
      if (busy) return fail("busy", "Another drive operation is still running. Wait for it to finish.");
      const d = drive(path);
      const p = plan(d, ids, options);
      if (p.untracked.length && !options.replaceUntracked) {
        return fail("conflict", `${p.untracked.join(", ")} already exists on the drive and wasn't installed by this app`);
      }
      busy = true;
      cancelled = false;
      const installed: InstalledPackage[] = [];
      p.steps.forEach((s) => emit({ id: s.id, stage: "queued", bytesDone: 0, bytesTotal: s.size }));
      try {
        for (const [i, step] of p.steps.entries()) {
          const pk = byId(step.id);
          try {
            if (!cached.has(pk.id)) {
              await runStage(pk.id, "download", pk.size, 1600);
              cached.add(pk.id);
              cacheBytes += pk.size;
            }
            await runStage(pk.id, "verify", pk.size, 500);
            await runStage(pk.id, step.extract ? "extract" : "copy", pk.size, 1400);
            await runStage(pk.id, "readback", pk.size, 700);
          } catch (err) {
            const message = (err as { message: string }).message;
            emit({ id: pk.id, stage: "error", bytesDone: 0, bytesTotal: 0, message });
            for (const rest of p.steps.slice(i + 1)) {
              emit({ id: rest.id, stage: "error", bytesDone: 0, bytesTotal: 0, message: cancelled ? "Cancelled" : `Not installed because ${pk.name} failed` });
            }
            throw err;
          }
          const rec: InstalledPackage = { ...record(pk, pk.version), extracted: step.extract, installedAt: new Date().toISOString() };
          d.manifest!.installed = [...d.manifest!.installed.filter((r) => r.id !== pk.id), rec];
          d.untracked.delete(step.target);
          d.info.freeBytes -= pk.size;
          installed.push(rec);
          emit({ id: pk.id, stage: "done", bytesDone: pk.size, bytesTotal: pk.size });
        }
      } finally {
        busy = false;
      }
      return { installed, upToDate: p.upToDate };
    },
    cancelInstall: async () => {
      cancelled = true;
    },
    uninstall: async (path, id, force) => {
      const d = drive(path);
      const m = d.manifest;
      if (!m) return fail("notInitialized", "This drive hasn't been set up yet.");
      const dependents = m.installed.filter((r) => r.deps?.includes(id)).map((r) => r.id);
      if (dependents.length && !force) return fail("dependency", `${id} is needed by ${dependents.join(", ")}. Uninstall them first.`);
      const rec = m.installed.find((r) => r.id === id);
      m.installed = m.installed.filter((r) => r.id !== id);
      if (rec) d.info.freeBytes += rec.size;
      return readDrive(d);
    },
    eject: async (path) => {
      await sleep(600);
      const i = drives.findIndex((d) => d.info.mountPoint === path);
      if (i >= 0) drives.splice(i, 1);
    },
    isBusy: async () => busy,
    cacheInfo: async () => ({ path: "C:\\Users\\you\\AppData\\Local\\pkgmgr", bytes: cacheBytes }),
    clearCache: async () => {
      const freed = cacheBytes;
      cacheBytes = 0;
      cached.clear();
      return freed;
    },
    checkUpdate: async () => null,
    downloadUpdate: () => fail("invalidData", "No update to download"),
    applyUpdate: () => fail("invalidData", "No update to install"),
    chooseFolder: async () => "D:\\Test drive",
    openUrl: async (url) => {
      window.open(url, "_blank", "noopener");
    },
    onProgress: async (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    onUpdateProgress: async () => () => {},
  };
}
