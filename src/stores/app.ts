import { create } from "zustand";
import type { DriveInfo } from "@/bindings/DriveInfo";
import type { InstallOptions } from "@/bindings/InstallOptions";
import type { InstallPlan } from "@/bindings/InstallPlan";

export type View = "browse" | "installed" | "queue" | "settings";

/** An install waiting for the user to confirm its plan. */
export interface PendingConfirm {
  plan: InstallPlan;
  ids: string[];
  options: InstallOptions;
  drive: string;
  driveLabel: string;
  profile: string;
}

interface AppStore {
  view: View;
  setView: (view: View) => void;
  /** Mount point (or chosen folder) of the selected drive. */
  drivePath: string | null;
  setDrivePath: (path: string | null) => void;
  /** A folder picked with Choose folder, kept in the drive list. */
  customDrive: DriveInfo | null;
  setCustomDrive: (drive: DriveInfo | null) => void;
  profileId: string | null;
  setProfileId: (id: string | null) => void;
  search: string;
  setSearch: (search: string) => void;
  detailId: string | null;
  openDetail: (id: string | null) => void;
  /** Per-package extract choice; absent means the package default. */
  extract: Record<string, boolean>;
  setExtract: (id: string, value: boolean) => void;
  confirm: PendingConfirm | null;
  setConfirm: (confirm: PendingConfirm | null) => void;
}

export const useApp = create<AppStore>((set) => ({
  view: "browse",
  setView: (view) => set({ view }),
  drivePath: null,
  setDrivePath: (drivePath) => set({ drivePath }),
  customDrive: null,
  setCustomDrive: (customDrive) => set({ customDrive }),
  profileId: null,
  setProfileId: (profileId) => set({ profileId }),
  search: "",
  setSearch: (search) => set({ search }),
  detailId: null,
  openDetail: (detailId) => set({ detailId }),
  extract: {},
  setExtract: (id, value) => set((s) => ({ extract: { ...s.extract, [id]: value } })),
  confirm: null,
  setConfirm: (confirm) => set({ confirm }),
}));
