import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex cursor-pointer items-center justify-center gap-1.5 rounded-[7px] border font-mono lowercase transition-colors " +
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 " +
    "disabled:cursor-not-allowed disabled:opacity-40 active:translate-y-px",
  {
    variants: {
      variant: {
        default: "border-line bg-panel-2 text-foreground hover:border-dim",
        primary:
          "border-gold bg-gold font-[650] text-ink hover:bg-[#ffc02e] hover:border-[#ffc02e]",
        ghost: "border-line bg-transparent text-foreground hover:border-dim",
        danger: "border-line bg-transparent text-bad hover:border-bad/60",
        dashed:
          "border-dashed border-line bg-transparent text-muted hover:border-accent hover:text-accent",
      },
      size: {
        default: "px-3.5 py-2 text-[13px]",
        sm: "px-2.5 py-[5px] text-xs",
        lg: "px-6 py-3 text-[15px]",
        icon: "size-8 p-0",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  },
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

export function Button({ className, variant, size, type, ...props }: ButtonProps) {
  return (
    <button
      type={type ?? "button"}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  );
}

export { buttonVariants };
