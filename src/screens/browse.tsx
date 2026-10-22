import { Layers, PackageOpen, Search, TriangleAlert, Usb } from "lucide-react";
import { type ReactNode, useMemo } from "react";
import { PackageCard } from "@/components/package-card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { EmptyState, Skeleton } from "@/components/ui/misc";
import { useDriveState, useIndex } from "@/hooks/queries";
import { useInstaller, useIsBusy } from "@/hooks/useInstaller";
import { errorMessage } from "@/lib/errors";
import { formatBytes } from "@/lib/utils";
import { useApp } from "@/stores/app";
import { ScreenHeader } from "./screen-header";

export function BrowseScreen() {
  const { result, isLoading, error, refetch } = useIndex();
  const { data: drive } = useDriveState();
  const { search, setSearch, openDetail, profileId, drivePath } = useApp();
  const install = useInstaller();
  const busy = useIsBusy();

  const packages = useMemo(() => {
    const all = (result?.index.packages ?? []).filter((p) => !profileId || p.profiles.includes(profileId));
    const q = search.trim().toLowerCase();
    const filtered = q
      ? all.filter((p) => `${p.name} ${p.id} ${p.description}`.toLowerCase().includes(q))
      : all;
    return [...filtered].sort((a, b) => Number(b.core) - Number(a.core) || a.name.localeCompare(b.name));
  }, [result, search, profileId]);

  const installedById = new Map(drive?.installed.map((i) => [i.record.id, i]) ?? []);
  const core = result?.index.packages.find((p) => p.core && (!profileId || p.profiles.includes(profileId)));
  const ready = drive?.status === "ready";
  const coreMissing = ready && core && !installedById.has(core.id);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ScreenHeader
        title="Packages"
        subtitle={result ? `${packages.length} available` : undefined}
        actions={
          <div className="relative w-64">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Search packages"
              aria-label="Search packages"
              className="pl-8"
            />
          </div>
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6 scrollbar-thin">
        {result?.warnings.length ? (
          <div className="mb-4 flex gap-2 rounded-lg border border-warning/40 bg-warning/10 p-3 text-[13px]">
            <TriangleAlert className="mt-0.5 size-4 shrink-0 text-warning" />
            <div>
              <p className="font-medium">Some entries in the package list were skipped.</p>
              <ul className="mt-1 list-disc pl-4 text-muted-foreground">
                {result.warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </div>
          </div>
        ) : null}

        {!drivePath && result && (
          <Banner icon={<Usb />} title="No drive selected">
            You can browse packages now. Insert a USB drive to install them.
          </Banner>
        )}
        {drivePath && drive?.status === "uninitialized" && (
          <Banner icon={<TriangleAlert />} title="This drive isn't set up yet">
            Use <b>Initialize</b> in the Drive check on the right to create the folders the device expects.
          </Banner>
        )}
        {coreMissing && core && (
          <div className="mb-5 flex items-center gap-4 rounded-xl border border-primary/25 bg-gradient-to-r from-primary/12 to-primary/4 p-4">
            <div className="grid size-10 shrink-0 place-items-center rounded-xl bg-primary text-primary-foreground">
              <Layers className="size-5" />
            </div>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-semibold">{core.name} isn't on this drive yet</p>
              <p className="text-[13px] text-muted-foreground">
                Every other package needs it. {formatBytes(core.size)}.
              </p>
            </div>
            <Button disabled={busy} onClick={() => void install([core.id])}>
              Install {core.name}
            </Button>
          </div>
        )}

        {isLoading ? (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-3">
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} className="h-40 rounded-xl" />
            ))}
          </div>
        ) : !result ? (
          <EmptyState icon={<TriangleAlert />} title="Couldn't load the package list">
            <p className="selectable">{error ? errorMessage(error) : "Check your connection."}</p>
            <Button variant="outline" size="sm" className="mt-3" onClick={() => void refetch()}>
              Try again
            </Button>
          </EmptyState>
        ) : packages.length === 0 ? (
          <EmptyState icon={<PackageOpen />} title={search ? "No matches" : "No packages yet"}>
            {search ? `Nothing matches "${search}".` : "The registry doesn't list any packages for this device."}
          </EmptyState>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-3">
            {packages.map((p) => (
              <PackageCard
                key={p.id}
                pkg={p}
                installed={installedById.get(p.id)}
                canInstall={ready && !busy}
                onOpen={() => openDetail(p.id)}
                onInstall={() => void install([p.id])}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function Banner({ icon, title, children }: { icon: ReactNode; title: string; children: ReactNode }) {
  return (
    <div className="mb-5 flex gap-3 rounded-xl border bg-card/70 p-4 text-[13px] [&>svg]:mt-0.5 [&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:text-muted-foreground">
      {icon}
      <div>
        <p className="font-medium">{title}</p>
        <p className="text-muted-foreground">{children}</p>
      </div>
    </div>
  );
}
