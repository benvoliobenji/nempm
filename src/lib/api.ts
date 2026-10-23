// Typed access to the Rust commands. Inside the Tauri window this calls the
// real core; in a plain browser (`pnpm dev`) it falls back to an in-memory
// mock so the UI can be developed and previewed without a drive.

import type { AppInfo } from "@/bindings/AppInfo";
import type { CacheInfo } from "@/bindings/CacheInfo";
import type { CheckReport } from "@/bindings/CheckReport";
import type { DriveInfo } from "@/bindings/DriveInfo";
import type { DriveState } from "@/bindings/DriveState";
import type { FixAction } from "@/bindings/FixAction";
import type { IndexResult } from "@/bindings/IndexResult";
import type { InstallOptions } from "@/bindings/InstallOptions";
import type { InstallOutcome } from "@/bindings/InstallOutcome";
import type { InstallPlan } from "@/bindings/InstallPlan";
import type { Progress } from "@/bindings/Progress";
import type { Settings } from "@/bindings/Settings";
import type { UpdateInfo } from "@/bindings/UpdateInfo";
import type { UpdateProgress } from "@/bindings/UpdateProgress";

export interface Backend {
  appInfo(): Promise<AppInfo>;
  getSettings(): Promise<Settings>;
  setSettings(settings: Settings): Promise<Settings>;
  cachedIndex(): Promise<IndexResult | null>;
  fetchIndex(): Promise<IndexResult>;
  listDrives(): Promise<DriveInfo[]>;
  inspectFolder(path: string): Promise<DriveInfo>;
  readDrive(path: string): Promise<DriveState>;
  checkDrive(path: string, profile: string): Promise<CheckReport>;
  initDrive(path: string, profile: string): Promise<CheckReport>;
  fixDrive(path: string, profile: string, fixes: FixAction[]): Promise<CheckReport>;
  planInstall(path: string, profile: string, ids: string[], options: InstallOptions): Promise<InstallPlan>;
  install(path: string, profile: string, ids: string[], options: InstallOptions): Promise<InstallOutcome>;
  cancelInstall(): Promise<void>;
  uninstall(path: string, id: string): Promise<DriveState>;
  eject(path: string): Promise<void>;
  isBusy(): Promise<boolean>;
  cacheInfo(): Promise<CacheInfo>;
  clearCache(): Promise<number>;
  checkUpdate(): Promise<UpdateInfo | null>;
  downloadUpdate(): Promise<UpdateInfo>;
  applyUpdate(): Promise<void>;
  chooseFolder(): Promise<string | null>;
  openUrl(url: string): Promise<void>;
  onProgress(cb: (p: Progress) => void): Promise<() => void>;
  onUpdateProgress(cb: (p: UpdateProgress) => void): Promise<() => void>;
}

export const isTauri = (): boolean => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const opener = await import("@tauri-apps/plugin-opener");
  return {
    appInfo: () => invoke("app_info"),
    getSettings: () => invoke("get_settings"),
    setSettings: (settings) => invoke("set_settings", { settings }),
    cachedIndex: () => invoke("cached_index"),
    fetchIndex: () => invoke("fetch_index"),
    listDrives: () => invoke("list_drives"),
    inspectFolder: (path) => invoke("inspect_folder", { path }),
    readDrive: (path) => invoke("read_drive", { path }),
    checkDrive: (path, profile) => invoke("check_drive", { path, profile }),
    initDrive: (path, profile) => invoke("init_drive", { path, profile }),
    fixDrive: (path, profile, fixes) => invoke("fix_drive", { path, profile, fixes }),
    planInstall: (path, profile, ids, options) => invoke("plan_install", { path, profile, ids, options }),
    install: (path, profile, ids, options) => invoke("install", { path, profile, ids, options }),
    cancelInstall: () => invoke("cancel_install"),
    uninstall: (path, id) => invoke("uninstall", { path, id }),
    eject: (path) => invoke("eject", { path }),
    isBusy: () => invoke("is_busy"),
    cacheInfo: () => invoke("cache_info"),
    clearCache: () => invoke("clear_cache"),
    checkUpdate: () => invoke("check_update"),
    downloadUpdate: () => invoke("download_update"),
    applyUpdate: () => invoke("apply_update"),
    chooseFolder: async () => {
      const picked = await dialog.open({ directory: true, multiple: false, title: "Choose the drive or folder" });
      return typeof picked === "string" ? picked : null;
    },
    openUrl: (url) => opener.openUrl(url),
    onProgress: (cb) => listen<Progress>("progress", (e) => cb(e.payload)),
    onUpdateProgress: (cb) => listen<UpdateProgress>("update-progress", (e) => cb(e.payload)),
  };
}

let backend: Promise<Backend> | null = null;

export function getBackend(): Promise<Backend> {
  backend ??= isTauri() ? tauriBackend() : import("./mock").then((m) => m.createMockBackend());
  return backend;
}

/** `api.listDrives()` etc., resolving the backend lazily. */
export const api = new Proxy({} as Backend, {
  get(_target, prop: keyof Backend) {
    return async (...args: unknown[]) => {
      const b = await getBackend();
      const fn = b[prop] as (...a: unknown[]) => Promise<unknown>;
      return fn(...args);
    };
  },
});
