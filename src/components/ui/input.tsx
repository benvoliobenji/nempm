import type * as React from "react";
import { cn } from "@/lib/utils";

export function Input({ className, type = "text", ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      spellCheck={false}
      className={cn(
        "h-8 w-full min-w-0 rounded-md border border-input bg-card/70 px-2.5 text-[13px] outline-none transition-[box-shadow,border-color] placeholder:text-muted-foreground focus-visible:border-primary/60 focus-visible:ring-[3px] focus-visible:ring-ring disabled:opacity-50 aria-invalid:border-destructive",
        className,
      )}
      {...props}
    />
  );
}
