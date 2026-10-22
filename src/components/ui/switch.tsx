import { Switch as SwitchPrimitive } from "@base-ui/react/switch";
import type * as React from "react";
import { cn } from "@/lib/utils";

export function Switch({ className, ...props }: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border border-transparent bg-foreground/15 outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring data-[checked]:bg-primary data-[disabled]:opacity-50",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="pointer-events-none block size-4 translate-x-0.5 rounded-full bg-white shadow-sm transition-transform data-[checked]:translate-x-[18px]" />
    </SwitchPrimitive.Root>
  );
}
