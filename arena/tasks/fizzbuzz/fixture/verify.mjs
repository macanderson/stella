// Arena verifier — do not modify. Exit 0 = task solved.
import { fizzbuzz } from "./fizzbuzz.mjs";

const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const checks = [
  [() => eq(fizzbuzz(5), [1, 2, "Fizz", 4, "Buzz"]), "fizzbuzz(5)"],
  [() => eq(fizzbuzz(15).slice(11), ["Fizz", 13, 14, "FizzBuzz"]), "fizzbuzz(15) tail"],
  [() => eq(fizzbuzz(0), []), "fizzbuzz(0)"],
  [() => eq(fizzbuzz(-3), []), "fizzbuzz(-3)"],
  [() => typeof fizzbuzz(2)[0] === "number", "numbers stay numbers"],
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
