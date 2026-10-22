import { Select as SelectPrimitive } from "@base-ui/react/select";
import { Check, ChevronsUpDown } from "lucide-react";
import type * as React from "react";
import { cn } from "@/lib/utils";

export interface SelectOption {
  value: string;
  label: React.ReactNode;
  /** Shown under the label in the list. */
  hint?: React.ReactNode;
  disabled?: boolean;
}

/** A single-value select built on Base UI. */
export function Select({
  value,
  onValueChange,
  options,
  placeholder = "Select…",
  disabled,
  className,
  ariaLabel,
}: {
  value: string | null;
  onValueChange: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  ariaLabel?: string;
}) {
  const items = options.map((o) => ({ value: o.value, label: o.label }));
  return (
    <SelectPrimitive.Root
      items={items}
      value={value}
      disabled={disabled}
      onValueChange={(v) => {
        if (typeof v === "string") onValueChange(v);
      }}
    >
      <SelectPrimitive.Trigger
        aria-label={ariaLabel}
        className={cn(
          "flex h-8 w-full min-w-0 items-center justify-between gap-2 rounded-md border border-input bg-card/70 px-2.5 text-left text-[13px] outline-none transition-[box-shadow] focus-visible:ring-[3px] focus-visible:ring-ring data-[disabled]:opacity-50 data-[popup-open]:ring-[3px] data-[popup-open]:ring-ring",
          className,
        )}
      >
        <SelectPrimitive.Value className="truncate data-[placeholder]:text-muted-foreground" placeholder={placeholder} />
        <SelectPrimitive.Icon className="text-muted-foreground">
          <ChevronsUpDown className="size-3.5" />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Positioner className="z-50 outline-none" sideOffset={4} alignItemWithTrigger={false}>
          <SelectPrimitive.Popup className="frosted min-w-[var(--anchor-width)] overflow-hidden rounded-lg border bg-popover p-1 shadow-xl outline-none transition-[opacity,transform] duration-100 data-[ending-style]:opacity-0 data-[starting-style]:-translate-y-1 data-[starting-style]:opacity-0">
            <SelectPrimitive.List className="max-h-72 overflow-y-auto scrollbar-thin">
              {options.map((o) => (
                <SelectPrimitive.Item
                  key={o.value}
                  value={o.value}
                  disabled={o.disabled}
                  className="relative flex cursor-default items-start gap-2 rounded-md py-1.5 pr-2 pl-7 text-[13px] outline-none select-none data-[disabled]:opacity-50 data-[highlighted]:bg-primary data-[highlighted]:text-primary-foreground"
                >
                  <SelectPrimitive.ItemIndicator className="absolute top-2 left-2">
                    <Check className="size-3.5" />
                  </SelectPrimitive.ItemIndicator>
                  <div className="min-w-0">
                    <SelectPrimitive.ItemText className="block truncate">{o.label}</SelectPrimitive.ItemText>
                    {o.hint && <div className="truncate text-[11px] opacity-70">{o.hint}</div>}
                  </div>
                </SelectPrimitive.Item>
              ))}
            </SelectPrimitive.List>
          </SelectPrimitive.Popup>
        </SelectPrimitive.Positioner>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}
