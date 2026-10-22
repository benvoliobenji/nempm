import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import type * as React from "react";

export const TooltipProvider = TooltipPrimitive.Provider;

export function Tooltip({ content, children }: { content: React.ReactNode; children: React.ReactElement }) {
  return (
    <TooltipPrimitive.Root>
      <TooltipPrimitive.Trigger render={children} />
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Positioner sideOffset={6} className="z-50">
          <TooltipPrimitive.Popup className="max-w-64 rounded-md bg-foreground px-2 py-1 text-[11px] text-background shadow-md transition-opacity duration-100 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
            {content}
          </TooltipPrimitive.Popup>
        </TooltipPrimitive.Positioner>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}
