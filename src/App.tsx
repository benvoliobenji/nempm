import { Toaster } from "sonner";
import { ConfirmInstallDialog } from "@/components/confirm-install";
import { DrivePanel } from "@/components/drive-panel";
import { PackageDetailDialog } from "@/components/package-detail";
import { Sidebar } from "@/components/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useDocumentAttributes, useResolvedTheme, useUpdater } from "@/hooks/useEnvironment";
import { useProgressEvents, useQueueRunner } from "@/hooks/useInstaller";
import { BrowseScreen } from "@/screens/browse";
import { InstalledScreen } from "@/screens/installed";
import { QueueScreen } from "@/screens/queue";
import { SettingsScreen } from "@/screens/settings";
import { useApp } from "@/stores/app";

function Screen() {
  const view = useApp((s) => s.view);
  switch (view) {
    case "browse":
      return <BrowseScreen />;
    case "installed":
      return <InstalledScreen />;
    case "queue":
      return <QueueScreen />;
    case "settings":
      return <SettingsScreen />;
  }
}

export default function App() {
  useDocumentAttributes();
  useProgressEvents();
  useQueueRunner();
  useUpdater();
  const theme = useResolvedTheme();

  return (
    <TooltipProvider delay={400}>
      <div className="flex h-full">
        <Sidebar />
        <main className="flex min-w-0 flex-1 flex-col border-l bg-surface">
          <Screen />
        </main>
        <DrivePanel />
      </div>
      <PackageDetailDialog />
      <ConfirmInstallDialog />
      <Toaster
        theme={theme}
        position="bottom-center"
        toastOptions={{ classNames: { toast: "frosted !bg-popover !border-border !text-foreground !shadow-xl" } }}
      />
    </TooltipProvider>
  );
}
