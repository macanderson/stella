#!/usr/bin/env node
/**
 * Acceptance checks for the transcript page's reading tools.
 *
 * The page has no test runner — it is a static-exported Next.js client, and
 * standing one up to test four pure functions would cost more than it proves.
 * So the decisions live in `lib/transcript-view.ts` and this script drives them
 * the way `check-diff-view-parity.mjs` drives `lib/diff-view.ts`: zero
 * dependencies, Node stripping the type annotations natively, runnable against
 * a bare checkout.
 *
 *   node scripts/check-transcript-view.mjs
 *
 * What is checked here is what a reader would notice being wrong:
 *
 * - an errors-only view that shows a failure without the call that caused it;
 * - a JSON fold whose close is on the wrong line, which silently swallows the
 *   rest of the payload;
 * - a `command` string rendered as one 4000-character line because its `\n`
 *   escapes were left alone;
 * - a drawer that reports a request or a response it does not have.
 */

import {
  erroredSeqs,
  foldableLines,
  formatJson,
  hiddenLines,
  indexByCallId,
  isFailedResult,
  lineText,
  rawExchange,
} from "../lib/transcript-view.ts";

let failures = 0;
let checks = 0;

function check(name, condition, detail = "") {
  checks += 1;
  if (condition) return;
  failures += 1;
  console.error(`  ✗ ${name}${detail ? `\n      ${detail}` : ""}`);
}

function eq(name, got, want) {
  const same = JSON.stringify(got) === JSON.stringify(want);
  check(name, same, same ? "" : `got  ${JSON.stringify(got)}\n      want ${JSON.stringify(want)}`);
}

const entry = (seq, kind, extra = {}) => ({
  seq,
  t: seq,
  kind,
  title: extra.title ?? kind,
  body: extra.body ?? "",
  meta: extra.meta ?? {},
});

// -- the errors-only filter ------------------------------------------------

console.log("errors-only filter");
{
  const entries = [
    entry(1, "text", { body: "planning" }),
    entry(2, "tool", { title: "bash", meta: { call_id: "c1" } }),
    entry(3, "tool_result", { title: "bash", meta: { call_id: "c1" } }),
    entry(4, "tool", { title: "edit_file", meta: { call_id: "c2" } }),
    entry(5, "tool_result", {
      title: "edit_file",
      meta: { call_id: "c2", error: true },
      body: "error: no such file",
    }),
    entry(6, "error", { body: "the provider hung up" }),
  ];

  const kept = erroredSeqs(entries);
  eq("keeps the failed result, its call, and the engine error", [...kept].sort((a, b) => a - b), [4, 5, 6]);
  check("drops the successful exchange", !kept.has(2) && !kept.has(3));
  check("drops prose", !kept.has(1));

  check("a successful result is not a failure", !isFailedResult(entries[2]));
  check("a failed result is", isFailedResult(entries[4]));

  eq("nothing errored means nothing kept", [...erroredSeqs(entries.slice(0, 3))], []);
}

// A failed result whose call never arrived must still be kept: half a story is
// better than dropping the only evidence of the failure.
{
  const orphan = [entry(9, "tool_result", { meta: { call_id: "gone", error: true } })];
  eq("an orphaned failure is still kept", [...erroredSeqs(orphan)], [9]);
}

// -- the raw exchange ------------------------------------------------------

console.log("raw exchange");
{
  const call = entry(2, "tool", {
    title: "bash",
    meta: { call_id: "c1", raw: '{\n  "command": "ls"\n}' },
  });
  const result = entry(3, "tool_result", {
    title: "bash",
    meta: { call_id: "c1" },
    body: "a.txt\nb.txt",
  });
  const index = indexByCallId([call, result]);

  const fromCall = rawExchange(call, index);
  const fromResult = rawExchange(result, index);
  check("the drawer opens from either half", fromCall !== null && fromResult !== null);
  eq("and shows the same exchange", fromCall, fromResult);
  eq("titled by tool and call id", fromCall.title, "bash · c1");
  check("the request is the call's arguments", fromCall.request.text.includes('"command"'));
  check("the request is recognised as JSON", fromCall.request.json === true);
  check("the response is the result body", fromCall.response.text === "a.txt\nb.txt");
  check("plain output is not claimed to be JSON", fromCall.response.json === false);

  const failed = entry(4, "tool_result", { meta: { call_id: "c1", error: true } });
  const bad = rawExchange(failed, indexByCallId([call, failed]));
  check("a failed response says so in its label", bad.response.label.includes("failed"));
}
{
  // A call still in flight has no result, and that must be reported as absent
  // rather than as an empty response.
  const call = entry(2, "tool", { title: "bash", meta: { call_id: "c9", raw: '{"a":1}' } });
  const exchange = rawExchange(call, indexByCallId([call]));
  check("a call with no result yet has a null response", exchange.response === null);
  check("and still has its request", exchange.request !== null);
}
{
  const usage = entry(1, "usage", { meta: { model: "claude-opus-5" } });
  check("a non-exchange row has no drawer", rawExchange(usage, new Map()) === null);
  const bare = entry(1, "tool", { title: "bash", meta: { call_id: "c", raw: "{}" } });
  check("a call with no arguments and no result has no drawer", rawExchange(bare, indexByCallId([bare])) === null);
}

// -- JSON formatting -------------------------------------------------------

console.log("json formatting");
{
  const lines = formatJson('{"a": 1, "b": [true, null], "c": {}}');
  eq(
    "renders one line per structural element",
    lines.map(lineText),
    ['{', '  "a": 1,', '  "b": [', "    true,", "    null", "  ],", '  "c": {}', "}"],
  );
  eq("numbering is 1-based and dense", lines.map((l) => l.no), [1, 2, 3, 4, 5, 6, 7, 8]);

  const opener = lines.find((l) => lineText(l).includes('"b"'));
  eq("an opener knows the line its close is on", opener.closesAt, 6);
  eq("the root closes at the last line", lines[0].closesAt, 8);
  eq("an empty object is one line, not a fold", lines[6].closesAt, undefined);

  eq("folding the array hides exactly its body", [...hiddenLines(lines, new Set([3]))].sort((a, b) => a - b), [4, 5]);
  eq("the root is not offered as a fold", foldableLines(lines), [3]);
}
{
  // The case the drawer exists for: a `bash` command or a written file, which
  // `JSON.stringify` renders as one line however long the payload is.
  const lines = formatJson(JSON.stringify({ command: "set -e\ncargo test\n" }));
  eq(
    "an embedded newline becomes a real line",
    lines.map(lineText),
    ['{', '  "command": "set -e\\n', "    cargo test\\n", '    "', "}"],
  );
  check("every line is numbered after the split", lines.every((l, i) => l.no === i + 1));
}
{
  // Nested folds compose by union: collapsing the outer object while the inner
  // one is also collapsed must not reopen the inner one.
  const lines = formatJson('{"outer": {"inner": [1, 2]}}');
  const all = foldableLines(lines);
  const hiddenBoth = hiddenLines(lines, new Set(all));
  const hiddenOuter = hiddenLines(lines, new Set([all[0]]));
  check("an outer fold hides at least what the inner one does", [...hiddenBoth].every((n) => hiddenOuter.has(n)));
}
{
  eq("plain text still numbers its lines", formatJson("one\ntwo\n").map(lineText), ["one", "two"]);
  eq("an empty payload is one empty line", formatJson("").map(lineText), [""]);
  eq("malformed JSON degrades to text, never throws", formatJson("{not json").map(lineText), ["{not json"]);
  eq("a bare scalar is not JSON here", formatJson("42").map(lineText), ["42"]);
}

// -------------------------------------------------------------------------

if (failures > 0) {
  console.error(`\n${failures} of ${checks} checks failed.`);
  process.exit(1);
}
console.log(`\n${checks} checks passed.`);
