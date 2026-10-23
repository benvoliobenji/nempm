// In-memory stand-in for the Rust core, used only when the UI runs in a plain
// browser. It mimics the real commands closely enough to exercise every
// screen: drives come and go, a stick holds one package at a time, installs
// report each stage, and errors look real.

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
const DATA = "NEMESIS.data";

const tool = (p: Partial<Package> & Pick<Package, "id" | "name" | "size">): Package => {
  const version = p.version ?? "1.0.0";
  const release = `https://github.com/example/${p.id}/releases/download/v${version}`;
  return {
    description: "",
    version,
    format: "zip",
    core: false,
    url: `${release}/${p.id}.zip`,
    sha256: sha(p.size),
    data: { url: `${release}/${DATA}`, sha256: sha(p.size + 1), size: 4096 },
    extract: false,
    slot: "packages",
    profiles: ["nemesis"],
    requires: ["nemesis-core"],
    ...p,
  };
};

const index: Index = {
  schemaVersion: 1,
  generatedAt: new Date(Date.now() - 3 * 3600_000).toISOString(),
  profiles: [
    {
      id: "nemesis",
      name: "Nemesis",
      version: 3,
      filesystems: ["exfat", "fat32"],
      maxFileBytes: { fat32: 4294967295 },
      maxPackages: 1,
      requiredDirs: [],
      slots: {
        core: { path: "", allow: ["tar"], extract: "never" },
        packages: {
          path: "packages",
          file: "{id}.zip",
          allow: ["zip"],
          extract: "optional",
          extractDir: "",
          dataFile: DATA,
        },
      },
      naming: { pattern: "^[a-z0-9]+(?:[.-][a-z0-9]+)*$", case: "lower" },
      reserved: [".nempm"],
    },
  ],
  packages: [
    {
      id: "nemesis-core",
      name: "Nemesis Core",
      description: "The Core system. Put it on a stick by itself to set up a new device.",
      version: "1.1.0",
      format: "tar",
      core: true,
      url: "https://github.com/example/hmg_nemesis/releases/download/v1.1.0/ota_package.tar",
      sha256: sha(174080),
      size: 174080,
      extract: false,
      slot: "core",
      profiles: ["nemesis"],
      requires: [],
      homepage: "https://github.com/example/hmg_nemesis",
    },
    tool({ id: "map-pack", name: "Map Pack", description: "Offline maps for every supported region.", size: 820 * MB, version: "2.3.0" }),
    tool({ id: "audio.tools", name: "Audio Tools", description: "Recording, playback and level meters.", size: 46 * MB, version: "1.1.0" }),
    tool({ id: "diagnostics", name: "Diagnostics", description: "Self-tests and log collection for support.", size: 12 * MB, extract: true }),
    tool({ id: "firmware-helper", name: "Firmware Helper", description: "Checks and stages firmware updates.", size: 98 * MB, version: "0.9.2" }),
    tool({ id: "themes", name: "Themes", description: "Extra colour themes and fonts for the device UI.", size: 5.2 * MB }),
    // A release that is just a zip with the stick layout, NEMESIS.data included.
    tool({
      id: "starter-kit",
      name: "Starter Kit",
      description: "Launcher scripts, packed as a ready-made stick.",
      size: 75 * 1024,
      extract: true,
      data: undefined,
    }),
  ],
};

interface MockDrive {
  info: DriveInfo;
  manifest: DriveManifest | null;
  untracked: Set<string>;
}

const byId = (id: string) => index.packages.find((p) => p.id === id)!;
const total = (p: Package) => p.size + (p.data?.size ?? 0);

function targets(p: Package, extract: boolean): { target: string; data?: string } {
  if (p.slot === "core") return { target: decodeURIComponent(p.url.split("/").pop()!) };
  // Unpacked, the zip's own folders land in the drive root.
  return { target: extract ? "" : `packages/${p.id}.zip`, data: DATA };
}

function record(p: Package, version: string, extract = false): InstalledPackage {
  const { target, data } = targets(p, extract);
  return {
    id: p.id,
    version,
    sha256: p.sha256,
    slot: p.slot,
    format: p.format,
    extracted: extract,
    paths: data ? [data, extract ? `packages/${p.id}.inner.zip` : target] : [target],
    dirs: extract ? ["packages"] : [],
    data,
    size: total(p),
    installedAt: new Date(Date.now() - 86400_000 * 4).toISOString(),
  };
}

const drives: MockDrive[] = [
  {
    info: {
      mountPoint: "E:\\",
      label: "NEMESIS",
      filesystem: "exfat",
      filesystemRaw: "exFAT",
      totalBytes: 32 * GB,
      freeBytes: 31.9 * GB,
      removable: true,
      readOnly: false,
    },
    manifest: {
      schemaVersion: 1,
      profile: { id: "nemesis", version: 2 },
      installed: [record(byId("audio.tools"), "1.0.0")],
      writtenBy: "nempm 0.1.0",
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
    untracked: new Set([DATA]),
  },
];

/** Folders picked with Choose folder; not listed as drives. */
const folders: MockDrive[] = [];

let settings: Settings = {
  theme: "system",
  indexUrl: "https://benvoliobenji.github.io/nempm_reg/index.json",
  updateChannel: "stable",
  showAllDrives: false,
  writePermissionExplained: false,
  lastProfile: "nemesis",
};

const listeners = new Set<(p: Progress) => void>();
const emit = (p: Progress) => listeners.forEach((l) => l(p));
let busy = false;
let cancelled = false;
let cacheBytes = 0.9 * GB;
const cached = new Set<string>(["themes"]);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const fail = (kind: string, message: string) => Promise.reject({ kind, message });

function drive(path: string): MockDrive {
  const d = [...drives, ...folders].find((x) => x.info.mountPoint === path);
  if (!d) throw { kind: "invalidData", message: `Can't open ${path}: the drive was removed.` };
  return d;
}

function check(d: MockDrive): CheckReport {
  const issues: Issue[] = [];
  const profile = index.profiles[0]!;
  if (!profile.filesystems.includes(d.info.filesystem)) {
    issues.push({ code: "filesystemNotAllowed", severity: "error", message: `This drive is ${d.info.filesystemRaw}; Nemesis needs exFAT or FAT32.` });
  }
  if (!d.manifest) {
    issues.push({ code: "notInitialized", severity: "warning", message: "This drive isn't set up for Nemesis yet.", fix: "initialize" });
  }
  for (const path of d.untracked) {
    issues.push({
      code: "untrackedFile",
      severity: "warning",
      message: `${path} is already on the drive but wasn't put there by this app. Installing a package will ask before replacing it.`,
      path,
    });
  }
  return {
    profile: { id: "nemesis", version: 2 },
    drive: d.info,
    issues,
    ready: !!d.manifest && !issues.some((i) => i.severity === "error"),
  };
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
  const m = d.manifest;
  if (!m) throw { kind: "notInitialized", message: "This drive hasn't been set up yet. Run Check drive and choose Initialize." };
  if (ids.length > 1) throw { kind: "profile", message: "A Nemesis drive holds 1 package at a time. Pick one." };
  const p = index.packages.find((x) => x.id === ids[0]);
  if (!p) throw { kind: "unknownPackage", message: `Unknown package: ${ids[0]}` };
  const removes = m.installed
    .filter((r) => r.id !== p.id)
    .map((r) => ({ id: r.id, name: byId(r.id)?.name ?? r.id, version: r.version, paths: r.paths }));
  const rec = m.installed.find((r) => r.id === p.id);
  // Without a data asset the zip carries NEMESIS.data, so it is always unpacked.
  const dataInZip = p.slot === "packages" && !p.data;
  const extract = p.format === "zip" && (dataInZip || (options.extract[p.id] ?? p.extract));
  const cmp = rec ? compareVersions(p.version, rec.version) : 1;
  if (rec && cmp === 0 && !options.reinstall && rec.extracted === extract) {
    return { steps: [], removes, upToDate: [p.id], totalBytes: 0, untracked: [] };
  }
  const { target, data } = targets(p, extract);
  const untracked = [target, ...(data ? [data] : [])].filter((t) => d.untracked.has(t));
  const step: PlanStep = {
    id: p.id,
    name: p.name,
    version: p.version,
    fromVersion: rec?.version,
    action: !rec ? "install" : cmp > 0 ? "update" : cmp < 0 ? "downgrade" : "reinstall",
    slot: p.slot,
    extract,
    size: total(p),
    target,
    dataTarget: data,
    dataInZip,
    untracked: untracked.length > 0,
  };
  return { steps: [step], removes, upToDate: [], totalBytes: total(p), untracked };
}

async function runStage(id: string, stage: Stage, done: number, size: number, grand: number, ms: number) {
  const ticks = Math.max(4, Math.round(ms / 120));
  for (let i = 0; i <= ticks; i++) {
    if (cancelled) throw { kind: "cancelled", message: "Cancelled" };
    emit({ id, stage, bytesDone: done + Math.round((size * i) / ticks), bytesTotal: grand });
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
      defaultIndexUrl: "https://benvoliobenji.github.io/nempm_reg/index.json",
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
      d.manifest ??= { schemaVersion: 1, profile: { id: "nemesis", version: 2 }, installed: [], writtenBy: "nempm 0.1.0" };
      return check(d);
    },
    fixDrive: async (path, _profile, fixes) => {
      const d = drive(path);
      if (fixes.includes("initialize")) {
        d.manifest ??= { schemaVersion: 1, profile: { id: "nemesis", version: 2 }, installed: [], writtenBy: "nempm 0.1.0" };
      }
      return check(d);
    },
    planInstall: async (path, _profile, ids, options) => plan(drive(path), ids, options),
    install: async (path, _profile, ids, options) => {
      if (busy) return fail("busy", "Another drive operation is still running. Wait for it to finish.");
      const d = drive(path);
      const p = plan(d, ids, options);
      if (p.removes.length && !options.replaceInstalled) {
        return fail("occupied", `This drive already holds ${p.removes.map((r) => r.name).join(", ")}, and it only takes one package at a time. Confirm replacing it.`);
      }
      if (p.untracked.length && !options.replaceUntracked) {
        return fail("conflict", `${p.untracked.join(", ")} already exists on the drive and wasn't installed by this app`);
      }
      busy = true;
      cancelled = false;
      const installed: InstalledPackage[] = [];
      p.steps.forEach((s) => emit({ id: s.id, stage: "queued", bytesDone: 0, bytesTotal: s.size }));
      try {
        for (const step of p.steps) {
          const pk = byId(step.id);
          const grand = total(pk);
          const dataSize = pk.data?.size ?? 0;
          try {
            if (!cached.has(pk.id)) {
              if (dataSize) await runStage(pk.id, "download", 0, dataSize, grand, 300);
              await runStage(pk.id, "download", dataSize, pk.size, grand, 1600);
              cached.add(pk.id);
              cacheBytes += grand;
            }
            await runStage(pk.id, "verify", 0, grand, grand, 500);
            if (step.extract) {
              if (dataSize) await runStage(pk.id, "copy", 0, dataSize, dataSize, 300);
              await runStage(pk.id, "extract", 0, pk.size, pk.size, 1400);
            } else {
              await runStage(pk.id, "copy", 0, grand, grand, 1400);
            }
            await runStage(pk.id, "readback", 0, grand, grand, 700);
          } catch (err) {
            emit({ id: pk.id, stage: "error", bytesDone: 0, bytesTotal: 0, message: (err as { message: string }).message });
            throw err;
          }
          const m = d.manifest!;
          for (const r of m.installed) d.info.freeBytes += r.size;
          m.installed = [{ ...record(pk, pk.version, step.extract), installedAt: new Date().toISOString() }];
          d.untracked.delete(step.target);
          if (step.dataTarget) d.untracked.delete(step.dataTarget);
          d.info.freeBytes -= grand;
          installed.push(m.installed[0]!);
          emit({ id: pk.id, stage: "done", bytesDone: grand, bytesTotal: grand });
        }
        if (!p.steps.length) d.manifest!.installed = d.manifest!.installed.filter((r) => ids.includes(r.id));
      } finally {
        busy = false;
      }
      return { installed, removed: p.removes.map((r) => r.id), upToDate: p.upToDate };
    },
    cancelInstall: async () => {
      cancelled = true;
    },
    uninstall: async (path, id) => {
      const d = drive(path);
      const m = d.manifest;
      if (!m) return fail("notInitialized", "This drive hasn't been set up yet.");
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
    cacheInfo: async () => ({ path: "C:\\Users\\you\\AppData\\Local\\io.github.benvoliobenji.nempm", bytes: cacheBytes }),
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
