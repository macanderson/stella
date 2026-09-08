---
id: adr/0040-host-does-not-pick-the-tests
title: "ADR 0040: The host does not pick the tests"
status: implemented
---

# ADR 0040: The host does not pick the tests

- Status: accepted
- Date: 2026-09-07
- Decides: `#1292`
- Not part of the Phase 0 series.

## Context

Running a whole suite to prove one change is slow. Running only the tests a
change touches is cheap. `#1292` asked for that saving. It also asked for the
result to count as evidence.

The selector it names is gone. `run_tests` is a retired name in
`crates/stella-tool-facts/src/catalog.rs`. `#3236` deleted
`CodeGraph::importer_paths` when the tool purge took its only caller.
`crates/stella-graph/src/graph.rs` says so, in
`the_importer_relation_lists_files_whose_imports_resolve_here`. The raw
relation is still indexed. Nothing picks tests with it.

Host-run verification is gone as well (`#3865`). A check runs inside an
installed plugin's `[oracle]`. Stella grades that evidence against the plugin's
own rule and never runs it again. So the open question is not whether to narrow
a suite. It is who may pick the command, and what a pass may then claim.

Timing is the hard part. `ROADMAP.md` §3 has said so since before the pipeline
was deleted. A flip is one command watched twice: red, then green. The red half
is watched before the turn. `grant_shared_tree` in
`crates/stella-cli/src/wrapper_candidate.rs` runs the granted command once and
stamps `TestPlan::baseline` with what it saw. An impacted set comes from the
diff. At that moment there is no diff. Pick before the turn and there is nothing
to pick on. Pick after it and the two halves are different commands. That pair
is not a flip.

## Decision

**The host does not pick the tests, and gains no capability that does.** No
`HostCall` returns an impacted set. `run_test` runs the command the grant
carried.

**A plugin may narrow inside its own oracle.** On the shared-tree path the
grant carries a real root. A plugin holding it can work out any command it
likes and run it in its own process. What comes back is
`EvidenceProvenance::PluginReported`. That is the grade `#3511` already set for
everything a plugin watches, and it is the grade a self-picked suite earns.

**A narrowed run cannot borrow the wide run's red half.** The baseline belongs
to the granted `program` and `args`. A plugin that reports
`FlipObservation::Achieved` about some other command is claiming a flip whose
red nobody saw. The provenance says whose claim that is. The host does not
raise it.

## Consequences

Stella ships no impacted-test selector, and will not bring one back. A slow
suite still costs one baseline run of the `--test-command` the user named. That
run buys the red half. Without it, a green suite afterwards proves nothing.

A plugin cannot narrow on a fan-out candidate. `CandidateHandle` is opaque by
design. `crates/stella-runtime/src/wrapper/test_run.rs` puts it plainly: a
plugin says which workspace, never where it is. With no path there is nothing to
run a command against. Narrowing there would need the host to run the plugin's
command, which is what this record refuses.

`ROADMAP.md` §3 described the deleted selector in the present tense. It also
named the isolated-candidate path as the way in. Both are rewritten to what the
tree holds, and point here.

## Alternatives considered

**The host works out the impacted set and narrows the command.** Then the host
authors the proof it grades. `crates/stella-cli/src/wrapper_candidate.rs`
already refuses the smaller version: deriving a witness file from a runner's
convention. `BeforeTurnResponse::witness` exists because of that refusal.
Picking which tests decide "done" is the larger claim. The host cannot be the
one to make it.

**A `HostCall::ImpactedTests` a plugin asks for.** It reads as a fact. It is a
guess. The code graph is an import index over tree-sitter parses. It cannot see
reflection, dynamic imports, fixtures, data files, or a test that drives a built
binary. A call with that name hands back a list the plugin cannot check and has
every reason to trust. The guess is still the host's. `recall` is the door to
the same index that says what it is. It fans out over `codegraph.db`, and the
code-graph provider renders importer frames among what comes back — scored,
budget-packed, labelled as retrieval. A plugin asks it for material rather than
for a verdict. It is a loose door: `RecallArgs` carries a goal and a limit, so a
plugin cannot anchor a query on the exact file it cares about. A heuristic
should be that loose. The tight door is the one to refuse.

**A `program` and `args` pair on `RunTestArgs`.** Someone will propose this
again. It looks like a small additive field. It turns a capability a human
consented to as `calls = ["run_test"]` into an ungated shell. That shell runs at
the host's authority, in the granted root. This workspace has one shell, `bash`,
and the user approves what it runs.

Adding a field to the struct does redden something: `wire_corpus.rs` builds its
`run_test` example as a Rust struct literal, so a new field fails to compile
there. The *refusal* was held by nothing. `RunTestArgs` derives no JSON schema,
so `docs/wire/wrapper.wire.json` carries an example message and no shape the
`wire-schema` gate can check. Delete its `deny_unknown_fields` and the host
quietly drops a narrowed command a plugin sends, rather than refusing it. Before
`a_run_test_ask_cannot_carry_its_own_invocation`, no test in the tree failed on
that — measured, with the attribute removed.

**Run the narrowed set at both moments in an isolated candidate.** Sound, and
`ROADMAP.md` §3 named it. The session tree stays clean through the turn, so the
same diff-derived command runs red on the base and green on the candidate. It
needs the host to derive a command, run it on a tree it picked, and credit the
pair. Every clause there is one this record refuses. A plugin can build the same
thing for itself once it can reach a candidate's tree. What it reports stays its
own claim.
