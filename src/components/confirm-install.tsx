import { ShieldCheck, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";
import { useAppInfo, useSaveSettings, useSettings } from "@/hooks/queries";
import { enqueue } from "@/hooks/useInstaller";
import { formatBytes } from "@/lib/utils";
import { useApp } from "@/stores/app";

const ACTION: Record<string, string> = {
  install: "Install",
  update: "Update",
  reinstall: "Reinstall",
  downgrade: "Downgrade",
};

export function ConfirmInstallDialog() {
  const { confirm, setConfirm, setView } = useApp();
  const { data: info } = useAppInfo();
  const { data: settings } = useSettings();
  const save = useSaveSettings();
  const [replace, setReplace] = useState(false);

  const close = () => {
    setConfirm(null);
    setReplace(false);
  };
  const plan = confirm?.plan;
  const macNote = info?.platform === "macos" && !settings?.writePermissionExplained;
  const needsReplace = (plan?.untracked.length ?? 0) > 0;

  const start = () => {
    if (!confirm) return;
    enqueue({ ...confirm, options: { ...confirm.options, replaceUntracked: replace } });
    if (macNote && settings) save.mutate({ ...settings, writePermissionExplained: true });
    close();
    setView("queue");
  };

  return (
    <Dialog open={!!confirm} onOpenChange={(open) => !open && close()}>
      {plan && confirm && (
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {plan.steps.length === 1 ? `${ACTION[plan.steps[0]!.action]} ${plan.steps[0]!.name}?` : `Install ${plan.steps.length} packages?`}
            </DialogTitle>
            <DialogDescription>
              To {confirm.driveLabel} · {formatBytes(plan.totalBytes)} total
            </DialogDescription>
          </DialogHeader>

          <ol className="flex flex-col divide-y rounded-lg border bg-card/60">
            {plan.steps.map((s) => (
              <li key={s.id} className="flex items-center gap-3 px-3 py-2 text-[13px]">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium">{s.name}</span>
                    {s.reason === "dependency" && <Badge variant="secondary">Required</Badge>}
                  </div>
                  <code className="text-[11px] text-muted-foreground">{s.target}</code>
                </div>
                <span className="text-right text-[12px] text-muted-foreground tabular-nums">
                  {ACTION[s.action]} {s.fromVersion ? `${s.fromVersion} → ` : ""}
                  {s.version}
                  <br />
                  {formatBytes(s.size)}
                </span>
              </li>
            ))}
          </ol>

          {needsReplace && (
            <div className="flex gap-3 rounded-lg border border-warning/40 bg-warning/10 p-3 text-[13px]">
              <TriangleAlert className="mt-0.5 size-4 shrink-0 text-warning" />
              <div className="flex-1">
                <p>
                  {plan.untracked.join(", ")} {plan.untracked.length === 1 ? "is" : "are"} already on the drive but
                  {plan.untracked.length === 1 ? " wasn't" : " weren't"} installed by this app.
                </p>
                <label className="mt-2 flex items-center gap-2 font-medium">
                  <Switch checked={replace} onCheckedChange={setReplace} /> Replace{" "}
                  {plan.untracked.length === 1 ? "it" : "them"}
                </label>
              </div>
            </div>
          )}

          {macNote && (
            <div className="flex gap-3 rounded-lg border bg-card/60 p-3 text-[13px]">
              <ShieldCheck className="mt-0.5 size-4 shrink-0 text-primary" />
              <p>
                macOS will ask whether this app may access files on a removable volume. Choose <b>Allow</b> so it can
                write to the drive. You'll only be asked once.
              </p>
            </div>
          )}

          <DialogFooter>
            <Button variant="ghost" onClick={close}>
              Cancel
            </Button>
            <Button onClick={start} disabled={needsReplace && !replace}>
              {plan.steps.length === 1 ? ACTION[plan.steps[0]!.action] : "Install all"}
            </Button>
          </DialogFooter>
        </DialogContent>
      )}
    </Dialog>
  );
}
