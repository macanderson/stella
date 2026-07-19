// Merge overlapping or touching [start, end] intervals; result sorted by start.
// The input array must not be mutated.
export function mergeIntervals(intervals) {
  if (intervals.length === 0) return [];
  const sorted = intervals.map((iv) => [...iv]).sort((a, b) => a[0] - b[0]);
  const out = [sorted[0]];
  for (let i = 1; i < sorted.length; i++) {
    const last = out[out.length - 1];
    const cur = sorted[i];
    if (cur[0] <= last[1]) {
      last[1] = Math.max(last[1], cur[1]);
    } else {
      out.push(cur);
    }
  }
  return out;
}
