import { Check, CircleArrowUp, Download, FileArchive, Layers } from "lucide-react";
import type { InstalledView } from "@/bindings/InstalledView";
import type { Package } from "@/bindings/Package";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn, formatBytes } from "@/lib/utils";

export function PackageGlyph({ pkg, className }: { pkg: Package; className?: string }) {
  return (
    <div
      className={cn(
        "grid size-10 shrink-0 place-items-center rounded-xl",
        pkg.core
          ? "bg-gradient-to-br from-primary to-[oklch(0.45_0.2_300)] text-primary-foreground"
          : "bg-primary/10 text-primary",
        className,
      )}
    >
      {pkg.core ? <Layers className="size-5" /> : <FileArchive className="size-5" />}
    </div>
  );
}

export function PackageCard({
  pkg,
  installed,
  canInstall,
  onOpen,
  onInstall,
}: {
  pkg: Package;
  installed?: InstalledView;
  canInstall: boolean;
  onOpen: () => void;
  onInstall: () => void;
}) {
  const update = installed?.updateAvailable;
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="group flex flex-col gap-3 rounded-xl border bg-card p-4 text-left shadow-[0_1px_2px_oklch(0_0_0/0.04)] outline-none transition-[box-shadow,border-color] hover:border-primary/30 hover:shadow-md focus-visible:ring-[3px] focus-visible:ring-ring"
    >
      <div className="flex items-start gap-3">
        <PackageGlyph pkg={pkg} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-sm font-semibold">{pkg.name}</h3>
            {pkg.core && <Badge>Core</Badge>}
          </div>
          <p className="mt-0.5 text-[12px] text-muted-foreground tabular-nums">
            v{pkg.version} · {formatBytes(pkg.size)}
          </p>
        </div>
      </div>
      <p className="line-clamp-2 min-h-[2lh] text-[13px] leading-snug text-muted-foreground">
        {pkg.description || "No description."}
      </p>
      <div className="mt-auto flex items-center justify-between gap-2">
        {installed ? (
          update ? (
            <Badge variant="default">
              <CircleArrowUp /> {installed.record.version} → {pkg.version}
            </Badge>
          ) : (
            <Badge variant="success">
              <Check /> Installed
            </Badge>
          )
        ) : (
          <span className="text-[12px] text-muted-foreground">.{pkg.format}</span>
        )}
        {(!installed || update) && (
          <Button
            size="sm"
            variant={update ? "default" : "secondary"}
            disabled={!canInstall}
            onClick={(e) => {
              e.stopPropagation();
              onInstall();
            }}
          >
            {update ? <CircleArrowUp /> : <Download />}
            {update ? "Update" : "Install"}
          </Button>
        )}
      </div>
    </div>
  );
}
