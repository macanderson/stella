// The `mock-solver` agent: copy the task's known-good solution files over the
// workspace. Zero cost, always "solves" — used to smoke-test the harness and
// to prove each task's verify.mjs accepts at least one solution.
import fs from "node:fs";

const [solutionDir, workspace] = process.argv.slice(2);
if (!solutionDir || !workspace) {
  console.error("usage: mock-solver.mjs <solution-dir> <workspace>");
  process.exit(2);
}
if (!fs.existsSync(solutionDir)) {
  console.error(`no solution/ directory for this task: ${solutionDir}`);
  process.exit(1);
}
fs.cpSync(solutionDir, workspace, { recursive: true });
console.log(JSON.stringify({ mock: "solver", appliedFrom: solutionDir }));
