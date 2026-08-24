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
 * - a transcript that opens on anything but the prompt — the task instruction
 *   rides the reserved seq 0 and must head the reading order however late its
 *   `block_registered` lands in the stream (#4039);
 * - an errors-only view that shows a failure without the call that caused it;
 * - a JSON fold whose close is on the wrong line, which silently swallows the
 *   rest of the payload;
 * - a `command` string rendered as one 4000-character line because its `\n`
 *   escapes were left alone;
 * - a drawer that reports a request or a response it does not have;
 * - a folded prose block showing something other than its first sentence, or
 *   an expanded one that lost bytes (#4578, transcript-spec addendum §1);
 * - a collapsed turn whose digest disagrees with the trial it stands for.
 */

import {
  elide,
  erroredSeqs,
  firstSentence,
  foldableLines,
  formatJson,
  hiddenLines,
  indexByCallId,
  isFailedResult,
  lineText,
  mergeToolRows,
  orderEntries,
  proseFold,
  rawExchange,
  turnDigest,
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

// -- the prompt heads the transcript ---------------------------------------

// The wire carries the task instruction as a `block_registered` event that
// lands AFTER the trial's first stage rule, so arrival order buries the
// question under a row of the answer. The reader stamps it with the reserved
// seq 0 (`arenabench/arenabench/transcript.py`), and this is the other half of
// that contract: the reading order is `seq` order, so the entry list begins
// with the prompt row.
console.log("prompt ordering");
{
  const arrived = [
    entry(1, "stage", { title: "execute" }),
    entry(2, "tool", { title: "bash", meta: { call_id: "c1" } }),
    entry(0, "prompt", { title: "prompt", body: "Fix the bug." }),
  ];
  const ordered = orderEntries(arrived);
  eq("the entry list begins with the prompt row", ordered[0].kind, "prompt");
  eq("and everything else keeps its order", ordered.map((e) => e.seq), [0, 1, 2]);
  eq("the input list is not reordered in place", arrived[2].kind, "prompt");

  // The prompt is its own row: nothing folds into it, and it does not
  // displace the call/result pairing behind it.
  const rows = mergeToolRows(ordered);
  eq("the prompt is its own row and stays first", rows[0].entry.kind, "prompt");
  check("with nothing folded into it", rows[0].result === undefined);
}

// -- one row per call ------------------------------------------------------

// The defect: a `tool` and its `tool_result` rendered as two rows, so the tool's
// name was printed twice and the invocation up to three times, all the way down
// the transcript. `crates/stella-transcript` makes this structurally impossible
// by having a Call OWN its Output; over the wire's flat stream, this is where
// the same shape is recovered.
console.log("one row per call");
{
  const entries = [
    entry(1, "text", { body: "planning" }),
    entry(2, "tool", { title: "bash", body: "ls", meta: { call_id: "c1" } }),
    entry(3, "tool_result", { title: "bash", body: "a.txt", meta: { call_id: "c1" } }),
    entry(4, "tool", { title: "read_file", meta: { call_id: "c2" } }),
    entry(5, "tool_result", { title: "read_file", meta: { call_id: "c2" } }),
  ];

  const rows = mergeToolRows(entries);
  eq("two calls and a prose row make three rows, not five", rows.length, 3);
  eq("the rows are anchored on the prose and the two CALLS", rows.map((r) => r.entry.seq), [1, 2, 4]);
  check("no row is anchored on a result", rows.every((r) => r.entry.kind !== "tool_result"));
  eq("each call carries its own result", rows.slice(1).map((r) => r.result?.seq), [3, 5]);
  // The whole point, stated as the reader sees it: the tool's name appears once
  // per exchange, on the call.
  const named = rows.filter((r) => r.entry.title === "bash").length;
  eq("the tool is named once per exchange", named, 1);
}

// A call still running has no result to fold, and must not be made to look like
// one that returned nothing.
{
  const rows = mergeToolRows([entry(1, "tool", { title: "bash", meta: { call_id: "c1" } })]);
  eq("a running call is one row", rows.length, 1);
  check("with no result attached", rows[0].result === undefined);
}

// The orphan: a result whose call is not in the list — filtered off by kind, or
// begun before the stream's cursor. It stands alone and keeps its name, which is
// the one case where the name on a result is right.
{
  const rows = mergeToolRows([
    entry(7, "tool_result", { title: "bash", body: "out", meta: { call_id: "gone" } }),
  ]);
  eq("an orphan result still renders", rows.length, 1);
  eq("anchored on itself", rows[0].entry.seq, 7);
  check("with nothing folded into it", rows[0].result === undefined);
}

// Pairing happens over the list it is GIVEN, so it composes with the page's
// filters instead of fighting them: hiding calls must not silently hide results
// too, and hiding results must leave the call rows intact.
{
  const call = entry(2, "tool", { title: "bash", meta: { call_id: "c1" } });
  const result = entry(3, "tool_result", { title: "bash", meta: { call_id: "c1" } });
  eq("results filtered off leaves the call alone", mergeToolRows([call]).length, 1);
  const resultsOnly = mergeToolRows([result]);
  eq("calls filtered off leaves the result alone", resultsOnly.length, 1);
  eq("and it is still named", resultsOnly[0].entry.title, "bash");
}

// Order is the anchor's: a result joins a row, it never moves one.
{
  const rows = mergeToolRows([
    entry(1, "tool", { title: "a", meta: { call_id: "c1" } }),
    entry(2, "tool", { title: "b", meta: { call_id: "c2" } }),
    entry(3, "tool_result", { title: "b", meta: { call_id: "c2" } }),
    entry(4, "tool_result", { title: "a", meta: { call_id: "c1" } }),
  ]);
  eq("interleaved results do not reorder their calls", rows.map((r) => r.entry.seq), [1, 2]);
  eq("each still finds its own result", rows.map((r) => r.result?.seq), [4, 3]);
}

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

// -- prose folds to its first sentence (§1, #4578) --------------------------

// The rule is `stella_transcript::digest::first_sentence`'s: a boundary is a
// `.`, `!` or `?` followed by whitespace or the end, so a version number or a
// file name cannot end a sentence mid-token, and a wall of text with no
// boundary at all middle-elides rather than showing nothing.
console.log("prose fold");
{
  eq(
    "a folded block shows exactly its first sentence",
    firstSentence("Fix the bug. Then run the tests."),
    "Fix the bug.",
  );
  eq("a question is a sentence", firstSentence("Does it build? Yes."), "Does it build?");
  eq("so is an exclamation", firstSentence("Ship it! Now."), "Ship it!");
  eq(
    "a boundary at the very end of the text counts",
    firstSentence("One sentence only."),
    "One sentence only.",
  );
  eq(
    "a dot mid-token is not a boundary",
    firstSentence("v1.2 fixed fold.rs at last. More below."),
    "v1.2 fixed fold.rs at last.",
  );
  eq(
    "leading whitespace does not reach the summary",
    firstSentence("  Trimmed. Rest."),
    "Trimmed.",
  );

  // No sentence boundary anywhere: the fallback is a middle-elision, never an
  // empty summary — the prose cousin of the prompt's no-fold-to-nothing rule.
  const wall = "word ".repeat(60).trim();
  const head = firstSentence(wall);
  check("a boundary-less wall still shows something", head.length > 0);
  check("and is elided to the shared width", head.length <= 96, `got ${head.length}`);
  check("with the cut marked", head.includes(" … "));

  // `proseFold` is what the components render from: head + rest must never
  // lose bytes between them.
  {
    const { head, rest } = proseFold("Fix the bug. Then run the tests.");
    eq("the summary is the first sentence", head, "Fix the bug.");
    eq("and the rest is what expanding adds", rest, "Then run the tests.");
  }
  {
    const { rest } = proseFold(wall);
    eq("an elided summary is not a prefix, so the rest is the whole text", rest, wall);
  }
  {
    const { head, rest } = proseFold("Just one thought.");
    eq("a single-sentence block folds to itself", head, "Just one thought.");
    eq("with nothing left to disclose", rest, "");
  }

  eq("elide keeps short text whole", elide("short", 96), "short");
  eq("elide keeps the head and the tail", elide("abcdefghijklmnop", 10), "abcd … nop");
}

// -- the collapsed turn's digest (§1, #4578) --------------------------------

// A collapsed turn shows "prompt digest · answer digest · step count · rollup
// chips" — `stella_transcript::digest::turn_digest` over the wire's flat
// stream. An arena trial is one turn, so the digest summarises the whole
// entry list, and it is computed over the FULL list so a kind filter can
// never change what the collapsed turn claims.
console.log("turn digest");
{
  const entries = [
    entry(0, "prompt", { body: "Fix the flaky test.\nIt lives in fold.rs." }),
    entry(1, "stage", { title: "execute" }),
    entry(2, "reasoning", { body: "Thinking." }),
    entry(3, "tool", { title: "bash", meta: { call_id: "c1" } }),
    entry(4, "tool_result", { title: "bash", meta: { call_id: "c1" } }),
    entry(5, "usage", { meta: { tokens_in: 100, tokens_out: 20, cost_usd: 0.5 } }),
    entry(6, "text", { body: "Half way there." }),
    entry(7, "usage", { meta: { tokens_in: 50, tokens_out: 10, cost_usd: 0.25 } }),
    entry(8, "text", { body: "All fixed.\nDetails follow." }),
    entry(9, "complete", { title: "complete" }),
  ];
  const digest = turnDigest(entries, true);
  eq("the prompt digest is the prompt's first line", digest.promptLine, "Fix the flaky test.");
  eq("the answer digest is the LAST response's first line", digest.answerLine, "All fixed.");
  eq("steps are model calls, counted from usage", digest.steps, 2);
  eq("tokens in roll up", digest.tokensIn, 150);
  eq("tokens out roll up", digest.tokensOut, 30);
  eq("cost rolls up", digest.costUsd, 0.75);
  eq("the wall clock is the latest elapsed stamp", digest.durationSeconds, 9);

  // While the trial is still running its latest prose is not its answer —
  // the digest says nothing rather than promoting it.
  eq("an unfinished turn has no answer digest", turnDigest(entries, false).answerLine, "");

  const long = [
    entry(0, "prompt", { body: `${"p".repeat(100)}\nmore` }),
    entry(1, "text", { body: "a".repeat(100) }),
  ];
  const elided = turnDigest(long, true);
  check("a long prompt line is elided", elided.promptLine.length <= 64 && elided.promptLine.includes(" … "));
  check("so is a long answer line", elided.answerLine.length <= 64 && elided.answerLine.includes(" … "));

  const empty = turnDigest([], true);
  eq(
    "an empty trial digests to nothing, not to fabricated numbers",
    [empty.promptLine, empty.answerLine, empty.steps, empty.tokensIn, empty.tokensOut, empty.costUsd, empty.durationSeconds],
    ["", "", 0, 0, 0, 0, 0],
  );
}

// -------------------------------------------------------------------------

if (failures > 0) {
  console.error(`\n${failures} of ${checks} checks failed.`);
  process.exit(1);
}
console.log(`\n${checks} checks passed.`);
