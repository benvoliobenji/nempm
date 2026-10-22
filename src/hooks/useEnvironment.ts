import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api } from "@/lib/api";
import { errorMessage } from "@/lib/errors";
import { selectRunning, useQueue } from "@/stores/queue";
import { useAppInfo, useSettings } from "./queries";

const media = () => window.matchMedia("(prefers-color-scheme: dark)");

/** "light" or "dark", following the setting or the OS. */
export function useResolvedTheme(): "light" | "dark" {
  const { data: settings } = useSettings();
  const [systemDark, setSystemDark] = useState(() => media().matches);
  useEffect(() => {
    const m = media();
    const onChange = () => setSystemDark(m.matches);
    m.addEventListener("change", onChange);
    return () => m.removeEventListener("change", onChange);
  }, []);
  const theme = settings?.theme ?? "system";
  return theme === "dark" || (theme === "system" && systemDark) ? "dark" : "light";
}

/** Applies theme, window material and platform to <html>. Mount once. */
export function useDocumentAttributes() {
  const theme = useResolvedTheme();
  const { data: info } = useAppInfo();
  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
    document.documentElement.style.colorScheme = theme;
  }, [theme]);
  useEffect(() => {
    if (!info) return;
    document.documentElement.dataset.material = info.material;
    document.documentElement.dataset.platform = info.platform;
  }, [info]);
}

const SIX_HOURS = 6 * 60 * 60 * 1000;

/**
 * Checks for app updates on launch and every 6 hours, downloads in the
 * background and offers a restart. Never installs while a drive is written.
 */
export function useUpdater() {
  const { data: info } = useAppInfo();
  const { data: settings } = useSettings();
  const channel = settings?.updateChannel;

  useEffect(() => {
    if (!info?.updaterConfigured || !channel || channel === "off") return;
    let toastId: string | number | undefined;
    const run = async () => {
      try {
        const update = await api.checkUpdate();
        if (!update) return;
        const ready = update.downloaded ? update : await api.downloadUpdate();
        toastId = toast(`Update ready: version ${ready.version}`, {
          id: "app-update",
          description: "Restart to finish updating. Nothing on your drive is affected.",
          duration: Infinity,
          action: {
            label: "Restart",
            onClick: () => {
              if (selectRunning(useQueue.getState())) {
                toast.info("Finish the current install first, then restart.");
                return;
              }
              api.applyUpdate().catch((err) => toast.error(errorMessage(err)));
            },
          },
        });
      } catch (err) {
        // Update checks are best-effort; don't nag when offline.
        console.warn("Update check failed:", errorMessage(err));
      }
    };
    void run();
    const timer = window.setInterval(run, SIX_HOURS);
    return () => {
      window.clearInterval(timer);
      if (toastId !== undefined) toast.dismiss(toastId);
    };
  }, [info?.updaterConfigured, channel]);
}

/** Manual "Check now" from Settings. */
export async function checkForUpdatesNow(): Promise<string> {
  const update = await api.checkUpdate();
  if (!update) return "You're on the latest version.";
  const ready = update.downloaded ? update : await api.downloadUpdate();
  return `Version ${ready.version} is downloaded. Restart from the notification to install it.`;
}
