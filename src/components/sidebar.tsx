import { HardDrive, Layers, ListChecks, LoaderCircle, Package, RefreshCw, Settings, WifiOff } from "lucide-react";
import type * as React from "react";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { useAppInfo, useDriveState, useIndex } from "@/hooks/queries";
import { errorMessage } from "@/lib/errors";
import { cn, timeAgo } from "@/lib/utils";
import { type View, useApp } from "@/stores/app";
import { selectActiveCount, selectRunning, useQueue } from "@/stores/queue";

function NavItem({
  view,
  icon,
  label,
  trailing,
}: {
  view: View;
  icon: React.ReactNode;
  label: string;
  trailing?: React.ReactNode;
}) {
  const active = useApp((s) => s.view === view);
  const setView = useApp((s) => s.setView);
  return (
    <button
      type="button"
      onClick={() => setView(view)}
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex h-8 w-full items-center gap-2.5 rounded-md px-2.5 text-[13px] font-medium outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring [&_svg]:size-4",
        active ? "bg-foreground/[0.08] text-foreground" : "text-foreground/75 hover:bg-foreground/[0.05]",
      )}
    >
      <span className={cn(active ? "text-primary" : "text-muted-foreground")}>{icon}</span>
      <span className="flex-1 text-left">{label}</span>
      {trailing}
    </button>
  );
}

function Count({ n, tone = "muted" }: { n: number; tone?: "muted" | "primary" }) {
  if (n <= 0) return null;
  return (
    <span
      className={cn(
        "min-w-5 rounded-full px-1.5 text-center text-[11px] leading-5 tabular-nums",
        tone === "primary" ? "bg-primary text-primary-foreground" : "bg-foreground/10 text-muted-foreground",
      )}
    >
      {n}
    </span>
  );
}

export function Sidebar() {
  const { data: info } = useAppInfo();
  const { result, isFetching, error, refetch } = useIndex();
  const { data: drive } = useDriveState();
  const active = useQueue(selectActiveCount);
  const running = useQueue((s) => !!selectRunning(s));

  const offline = result?.offline || (!!error && !!result);
  return (
    <nav
      aria-label="Main"
      className="flex w-56 shrink-0 flex-col bg-sidebar in-data-[platform=macos]:pt-7"
    >
      <div data-tauri-drag-region className="flex h-14 shrink-0 items-center gap-2.5 px-4">
        <div className="grid size-8 place-items-center rounded-lg bg-gradient-to-br from-primary to-[oklch(0.45_0.2_300)] text-primary-foreground shadow-sm">
          <Layers className="size-4" />
        </div>
        <div data-tauri-drag-region className="leading-tight">
          <div className="text-[13px] font-semibold">nempm</div>
          <div className="text-[11px] text-muted-foreground">Nemesis Package Manager</div>
        </div>
      </div>

      <div className="flex flex-col gap-0.5 px-2 pt-2">
        <NavItem view="browse" icon={<Package />} label="Browse" />
        <NavItem
          view="installed"
          icon={<HardDrive />}
          label="Installed"
          trailing={<Count n={drive?.updatesAvailable ?? 0} tone="primary" />}
        />
        <NavItem
          view="queue"
          icon={<ListChecks />}
          label="Install queue"
          trailing={running ? <LoaderCircle className="size-3.5 animate-spin text-primary" /> : <Count n={active} />}
        />
        <NavItem view="settings" icon={<Settings />} label="Settings" />
      </div>

      <div className="mt-auto flex flex-col gap-1 p-3">
        <div className="flex items-center gap-2 rounded-lg px-1.5 py-1 text-[12px] text-muted-foreground">
          {offline ? (
            <Tooltip content={result?.error ?? (error ? errorMessage(error) : "Couldn't reach the registry")}>
              <span className="flex min-w-0 flex-1 items-center gap-1.5 text-warning">
                <WifiOff className="size-3.5 shrink-0" />
                <span className="truncate">Offline · saved list</span>
              </span>
            </Tooltip>
          ) : error && !result ? (
            <Tooltip content={errorMessage(error)}>
              <span className="flex-1 truncate text-destructive">Package list unavailable</span>
            </Tooltip>
          ) : (
            <span className="flex-1 truncate">
              {result ? `Updated ${timeAgo(result.fetchedAt)}` : "Loading packages…"}
            </span>
          )}
          <Tooltip content="Refresh the package list">
            <Button variant="ghost" size="icon-sm" onClick={() => void refetch()} aria-label="Refresh package list">
              <RefreshCw className={cn("size-3.5", isFetching && "animate-spin")} />
            </Button>
          </Tooltip>
        </div>
        {info && <div className="px-1.5 text-[11px] text-muted-foreground/70">Version {info.version}</div>}
      </div>
    </nav>
  );
}
