import { create } from "zustand";
import type { AppError } from "@/bindings/AppError";
import type { InstallOptions } from "@/bindings/InstallOptions";
import type { PlanStep } from "@/bindings/PlanStep";
import type { Progress } from "@/bindings/Progress";
import type { Stage } from "@/bindings/Stage";

export type JobStatus = "pending" | "running" | "done" | "failed" | "cancelled";

/** Latest progress for one package, plus the stage it was in when it failed. */
export interface StepProgress extends Progress {
  failedAt?: Stage;
  /** Any of the package's files had to be downloaded. */
  sawDownload?: boolean;
  /** Download was skipped because verified copies were cached. */
  cached?: boolean;
}

export interface Job {
  key: string;
  drive: string;
  driveLabel: string;
  profile: string;
  ids: string[];
  options: InstallOptions;
  steps: PlanStep[];
  status: JobStatus;
  error?: AppError;
  createdAt: number;
  finishedAt?: number;
  progress: Record<string, StepProgress>;
}

interface QueueStore {
  jobs: Job[];
  enqueue: (job: Omit<Job, "key" | "status" | "progress" | "createdAt">) => void;
  onProgress: (p: Progress) => void;
  setStatus: (key: string, status: JobStatus, error?: AppError) => void;
  remove: (key: string) => void;
  clearFinished: () => void;
}

let counter = 0;

export const useQueue = create<QueueStore>((set) => ({
  jobs: [],
  enqueue: (job) =>
    set((s) => ({
      jobs: [
        ...s.jobs,
        { ...job, key: `job-${Date.now()}-${counter++}`, status: "pending", progress: {}, createdAt: Date.now() },
      ],
    })),
  onProgress: (p) =>
    set((s) => ({
      jobs: s.jobs.map((job) => {
        if (job.status !== "running" || !job.steps.some((step) => step.id === p.id)) return job;
        const prev = job.progress[p.id];
        const next: StepProgress = { ...p };
        if (p.stage === "error") next.failedAt = prev?.stage === "error" ? prev.failedAt : prev?.stage;
        // Past the download stage without ever downloading: the cache had it.
        next.sawDownload = prev?.sawDownload || p.stage === "download";
        next.cached = p.stage === "error" ? !!prev?.cached : !next.sawDownload && p.stage !== "queued";
        return { ...job, progress: { ...job.progress, [p.id]: next } };
      }),
    })),
  setStatus: (key, status, error) =>
    set((s) => ({
      jobs: s.jobs.map((j) =>
        j.key === key
          ? { ...j, status, error, finishedAt: status === "running" || status === "pending" ? undefined : Date.now() }
          : j,
      ),
    })),
  remove: (key) => set((s) => ({ jobs: s.jobs.filter((j) => j.key !== key) })),
  clearFinished: () => set((s) => ({ jobs: s.jobs.filter((j) => j.status === "pending" || j.status === "running") })),
}));

export const selectRunning = (s: QueueStore) => s.jobs.find((j) => j.status === "running");
export const selectActiveCount = (s: QueueStore) =>
  s.jobs.filter((j) => j.status === "running" || j.status === "pending").length;
