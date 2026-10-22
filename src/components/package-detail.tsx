import { CircleArrowUp, Download, ExternalLink, RotateCcw, Trash2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";
import { useDriveState, useIndex, useRefreshDrive } from "@/hooks/queries";
import { useInstaller, useIsBusy } from "@/hooks/useInstaller";
import { api } from "@/lib/api";
import { errorMessage, isKind } from "@/lib/errors";
import { formatBytes } from "@/lib/utils";
import { useApp } from "@/stores/app";
import { PackageGlyph } from "./package-card";

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[7.5rem_1fr] gap-3 py-1.5 text-[13px]">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

/** Where a package lands under the selected profile, mirroring the core's rules. */
function targetPath(slotPath: string, file: string | undefined, id: string, format: string, version: string, url: string) {
  const fill = (t: string) => t.replaceAll("{id}", id).replaceAll("{format}", format).replaceAll("{version}", version);
  const name = file ? fill(file) : decodeURIComponent(url.split("/").pop() ?? `${id}.${format}`);
  const dir = fill(slotPath);
  return dir ? `${dir}/${name}` : name;
}

export function PackageDetailDialog() {
  const { detailId, openDetail, drivePath, profileId, extract, setExtract } = useApp();
  const { result } = useIndex();
  const { data: drive } = useDriveState();
  const install = useInstaller();
  const refresh = useRefreshDrive();
  const busy = useIsBusy();
  const [removing, setRemoving] = useState(false);

  const pkg = result?.index.packages.find((p) => p.id === detailId);
  const profile = result?.index.profiles.find((p) => p.id === profileId);
  const slot = pkg && profile ? profile.slots[pkg.slot] : undefined;
  const installed = drive?.installed.find((i) => i.record.id === pkg?.id);
  const deps = (pkg?.deps ?? []).map((d) => result?.index.packages.find((p) => p.id === d)?.name ?? d);
  const canExtract = pkg?.format === "zip" && slot?.extract === "optional";
  const extractOn = pkg ? (slot?.extract === "always" ? true : (extract[pkg.id] ?? pkg.extract)) && pkg.format === "zip" : false;
  const ready = !!drivePath && drive?.status === "ready";

  const target =
    pkg && slot
      ? extractOn
        ? `${(slot.extractDir ?? (slot.path ? `${slot.path}/{id}` : "{id}")).replaceAll("{id}", pkg.id)}/`
        : targetPath(slot.path, slot.file, pkg.id, pkg.format, pkg.version, pkg.url)
      : null;

  const remove = async (force = false) => {
    if (!pkg || !drivePath) return;
    setRemoving(true);
    try {
      await api.uninstall(drivePath, pkg.id, force);
      toast.success(`Removed ${pkg.name}.`);
      refresh();
    } catch (err) {
      if (!force && isKind(err, "dependency")) {
        if (window.confirm(`${errorMessage(err)}\n\nRemove it anyway? The packages that need it may stop working.`)) {
          setRemoving(false);
          return remove(true);
        }
      } else {
        toast.error(errorMessage(err));
      }
    } finally {
      setRemoving(false);
    }
  };

  return (
    <Dialog open={!!pkg} onOpenChange={(open) => !open && openDetail(null)}>
      {pkg && (
        <DialogContent className="w-[min(36rem,calc(100vw-2rem))]">
          <DialogHeader className="flex-row items-start gap-3">
            <PackageGlyph pkg={pkg} className="size-12" />
            <div className="min-w-0">
              <DialogTitle className="flex items-center gap-2">
                {pkg.name} {pkg.core && <Badge>Core</Badge>}
              </DialogTitle>
              <DialogDescription className="mt-1">{pkg.description || "No description."}</DialogDescription>
            </div>
          </DialogHeader>

          <dl className="divide-y rounded-lg border bg-card/60 px-3">
            <Row label="Version">
              <span className="tabular-nums">{pkg.version}</span>
              {installed && (
                <span className="ml-2 text-muted-foreground">
                  ({installed.updateAvailable ? `drive has ${installed.record.version}` : "installed"})
                </span>
              )}
            </Row>
            <Row label="Size">{formatBytes(pkg.size)}</Row>
            <Row label="Format">.{pkg.format}{pkg.format === "tar" && " · copied as-is, never unpacked"}</Row>
            {target && (
              <Row label="Goes to">
                <code className="rounded bg-foreground/[0.06] px-1.5 py-0.5 text-[12px]">{target}</code>
              </Row>
            )}
            {deps.length > 0 && <Row label="Needs">{deps.join(", ")}</Row>}
          </dl>

          {canExtract && (
            <label className="flex items-start justify-between gap-4 rounded-lg border bg-card/60 p-3">
              <span>
                <span className="block text-[13px] font-medium">Unpack on the drive</span>
                <span className="block text-[12px] text-muted-foreground">
                  Writes the files into a folder instead of the .zip. Leave off unless the device needs loose files.
                </span>
              </span>
              <Switch checked={extractOn} onCheckedChange={(v) => setExtract(pkg.id, v)} />
            </label>
          )}

          <DialogFooter className="justify-between">
            <div className="mr-auto flex gap-1">
              {pkg.homepage && (
                <Button variant="ghost" size="sm" onClick={() => void api.openUrl(pkg.homepage!)}>
                  <ExternalLink /> Homepage
                </Button>
              )}
            </div>
            {installed && (
              <Button variant="ghost" size="sm" disabled={busy || removing} onClick={() => void remove()}>
                <Trash2 /> Uninstall
              </Button>
            )}
            {installed && !installed.updateAvailable ? (
              <Button
                variant="outline"
                disabled={!ready || busy}
                onClick={() => {
                  openDetail(null);
                  void install([pkg.id], { reinstall: true });
                }}
              >
                <RotateCcw /> Reinstall
              </Button>
            ) : (
              <Button
                disabled={!ready || busy}
                onClick={() => {
                  openDetail(null);
                  void install([pkg.id]);
                }}
              >
                {installed ? <CircleArrowUp /> : <Download />}
                {installed ? `Update to ${pkg.version}` : "Install"}
              </Button>
            )}
          </DialogFooter>
          {!ready && (
            <p className="-mt-2 text-right text-[12px] text-muted-foreground">
              {drivePath ? "Set up the drive first (see Drive check)." : "Insert a drive to install."}
            </p>
          )}
        </DialogContent>
      )}
    </Dialog>
  );
}
