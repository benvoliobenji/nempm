import { useQueryClient } from "@tanstack/react-query";
import { FolderOpen, LoaderCircle, Monitor, Moon, RotateCcw, Sun } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { toast } from "sonner";
import type { Settings } from "@/bindings/Settings";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/misc";
import { Switch } from "@/components/ui/switch";
import { qk, useAppInfo, useCacheInfo, useIndex, useSaveSettings, useSettings } from "@/hooks/queries";
import { checkForUpdatesNow } from "@/hooks/useEnvironment";
import { useIsBusy } from "@/hooks/useInstaller";
import { api } from "@/lib/api";
import { errorMessage } from "@/lib/errors";
import { formatBytes, timeAgo } from "@/lib/utils";
import { ScreenHeader } from "./screen-header";

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <h2 className="px-1 text-[12px] font-semibold tracking-wide text-muted-foreground uppercase">{title}</h2>
      <Card className="divide-y">{children}</Card>
    </section>
  );
}

function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-6 px-4 py-3">
      <div className="min-w-0">
        <div className="text-[13px] font-medium">{label}</div>
        {hint && <div className="text-[12px] text-muted-foreground">{hint}</div>}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

const MATERIAL: Record<string, string> = {
  glass: "Liquid Glass / vibrancy",
  mica: "Mica",
  solid: "Solid",
};

export function SettingsScreen() {
  const { data: settings } = useSettings();
  const { data: info } = useAppInfo();
  const { data: cache } = useCacheInfo();
  const { result } = useIndex();
  const save = useSaveSettings();
  const qc = useQueryClient();
  const busy = useIsBusy();
  const [indexUrl, setIndexUrl] = useState("");
  const [checking, setChecking] = useState(false);
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    if (settings) setIndexUrl(settings.indexUrl);
  }, [settings]);

  if (!settings) return null;

  const update = async (patch: Partial<Settings>, then?: () => void) => {
    try {
      await save.mutateAsync({ ...settings, ...patch });
      then?.();
    } catch (err) {
      toast.error(errorMessage(err));
    }
  };

  const saveIndexUrl = (url: string) =>
    update({ indexUrl: url.trim() }, () => {
      toast.success("Index URL saved.");
      // Drop the old registry's list, then load the new one.
      void qc.resetQueries({ queryKey: qk.indexCache });
      void qc.resetQueries({ queryKey: qk.index });
    });

  const chooseCache = async () => {
    const dir = await api.chooseFolder();
    if (dir) await update({ cacheDir: dir }, () => void qc.invalidateQueries({ queryKey: qk.cache }));
  };

  const clearCache = async () => {
    setClearing(true);
    try {
      const freed = await api.clearCache();
      toast.success(`Freed ${formatBytes(freed)}.`);
      void qc.invalidateQueries({ queryKey: qk.cache });
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setClearing(false);
    }
  };

  const checkUpdates = async () => {
    setChecking(true);
    try {
      toast.info(await checkForUpdatesNow());
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ScreenHeader title="Settings" />
      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto px-6 pb-8 scrollbar-thin">
        <Section title="Appearance">
          <Field label="Theme" hint="System follows your OS light or dark setting.">
            <Segmented
              ariaLabel="Theme"
              value={settings.theme}
              onChange={(theme) => void update({ theme })}
              options={[
                { value: "system", label: <><Monitor /> System</> },
                { value: "light", label: <><Sun /> Light</> },
                { value: "dark", label: <><Moon /> Dark</> },
              ]}
            />
          </Field>
          <Field label="Window material" hint="Chosen automatically for your OS.">
            <span className="text-[13px] text-muted-foreground">{MATERIAL[info?.material ?? "solid"]}</span>
          </Field>
        </Section>

        <Section title="Package list">
          <div className="flex flex-col gap-2 px-4 py-3">
            <div className="text-[13px] font-medium">Index URL</div>
            <div className="flex gap-2">
              <Input
                value={indexUrl}
                onChange={(e) => setIndexUrl(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && void saveIndexUrl(indexUrl)}
                aria-label="Index URL"
                className="font-mono text-[12px]"
              />
              <Button variant="outline" disabled={indexUrl.trim() === settings.indexUrl} onClick={() => void saveIndexUrl(indexUrl)}>
                Save
              </Button>
              {info && settings.indexUrl !== info.defaultIndexUrl && (
                <Button variant="ghost" size="icon" aria-label="Reset to default" onClick={() => void saveIndexUrl(info.defaultIndexUrl)}>
                  <RotateCcw />
                </Button>
              )}
            </div>
            <p className="text-[12px] text-muted-foreground">
              {result
                ? `${result.index.packages.length} packages · built ${timeAgo(result.index.generatedAt)} · fetched ${timeAgo(result.fetchedAt)}${result.offline ? " · offline" : ""}`
                : "Not loaded yet."}
            </p>
          </div>
        </Section>

        <Section title="Downloads">
          <Field
            label="Cache folder"
            hint={<span className="selectable font-mono text-[11px] break-all">{cache?.path ?? "…"}</span>}
          >
            <Button variant="outline" size="sm" onClick={() => void chooseCache()} disabled={busy}>
              <FolderOpen /> Change…
            </Button>
            {settings.cacheDir && (
              <Button variant="ghost" size="sm" onClick={() => void update({ cacheDir: undefined }, () => void qc.invalidateQueries({ queryKey: qk.cache }))}>
                Default
              </Button>
            )}
          </Field>
          <Field label="Cache size" hint="Downloaded packages are kept so reinstalls are instant.">
            <span className="text-[13px] tabular-nums">{cache ? formatBytes(cache.bytes) : "…"}</span>
            <Button variant="outline" size="sm" disabled={busy || clearing || !cache?.bytes} onClick={() => void clearCache()}>
              {clearing && <LoaderCircle className="animate-spin" />} Clear
            </Button>
          </Field>
        </Section>

        <Section title="Drives">
          <Field label="Show all drives" hint="Some USB hard drives report themselves as fixed disks.">
            <Switch checked={settings.showAllDrives} onCheckedChange={(v) => void update({ showAllDrives: v }, () => void qc.invalidateQueries({ queryKey: qk.drives }))} />
          </Field>
        </Section>

        <Section title="Updates">
          <Field
            label="Update channel"
            hint={info?.updaterConfigured ? "Checked at launch and every 6 hours." : "This build has no update signing key configured."}
          >
            <Segmented
              ariaLabel="Update channel"
              value={settings.updateChannel}
              onChange={(updateChannel) => void update({ updateChannel })}
              options={[
                { value: "stable", label: "Stable" },
                { value: "beta", label: "Beta" },
                { value: "off", label: "Off" },
              ]}
            />
          </Field>
          <Field label="Version" hint={info ? `${info.platform} · ${MATERIAL[info.material]}` : undefined}>
            <span className="text-[13px] tabular-nums">{info?.version}</span>
            <Button variant="outline" size="sm" disabled={!info?.updaterConfigured || checking || settings.updateChannel === "off"} onClick={() => void checkUpdates()}>
              {checking && <LoaderCircle className="animate-spin" />} Check now
            </Button>
          </Field>
        </Section>
      </div>
    </div>
  );
}
