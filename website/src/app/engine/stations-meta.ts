/**
 * The tour's station list — the one copy both the page (section order) and the
 * shell (progress rail, keyboard jumps) read, so the rail can never disagree
 * with the sections it points at.
 *
 * `id` is the section's DOM id and its deep-link hash (`/engine#vera`);
 * `code` is the HUD's index label; `name` is the rail chip. Names are written
 * lowercase in the data because the tour's display voice is lowercase — the
 * CSS never has to transform what is already true in the markup (the same
 * reasoning as releases.css's `.rl-title`).
 */
export type StationMeta = {
  id: string;
  code: string;
  name: string;
};

export const STATIONS: readonly StationMeta[] = [
  { id: "intake", code: "00", name: "intake" },
  { id: "loop", code: "01", name: "turn loop" },
  { id: "tools", code: "02", name: "tool bay" },
  { id: "vera", code: "03", name: "vera" },
  { id: "governance", code: "04", name: "governance" },
  { id: "evolution", code: "05", name: "evolution" },
  { id: "exit", code: "06", name: "exit" },
] as const;
