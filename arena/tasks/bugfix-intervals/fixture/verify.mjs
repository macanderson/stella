// Arena verifier — do not modify. Exit 0 = task solved.
import { mergeIntervals } from "./intervals.mjs";

const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const checks = [
  [() => eq(mergeIntervals([[1, 3], [2, 6], [8, 10]]), [[1, 6], [8, 10]]), "overlap merge"],
  [() => eq(mergeIntervals([[1, 3], [3, 5]]), [[1, 5]]), "touching intervals merge"],
  [() => eq(mergeIntervals([[5, 7], [1, 2]]), [[1, 2], [5, 7]]), "sorted output"],
  [() => eq(mergeIntervals([]), []), "empty input"],
  [() => eq(mergeIntervals([[1, 10], [2, 3]]), [[1, 10]]), "contained interval"],
  [
    () => {
      const input = [[4, 5], [1, 2]];
      mergeIntervals(input);
      return eq(input, [[4, 5], [1, 2]]) && input[0][0] === 4;
    },
    "input not mutated",
  ],
];

let failed = 0;
for (const [check, name] of checks) {
  let ok = false;
  try {
    ok = check();
  } catch (e) {
    console.error(`  ✗ ${name} threw: ${e.message}`);
    failed++;
    continue;
  }
  if (!ok) {
    console.error(`  ✗ ${name}`);
    failed++;
  }
}
console.log(failed === 0 ? "verify: PASS" : `verify: FAIL (${failed}/${checks.length})`);
process.exit(failed === 0 ? 0 : 1);
