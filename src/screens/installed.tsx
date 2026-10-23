import { CircleArrowUp, CircleX, HardDrive, Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import type { InstalledView } from "@/bindings/InstalledView";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState, Skeleton } from "@/components/ui/misc";
import { useDriveState, useRefreshDrive } from "@/hooks/queries";
import { useInstaller, useIsBusy } from "@/hooks/useInstaller";
import { api } from "@/lib/api";
import { errorMessage } from "@/lib/errors";
import { formatBytes } from "@/lib/utils";
import { useApp } from "@/stores/app";
import { ScreenHeader } from "./screen-header";

export function InstalledScreen() {
  const { data: drive, isLoading, error } = useDriveState();
  const drivePath = useApp((s) => s.drivePath);
  const install = useInstaller();
  const busy = useIsBusy();
  const updates = drive?.installed.filter((i) => i.updateAvailable) ?? [];

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ScreenHeader
        title="Installed"
        subtitle={drive?.status === "ready" ? `${drive.installed.length} on this drive` : undefined}
        actions={
          updates.length > 0 && (
            <Button disabled={busy} onClick={() => void install(updates.map((u) => u.record.id))}>
              <CircleArrowUp /> Update all ({updates.length})
            </Button>
          )
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6 scrollbar-thin">
        {!drivePath ? (
          <EmptyState icon={<HardDrive />} title="No drive selected">
            Insert a USB drive or choose a folder in the Drive panel.
          </EmptyState>
        ) : isLoading ? (
          <div className="flex flex-col gap-2">
            <Skeleton className="h-14" />
            <Skeleton className="h-14" />
          </div>
        ) : error ? (
          <EmptyState icon={<CircleX />} title="Couldn't read the drive">
            <p className="selectable">{errorMessage(error)}</p>
          </EmptyState>
        ) : drive?.status === "corrupt" ? (
          <EmptyState icon={<CircleX />} title="The drive's manifest is damaged">
            <p className="selectable">{drive.error}</p>
            <p className="mt-1">Run Check drive to repair it.</p>
          </EmptyState>
        ) : drive?.status === "uninitialized" ? (
          <EmptyState icon={<HardDrive />} title="This drive isn't set up yet">
            Use Initialize in the Drive check.
          </EmptyState>
        ) : !drive || drive.installed.length === 0 ? (
          <EmptyState icon={<HardDrive />} title="Nothing installed yet">
            Packages you install appear here.
          </EmptyState>
        ) : (
          <div className="overflow-hidden rounded-xl border bg-card">
            <table className="w-full text-[13px]">
              <thead className="border-b bg-foreground/[0.02] text-left text-[12px] text-muted-foreground">
                <tr>
                  <th className="px-4 py-2 font-medium">Package</th>
                  <th className="px-4 py-2 font-medium">Version</th>
                  <th className="px-4 py-2 font-medium">Size</th>
                  <th className="px-4 py-2 font-medium">Status</th>
                  <th className="px-4 py-2" />
                </tr>
              </thead>
              <tbody className="divide-y">
                {drive.installed.map((item) => (
                  <InstalledRow key={item.record.id} item={item} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

function InstalledRow({ item }: { item: InstalledView }) {
  const { drivePath, openDetail } = useApp();
  const install = useInstaller();
  const refresh = useRefreshDrive();
  const busy = useIsBusy();
  const [removing, setRemoving] = useState(false);
  const rec = item.record;

  const remove = async () => {
    if (!drivePath) return;
    const files = rec.data ? `${rec.paths.length} file(s), including ${rec.data},` : `${rec.paths.length} file(s)`;
    if (!window.confirm(`Uninstall ${item.name}? Its ${files} will be deleted from the drive.`)) return;
    setRemoving(true);
    try {
      await api.uninstall(drivePath, rec.id);
      toast.success(`Removed ${item.name}.`);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setRemoving(false);
      refresh();
    }
  };

  return (
    <tr className="hover:bg-foreground/[0.02]">
      <td className="px-4 py-2.5">
        <button
          type="button"
          className="text-left font-medium outline-none hover:underline focus-visible:underline disabled:no-underline"
          onClick={() => openDetail(rec.id)}
          disabled={!item.inIndex}
        >
          {item.name}
        </button>
        <div className="font-mono text-[11px] text-muted-foreground">
          {rec.extracted
            ? `${rec.paths.length - (rec.data ? 1 : 0)} unpacked file(s)`
            : rec.paths.find((p) => p !== rec.data)}
          {rec.data && ` + ${rec.data}`}
        </div>
      </td>
      <td className="px-4 py-2.5 tabular-nums">
        {rec.version}
        {item.updateAvailable && <span className="text-primary"> → {item.latest}</span>}
      </td>
      <td className="px-4 py-2.5 text-muted-foreground tabular-nums">{formatBytes(rec.size)}</td>
      <td className="px-4 py-2.5">
        {!item.filesPresent ? (
          <Badge variant="destructive">Files missing</Badge>
        ) : item.updateAvailable ? (
          <Badge>Update available</Badge>
        ) : !item.inIndex ? (
          <Badge variant="outline">Not in list</Badge>
        ) : (
          <Badge variant="success">Up to date</Badge>
        )}
      </td>
      <td className="px-4 py-2.5">
        <div className="flex justify-end gap-1">
          {(item.updateAvailable || !item.filesPresent) && item.inIndex && (
            <Button size="sm" disabled={busy} onClick={() => void install([rec.id], { reinstall: !item.filesPresent })}>
              {item.updateAvailable ? "Update" : "Repair"}
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Uninstall ${item.name}`}
            disabled={busy || removing}
            onClick={() => void remove()}
          >
            <Trash2 />
          </Button>
        </div>
      </td>
    </tr>
  );
}
