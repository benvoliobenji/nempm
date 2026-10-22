import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1 whitespace-nowrap rounded-full px-2 py-0.5 text-[11px] font-medium leading-4 [&_svg]:size-3",
  {
    variants: {
      variant: {
        default: "bg-primary/12 text-primary dark:bg-primary/20",
        secondary: "bg-secondary text-secondary-foreground",
        outline: "border border-border text-muted-foreground",
        success: "bg-success/12 text-success dark:bg-success/18",
        warning: "bg-warning/15 text-[color-mix(in_oklch,var(--warning)_75%,var(--foreground))] dark:text-warning",
        destructive: "bg-destructive/12 text-destructive dark:bg-destructive/20",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

export function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}
