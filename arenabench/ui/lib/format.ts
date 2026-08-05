// Number formatting for the scoreboard. Ported verbatim from the previous
// client so a match rendered by the old UI and the new one read identically.

export function fmtTokens(value: unknown): string {
  const n = Number(value) || 0;
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "k";
  return String(Math.round(n));
}

// An absent cost is drawn as absent. A model with no entry in the price table
// yields null, and rendering that as "$0.0000" would put a fabricated number in
// a column that sorts — the one failure the whole pricing path exists to avoid.
export function fmtMoney(value: unknown): string {
  if (value == null) return "—";
  const n = Number(value) || 0;
  return n >= 100 ? "$" + n.toFixed(0) : n >= 1 ? "$" + n.toFixed(2) : "$" + n.toFixed(4);
}

export function fmtClock(value: unknown): string {
  const seconds = Math.max(0, Math.round(Number(value) || 0));
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return h
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
}

export function fmtPct(value: unknown): string {
  return (Number(value) || 0).toFixed(0) + "%";
}

export function fmtDim(key: string, value: unknown): string {
  switch (key) {
    case "solve_rate":
      return fmtPct(value);
    case "clock_time":
      return fmtClock(value);
    case "priced_cost":
    case "total_cost":
      return fmtMoney(value);
    default:
      return fmtTokens(value);
  }
}
