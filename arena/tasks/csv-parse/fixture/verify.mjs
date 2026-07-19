// Arena verifier — do not modify. Exit 0 = task solved.
import { parseCsv } from "./csv.mjs";

const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const checks = [
  [() => eq(parseCsv("a,b,c"), [["a", "b", "c"]]), "simple row"],
  [() => eq(parseCsv("a,b\nc,d\n"), [["a", "b"], ["c", "d"]]), "trailing newline"],
  [() => eq(parseCsv("a,b\r\nc,d"), [["a", "b"], ["c", "d"]]), "CRLF"],
  [() => eq(parseCsv('"a,x",b'), [["a,x", "b"]]), "comma in quotes"],
  [() => eq(parseCsv('"a\nx",b'), [["a\nx", "b"]]), "newline in quotes"],
  [() => eq(parseCsv('"say ""hi""",b'), [['say "hi"', "b"]]), "escaped quotes"],
  [() => eq(parseCsv(""), []), "empty input"],
  [() => eq(parseCsv("a,,c"), [["a", "", "c"]]), "empty field"],
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
