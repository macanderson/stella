import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Seat colours, in seating order. Mirrors the server's own default cycle. */
export const PALETTE = [
  "#FFB000",
  "#37D5F2",
  "#C77DFF",
  "#5CE68A",
  "#FF6B9D",
  "#FFE066",
  "#7AA2F7",
  "#FF8C42",
];

/** CSS custom property helper for the per-seat accent. */
export function seatStyle(color: string | undefined): React.CSSProperties {
  return { "--seat": color } as React.CSSProperties;
}
