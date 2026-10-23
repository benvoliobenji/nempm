import { useCallback, useEffect } from "react";
import { toast } from "sonner";
import type { InstallOptions } from "@/bindings/InstallOptions";
import { api } from "@/lib/api";
import { errorMessage, toAppError } from "@/lib/errors";
import { useApp } from "@/stores/app";
import { selectRunning, useQueue } from "@/stores/queue";
import { useAppInfo, useDrives, useRefreshDrive, useSettings } from "./queries";

/** Label for a drive path, e.g. "NEMESIS (E:\)". */
export function useDriveLabel() {
  const { data: drives } = useDrives();
  const custom = useApp((s) => s.customDrive);
  return useCallback(
    (path: string) => {
      const d = [...(drives ?? []), ...(custom ? [custom] : [])].find((x) => x.mountPoint === path);
      return d?.label ? `${d.label} (${path})` : path;
    },
    [drives, custom],
  );
}

/**
 * Starts an install: plans it, asks for confirmation when it swaps out the
 * package already on the drive, replaces files this app didn't write, or is
 * the first write on macOS, then adds it to the queue.
 */
export function useInstaller() {
  const { data: info } = useAppInfo();
  const { data: settings } = useSettings();
  const driveLabel = useDriveLabel();

  return useCallback(
    async (ids: string[], overrides: Partial<InstallOptions> = {}) => {
      const { drivePath, profileId, extract, setConfirm, setView } = useApp.getState();
      if (!drivePath || !profileId) {
        toast.error("Choose a drive first.");
        return;
      }
      const options: InstallOptions = {
        extract: Object.fromEntries(Object.entries(extract)),
        replaceUntracked: false,
        replaceInstalled: false,
        reinstall: false,
        ...overrides,
      };
      try {
        const plan = await api.planInstall(drivePath, profileId, ids, options);
        if (plan.steps.length === 0 && plan.removes.length === 0) {
          toast.success(plan.upToDate.length === 1 ? "Already up to date." : "Everything is already up to date.");
          return;
        }
        const firstMacWrite = info?.platform === "macos" && !settings?.writePermissionExplained;
        const needsConfirm = firstMacWrite || plan.untracked.length > 0 || plan.removes.length > 0;
        const job = { plan, ids, options, drive: drivePath, driveLabel: driveLabel(drivePath), profile: profileId };
        if (needsConfirm) {
          setConfirm(job);
        } else {
          enqueue(job);
          setView("queue");
        }
      } catch (err) {
        toast.error(errorMessage(err));
      }
    },
    [info, settings, driveLabel],
  );
}

export function enqueue(job: {
  plan: { steps: import("@/bindings/PlanStep").PlanStep[] };
  ids: string[];
  options: InstallOptions;
  drive: string;
  driveLabel: string;
  profile: string;
}) {
  useQueue.getState().enqueue({
    drive: job.drive,
    driveLabel: job.driveLabel,
    profile: job.profile,
    ids: job.ids,
    options: job.options,
    steps: job.plan.steps,
  });
}

/** Runs queued jobs one at a time. Mount once. */
export function useQueueRunner() {
  const jobs = useQueue((s) => s.jobs);
  const refresh = useRefreshDrive();

  useEffect(() => {
    // Read the store, not the render's `jobs`: StrictMode re-runs effects
    // with the same props and must not start a job twice.
    const { jobs: current, setStatus } = useQueue.getState();
    if (current.some((j) => j.status === "running")) return;
    const next = current.find((j) => j.status === "pending");
    if (!next) return;
    setStatus(next.key, "running");
    api
      .install(next.drive, next.profile, next.ids, next.options)
      .then((outcome) => {
        setStatus(next.key, "done");
        const n = outcome.installed.length;
        toast.success(n === 1 ? `Installed ${next.steps[0]?.name ?? "1 package"}.` : `Installed ${n} packages.`);
      })
      .catch((err) => {
        const e = toAppError(err);
        setStatus(next.key, e.kind === "cancelled" ? "cancelled" : "failed", e);
        if (e.kind !== "cancelled") toast.error(e.message);
      })
      .finally(refresh);
  }, [jobs, refresh]);
}

/** Feeds `progress` events into the queue. Mount once. */
export function useProgressEvents() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void api.onProgress((p) => useQueue.getState().onProgress(p)).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}

export function useIsBusy() {
  return useQueue((s) => !!selectRunning(s));
}
