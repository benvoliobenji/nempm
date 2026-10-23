import { Check, CircleX, ListChecks, LoaderCircle, RotateCcw, Square, X } from "lucide-react";
import { toast } from "sonner";
import type { PlanStep } from "@/bindings/PlanStep";
import type { Stage } from "@/bindings/Stage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/misc";
import { Progress } from "@/components/ui/progress";
import { api } from "@/lib/api";
import { errorMessage } from "@/lib/errors";
import { cn, formatBytes, percent } from "@/lib/utils";
import { type Job, type StepProgress, useQueue } from "@/stores/queue";
import { ScreenHeader } from "./screen-header";

const LABEL: Record<Stage, string> = {
  queued: "Waiting",
  download: "Download",
  verify: "Verify",
  copy: "Copy",
  readback: "Read-back",
  extract: "Unpack",
  done: "Done",
  error: "Failed",
};

/** The stages one package goes through, in order. Its data file is copied
 * before a zip is unpacked, so an unpacked package has both stages, unless
 * the zip carries the data file itself. */
function stagesFor(step: PlanStep): Stage[] {
  if (!step.extract) return ["download", "verify", "copy", "readback"];
  return step.dataTarget && !step.dataInZip
    ? ["download", "verify", "copy", "extract", "readback"]
    : ["download", "verify", "extract", "readback"];
}

function StageStepper({ step, progress }: { step: PlanStep; progress?: StepProgress }) {
  const stages = stagesFor(step);
  const current = progress?.stage ?? "queued";
  const failedAt = progress?.failedAt;
  const position = (s: Stage | undefined) => (s ? stages.indexOf(s) : -1);
  const at = current === "done" ? stages.length : current === "error" ? position(failedAt) : position(current);

  return (
    <ol className="flex items-center gap-1" aria-label={`${step.name}: ${LABEL[current]}`}>
      {stages.map((s, i) => {
        const skipped = s === "download" && progress?.cached;
        const done = i < at || current === "done";
        const active = i === at && current !== "error" && current !== "done";
        const failed = current === "error" && i === at;
        return (
          <li key={s} className="flex items-center gap-1">
            {i > 0 && <span className={cn("h-px w-3", done || active ? "bg-primary/50" : "bg-foreground/15")} />}
            <span
              className={cn(
                "inline-flex h-6 items-center gap-1 rounded-full px-2 text-[11px] font-medium",
                done && "bg-primary/12 text-primary",
                active && "bg-primary text-primary-foreground",
                failed && "bg-destructive/12 text-destructive",
                !done && !active && !failed && "bg-foreground/[0.06] text-muted-foreground",
              )}
            >
              {active && <LoaderCircle className="size-3 animate-spin" />}
              {done && <Check className="size-3" />}
              {failed && <CircleX className="size-3" />}
              {skipped ? "Cached" : LABEL[s]}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

function StepRow({ step, progress, jobStatus }: { step: PlanStep; progress?: StepProgress; jobStatus: Job["status"] }) {
  const stage = progress?.stage ?? "queued";
  const moving = stage !== "queued" && stage !== "done" && stage !== "error";
  return (
    <li className="flex flex-col gap-2 px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <span className="font-medium">{step.name}</span>{" "}
          <span className="text-[12px] text-muted-foreground tabular-nums">
            {step.fromVersion ? `${step.fromVersion} → ` : ""}
            {step.version} · {formatBytes(step.size)}
          </span>
        </div>
        {stage === "queued" && jobStatus === "running" && <span className="text-[12px] text-muted-foreground">Waiting</span>}
      </div>
      <StageStepper step={step} progress={progress} />
      {moving && progress && (
        <div className="flex items-center gap-3">
          <Progress value={percent(progress.bytesDone, progress.bytesTotal)} label={`${step.name} ${LABEL[stage]}`} />
          <span className="w-36 shrink-0 text-right text-[11px] text-muted-foreground tabular-nums">
            {formatBytes(progress.bytesDone)} / {formatBytes(progress.bytesTotal)}
          </span>
        </div>
      )}
      {stage === "error" && progress?.message && (
        <p className="selectable rounded-md bg-destructive/8 px-2.5 py-1.5 text-[12px] text-destructive">{progress.message}</p>
      )}
    </li>
  );
}

function JobCard({ job }: { job: Job }) {
  const { remove, enqueue } = useQueue();
  const title =
    job.steps.length === 1 ? `${job.steps[0]!.name}` : `${job.steps.length} packages`;

  const cancel = () => api.cancelInstall().catch((err) => toast.error(errorMessage(err)));
  const retry = (replaceUntracked = job.options.replaceUntracked) => {
    remove(job.key);
    enqueue({
      drive: job.drive,
      driveLabel: job.driveLabel,
      profile: job.profile,
      ids: job.ids,
      options: { ...job.options, replaceUntracked },
      steps: job.steps,
    });
  };
  // Files in the way of an unpacked zip only show up once it's downloaded.
  const conflict = job.status === "failed" && job.error?.kind === "conflict" && !job.options.replaceUntracked;

  return (
    <section className="overflow-hidden rounded-xl border bg-card" aria-label={`Install ${title}`}>
      <header className="flex items-center gap-3 border-b bg-foreground/[0.02] px-4 py-2.5">
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-semibold">{title}</p>
          <p className="truncate text-[12px] text-muted-foreground">To {job.driveLabel}</p>
        </div>
        <StatusBadge status={job.status} />
        {job.status === "running" && (
          <Button variant="outline" size="sm" onClick={() => void cancel()}>
            <Square className="size-3" /> Cancel
          </Button>
        )}
        {conflict ? (
          <Button variant="outline" size="sm" onClick={() => retry(true)}>
            <RotateCcw /> Replace files and retry
          </Button>
        ) : (
          (job.status === "failed" || job.status === "cancelled") && (
            <Button variant="outline" size="sm" onClick={() => retry()}>
              <RotateCcw /> Retry
            </Button>
          )
        )}
        {job.status !== "running" && (
          <Button variant="ghost" size="icon-sm" aria-label="Remove from list" onClick={() => remove(job.key)}>
            <X />
          </Button>
        )}
      </header>
      <ul className="divide-y text-[13px]">
        {job.steps.map((s) => (
          <StepRow key={s.id} step={s} progress={job.progress[s.id]} jobStatus={job.status} />
        ))}
      </ul>
      {job.status === "failed" && job.error && !Object.values(job.progress).some((p) => p.stage === "error") && (
        <p className="selectable border-t bg-destructive/8 px-4 py-2 text-[12px] text-destructive">{job.error.message}</p>
      )}
    </section>
  );
}

function StatusBadge({ status }: { status: Job["status"] }) {
  switch (status) {
    case "pending":
      return <Badge variant="secondary">Queued</Badge>;
    case "running":
      return <Badge>Installing</Badge>;
    case "done":
      return <Badge variant="success">Done</Badge>;
    case "failed":
      return <Badge variant="destructive">Failed</Badge>;
    case "cancelled":
      return <Badge variant="outline">Cancelled</Badge>;
  }
}

export function QueueScreen() {
  const jobs = useQueue((s) => s.jobs);
  const clearFinished = useQueue((s) => s.clearFinished);
  const finished = jobs.some((j) => j.status !== "pending" && j.status !== "running");
  const ordered = [...jobs].reverse();

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ScreenHeader
        title="Install queue"
        subtitle="Download → verify → copy → read-back, one package at a time"
        actions={
          finished && (
            <Button variant="ghost" size="sm" onClick={clearFinished}>
              Clear finished
            </Button>
          )
        }
      />
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-6 pb-6 scrollbar-thin">
        {ordered.length === 0 ? (
          <EmptyState icon={<ListChecks />} title="Nothing in the queue">
            Installs you start show their progress here.
          </EmptyState>
        ) : (
          ordered.map((job) => <JobCard key={job.key} job={job} />)
        )}
      </div>
    </div>
  );
}
