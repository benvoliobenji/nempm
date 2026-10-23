import { ArrowDown, ShieldCheck, TriangleAlert } from "lucide-react";
import { useState } from "react";
import type { PlanStep } from "@/bindings/PlanStep";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";
import { useAppInfo, useSaveSettings, useSettings } from "@/hooks/queries";
import { enqueue } from "@/hooks/useInstaller";
import { formatBytes, unpackLabel } from "@/lib/utils";
import { useApp } from "@/stores/app";

const ACTION: Record<string, string> = {
  install: "Install",
  update: "Update",
  reinstall: "Reinstall",
  downgrade: "Downgrade",
};

function writes(step: PlanStep): string[] {
  const target = step.extract ? unpackLabel(step.target) : step.target;
  return step.dataTarget ? [step.dataTarget, target] : [target];
}

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
  const swapping = (plan?.removes.length ?? 0) > 0;
  const first = plan?.steps[0];

  const start = () => {
    if (!confirm) return;
    // Clicking the button is the consent for the swap.
    enqueue({ ...confirm, options: { ...confirm.options, replaceUntracked: replace, replaceInstalled: swapping } });
    if (macNote && settings) save.mutate({ ...settings, writePermissionExplained: true });
    close();
    setView("queue");
  };

  const title = !plan
    ? ""
    : swapping && first
      ? `Replace ${plan.removes.map((r) => r.name).join(", ")} with ${first.name}?`
      : first
        ? `${ACTION[first.action]} ${first.name}?`
        : `Remove ${plan.removes.map((r) => r.name).join(", ")}?`;

  return (
    <Dialog open={!!confirm} onOpenChange={(open) => !open && close()}>
      {plan && confirm && (
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>
              {swapping
                ? `${confirm.driveLabel} holds one package at a time, so what's on it now comes off first.`
                : `To ${confirm.driveLabel} · ${formatBytes(plan.totalBytes)}`}
            </DialogDescription>
          </DialogHeader>

          <div className="flex flex-col gap-1.5">
            {plan.removes.map((r) => (
              <div key={r.id} className="rounded-lg border border-dashed bg-card/40 px-3 py-2 text-[13px]">
                <div className="flex items-center justify-between gap-3">
                  <span className="font-medium text-muted-foreground line-through decoration-foreground/30">
                    {r.name}
                  </span>
                  <span className="text-[12px] text-muted-foreground tabular-nums">Remove {r.version}</span>
                </div>
                <code className="text-[11px] text-muted-foreground">{r.paths.slice(0, 3).join(", ")}</code>
              </div>
            ))}
            {swapping && plan.steps.length > 0 && (
              <ArrowDown className="mx-auto size-4 text-muted-foreground" aria-hidden />
            )}
            {plan.steps.map((s) => (
              <div key={s.id} className="rounded-lg border bg-card/60 px-3 py-2 text-[13px]">
                <div className="flex items-center justify-between gap-3">
                  <span className="truncate font-medium">{s.name}</span>
                  <span className="text-right text-[12px] text-muted-foreground tabular-nums">
                    {ACTION[s.action]} {s.fromVersion ? `${s.fromVersion} → ` : ""}
                    {s.version} · {formatBytes(s.size)}
                  </span>
                </div>
                <code className="text-[11px] text-muted-foreground">{writes(s).join(", ")}</code>
              </div>
            ))}
          </div>

          {needsReplace && (
            <div className="flex gap-3 rounded-lg border border-warning/40 bg-warning/10 p-3 text-[13px]">
              <TriangleAlert className="mt-0.5 size-4 shrink-0 text-warning" />
              <div className="flex-1">
                <p>
                  {plan.untracked.join(", ")} {plan.untracked.length === 1 ? "is" : "are"} already on the drive but
                  {plan.untracked.length === 1 ? " wasn't" : " weren't"} put there by this app.
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
              {swapping ? "Replace" : first ? ACTION[first.action] : "Remove"}
            </Button>
          </DialogFooter>
        </DialogContent>
      )}
    </Dialog>
  );
}
