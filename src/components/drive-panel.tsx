import { useQueryClient } from "@tanstack/react-query";
import {
  CircleArrowUp,
  CircleCheck,
  CircleX,
  FolderOpen,
  HardDrive,
  Info,
  LoaderCircle,
  RefreshCw,
  TriangleAlert,
  Usb,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import type { DriveInfo } from "@/bindings/DriveInfo";
import type { FixAction } from "@/bindings/FixAction";
import type { Issue } from "@/bindings/Issue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EjectIcon, EmptyState, Separator, Skeleton } from "@/components/ui/misc";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import { qk, useCheck, useDrives, useDriveState, useIndex, useRefreshDrive, useSettings } from "@/hooks/queries";
import { useInstaller, useIsBusy } from "@/hooks/useInstaller";
import { api } from "@/lib/api";
import { errorMessage } from "@/lib/errors";
import { cn, filesystemLabel, formatBytes, percent } from "@/lib/utils";
import { useApp } from "@/stores/app";

const FIX_LABEL: Record<FixAction, string> = {
  initialize: "Initialize",
  removePartials: "Clean up",
  forgetMissing: "Forget missing",
  migrateProfile: "Migrate",
  resetManifest: "Reset manifest",
};

function driveName(d: DriveInfo) {
  return d.label || d.mountPoint;
}

export function DrivePanel() {
  const { data: drives, isLoading } = useDrives();
  const { result } = useIndex();
  const { data: settings } = useSettings();
  const { drivePath, setDrivePath, customDrive, setCustomDrive, profileId, setProfileId } = useApp();

  const all = useMemo(() => {
    const list = [...(drives ?? [])];
    if (customDrive && !list.some((d) => d.mountPoint === customDrive.mountPoint)) list.push(customDrive);
    return list;
  }, [drives, customDrive]);

  // Keep the selection valid as drives come and go.
  useEffect(() => {
    if (isLoading) return;
    if (drivePath && all.some((d) => d.mountPoint === drivePath)) return;
    setDrivePath(all[0]?.mountPoint ?? null);
  }, [all, drivePath, isLoading, setDrivePath]);

  const profiles = useMemo(() => result?.index.profiles ?? [], [result]);
  useEffect(() => {
    if (profileId && profiles.some((p) => p.id === profileId)) return;
    const preferred = profiles.find((p) => p.id === settings?.lastProfile) ?? profiles[0];
    setProfileId(preferred?.id ?? null);
  }, [profiles, profileId, settings?.lastProfile, setProfileId]);

  const selected = all.find((d) => d.mountPoint === drivePath) ?? null;

  const chooseFolder = async () => {
    try {
      const path = await api.chooseFolder();
      if (!path) return;
      const info = await api.inspectFolder(path);
      setCustomDrive(info);
      setDrivePath(info.mountPoint);
    } catch (err) {
      toast.error(errorMessage(err));
    }
  };

  return (
    <aside className="frosted flex w-[340px] shrink-0 flex-col border-l" aria-label="Drive">
      <div data-tauri-drag-region className="flex h-12 shrink-0 items-center justify-between px-4 pt-1">
        <h2 data-tauri-drag-region className="text-[13px] font-semibold">
          Drive
        </h2>
        <Tooltip content="Use a folder instead of a drive">
          <Button variant="ghost" size="icon-sm" onClick={chooseFolder} aria-label="Choose folder">
            <FolderOpen />
          </Button>
        </Tooltip>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4 scrollbar-thin">
        {isLoading ? (
          <Skeleton className="h-8" />
        ) : all.length === 0 ? (
          <EmptyState icon={<Usb />} title="Insert a USB drive" className="py-8">
            <p>It shows up here a moment after you plug it in.</p>
            <Button variant="outline" size="sm" className="mt-3" onClick={chooseFolder}>
              <FolderOpen /> Choose folder…
            </Button>
          </EmptyState>
        ) : (
          <>
            <div className="flex flex-col gap-1.5">
              <Select
                ariaLabel="Drive"
                value={drivePath}
                onValueChange={setDrivePath}
                options={all.map((d) => ({
                  value: d.mountPoint,
                  label: `${driveName(d)}${d.label ? ` (${d.mountPoint})` : ""}`,
                  hint: `${filesystemLabel(d.filesystem)} · ${formatBytes(d.freeBytes)} free${d.removable ? "" : " · not removable"}`,
                }))}
              />
              {profiles.length > 1 && (
                <Select
                  ariaLabel="Device profile"
                  value={profileId}
                  onValueChange={setProfileId}
                  options={profiles.map((p) => ({ value: p.id, label: p.name ?? p.id, hint: p.description }))}
                />
              )}
            </div>
            {selected && <DriveSummary drive={selected} />}
            <Separator />
            <CheckSection />
            <InstalledSummary />
          </>
        )}
      </div>

      {selected && <EjectBar drive={selected} />}
    </aside>
  );
}

function DriveSummary({ drive }: { drive: DriveInfo }) {
  const used = drive.totalBytes - drive.freeBytes;
  const pct = percent(used, drive.totalBytes);
  return (
    <div className="flex items-start gap-3">
      <div className="grid size-9 shrink-0 place-items-center rounded-lg bg-primary/10 text-primary">
        {drive.removable ? <Usb className="size-4.5" /> : <HardDrive className="size-4.5" />}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">{driveName(drive)}</span>
          <Badge variant="outline">{filesystemLabel(drive.filesystem)}</Badge>
        </div>
        {drive.totalBytes > 0 && (
          <>
            <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-foreground/10" aria-hidden>
              <div
                className={cn("h-full rounded-full", pct > 92 ? "bg-destructive" : "bg-primary/70")}
                style={{ width: `${pct}%` }}
              />
            </div>
            <p className="mt-1 text-[12px] text-muted-foreground">
              {formatBytes(drive.freeBytes)} free of {formatBytes(drive.totalBytes)}
            </p>
          </>
        )}
      </div>
    </div>
  );
}

function IssueIcon({ issue }: { issue: Issue }) {
  if (issue.severity === "error") return <CircleX className="mt-0.5 size-4 shrink-0 text-destructive" />;
  if (issue.severity === "warning") return <TriangleAlert className="mt-0.5 size-4 shrink-0 text-warning" />;
  return <Info className="mt-0.5 size-4 shrink-0 text-muted-foreground" />;
}

function CheckSection() {
  const check = useCheck();
  const { result } = useIndex();
  const refresh = useRefreshDrive();
  const busy = useIsBusy();
  const { drivePath, profileId } = useApp();
  const [fixing, setFixing] = useState<FixAction | null>(null);

  const applyFix = async (fix: FixAction) => {
    if (!drivePath || !profileId) return;
    if (fix === "resetManifest" && !window.confirm("Set the damaged manifest aside and start a new one? Files stay on the drive but are no longer tracked.")) {
      return;
    }
    setFixing(fix);
    try {
      if (fix === "initialize") await api.initDrive(drivePath, profileId);
      else await api.fixDrive(drivePath, profileId, [fix]);
      refresh();
      toast.success(fix === "initialize" ? "Drive set up." : "Fixed.");
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setFixing(null);
    }
  };

  // One button per distinct fix, so Initialize isn't repeated per folder.
  const issues = check.data?.issues ?? [];
  const seenFix = new Set<FixAction>();

  return (
    <section aria-label="Drive check" className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <h3 className="text-[12px] font-semibold tracking-wide text-muted-foreground uppercase">Drive check</h3>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void check.refetch()}
          disabled={check.isFetching || !drivePath}
          aria-label="Check drive again"
        >
          <RefreshCw className={cn(check.isFetching && "animate-spin")} /> Check again
        </Button>
      </div>
      {check.isLoading ? (
        <div className="flex flex-col gap-2">
          <Skeleton className="h-10" />
          <Skeleton className="h-10" />
        </div>
      ) : check.error ? (
        <p className="rounded-lg bg-destructive/10 p-3 text-[13px] text-destructive">{errorMessage(check.error)}</p>
      ) : check.data && issues.length === 0 ? (
        <div className="flex items-center gap-2 rounded-lg bg-success/10 p-3 text-[13px] text-success">
          <CircleCheck className="size-4" /> Ready for{" "}
          {result?.index.profiles.find((p) => p.id === check.data.profile.id)?.name ?? check.data.profile.id}.
        </div>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {issues.map((issue, i) => {
            const showFix = issue.fix && !seenFix.has(issue.fix);
            if (issue.fix) seenFix.add(issue.fix);
            return (
              <li key={`${issue.code}-${i}`} className="flex gap-2 rounded-lg bg-card/70 p-2.5 text-[13px] leading-snug">
                <IssueIcon issue={issue} />
                <div className="min-w-0 flex-1">
                  <p className="selectable">{issue.message}</p>
                  {showFix && issue.fix && (
                    <Button
                      size="sm"
                      variant={issue.fix === "initialize" ? "default" : "outline"}
                      className="mt-2"
                      disabled={busy || fixing !== null}
                      onClick={() => void applyFix(issue.fix!)}
                    >
                      {fixing === issue.fix && <LoaderCircle className="animate-spin" />}
                      {FIX_LABEL[issue.fix]}
                    </Button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function InstalledSummary() {
  const { data: state } = useDriveState();
  const install = useInstaller();
  const setView = useApp((s) => s.setView);
  const busy = useIsBusy();
  const openDetail = useApp((s) => s.openDetail);
  if (!state || state.status !== "ready") return null;
  const updates = state.installed.filter((i) => i.updateAvailable);
  const single = state.installed.length === 1 ? state.installed[0] : undefined;
  return (
    <section aria-label="Installed on this drive" className="flex flex-col gap-2">
      <Separator />
      <div className="flex items-center justify-between">
        <h3 className="text-[12px] font-semibold tracking-wide text-muted-foreground uppercase">On this drive</h3>
        {state.installed.length > 1 && (
          <Button variant="link" size="sm" onClick={() => setView("installed")}>
            View all
          </Button>
        )}
      </div>
      {state.installed.length === 0 ? (
        <p className="text-[13px] text-muted-foreground">Empty. Pick a package to put on it.</p>
      ) : single ? (
        <button
          type="button"
          onClick={() => single.inIndex && openDetail(single.record.id)}
          className="flex items-center justify-between gap-2 rounded-lg bg-card/70 p-2.5 text-left text-[13px] outline-none hover:bg-card focus-visible:ring-[3px] focus-visible:ring-ring"
        >
          <span className="min-w-0">
            <span className="block truncate font-medium">{single.name}</span>
            <span className="block truncate font-mono text-[11px] text-muted-foreground">
              {single.record.paths.slice(0, 2).join(" · ")}
            </span>
          </span>
          <span className="shrink-0 text-[12px] text-muted-foreground tabular-nums">{single.record.version}</span>
        </button>
      ) : (
        <p className="text-[13px]">{state.installed.length} packages installed.</p>
      )}
      {updates.length > 0 && (
        <div className="flex items-center justify-between gap-2 rounded-lg bg-primary/8 p-2.5">
          <span className="flex items-center gap-2 text-[13px] text-primary">
            <CircleArrowUp className="size-4" />
            {updates.length} update{updates.length === 1 ? "" : "s"} available
          </span>
          <Button size="sm" disabled={busy} onClick={() => void install(updates.map((u) => u.record.id))}>
            {updates.length === 1 ? "Update" : "Update all"}
          </Button>
        </div>
      )}
    </section>
  );
}

function EjectBar({ drive }: { drive: DriveInfo }) {
  const busy = useIsBusy();
  const [ejecting, setEjecting] = useState(false);
  const qc = useQueryClient();
  const { setDrivePath, customDrive, setCustomDrive } = useApp();
  const isFolder = customDrive?.mountPoint === drive.mountPoint && !drive.removable;

  const eject = async () => {
    setEjecting(true);
    try {
      await api.eject(drive.mountPoint);
      toast.success(`${driveName(drive)} can be removed safely.`);
      if (customDrive?.mountPoint === drive.mountPoint) setCustomDrive(null);
      setDrivePath(null);
      void qc.invalidateQueries({ queryKey: qk.drives });
    } catch (err) {
      toast.error(errorMessage(err), { duration: 10_000 });
    } finally {
      setEjecting(false);
    }
  };

  if (isFolder) {
    return (
      <div className="border-t p-3">
        <Button variant="ghost" className="w-full" onClick={() => setCustomDrive(null)}>
          Stop using this folder
        </Button>
      </div>
    );
  }
  return (
    <div className="border-t p-3">
      <Tooltip content={busy ? "Wait for the install to finish" : "Flush and eject so the drive can be unplugged"}>
        <span className="block">
          <Button variant="outline" className="w-full" disabled={busy || ejecting} onClick={() => void eject()}>
            {ejecting ? <LoaderCircle className="animate-spin" /> : <EjectIcon />} Eject {driveName(drive)}
          </Button>
        </span>
      </Tooltip>
    </div>
  );
}
