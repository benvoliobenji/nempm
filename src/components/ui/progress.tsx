import { Progress as ProgressPrimitive } from "@base-ui/react/progress";
import { cn } from "@/lib/utils";

export function Progress({
  value,
  className,
  indicatorClassName,
  label,
}: {
  /** 0–100, or null for indeterminate. */
  value: number | null;
  className?: string;
  indicatorClassName?: string;
  label?: string;
}) {
  return (
    <ProgressPrimitive.Root value={value} aria-label={label} className={cn("w-full", className)}>
      <ProgressPrimitive.Track className="relative h-1.5 w-full overflow-hidden rounded-full bg-foreground/10">
        <ProgressPrimitive.Indicator
          className={cn(
            "h-full rounded-full bg-primary transition-[width] duration-200 data-[indeterminate]:w-1/3 data-[indeterminate]:animate-pulse",
            indicatorClassName,
          )}
        />
      </ProgressPrimitive.Track>
    </ProgressPrimitive.Root>
  );
}
