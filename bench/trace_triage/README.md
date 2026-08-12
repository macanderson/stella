# `make triage-bench-traces` — bench run in, issue activity out

A repeatable pass over a finished ArenaBench run whose **input** is the run's
traces and whose **output** is GitHub issue activity: a new issue for a
genuinely new defect, a comment carrying fresh citable evidence onto an issue
that already describes the behaviour.

```sh
make triage-bench-traces RUN=w10p FETCH=1          # dry run: prints the plan
make triage-bench-traces RUN=w10p MIRROR=~/w10p    # a mirror you already have
make triage-bench-traces RUN=w10p MIRROR=~/w10p ARGS=--apply   # writes
```

The default is a **dry run**. It prints, per finding, the de-duplication
decision, what that decision was made on, and the exact title and body it would
post — so the plan is readable before anything reaches the tracker.

## Why it exists

Every bench run surfaces defects, and until now they were found by a human
reading traces by hand and then remembering to file something. That is how
findings get lost, and how one defect gets filed three times in three different
sets of words. This repository's own backlog carries the proof: #2929 and #2872
describe a single defect — an oracle that fails on both trees and routes to
`Revise` — with almost no shared vocabulary, found on different runs.

So the hard requirement is: **the output is never a duplicate issue**, including
against closed ones. A closed issue whose defect recurs gets a comment saying
so, which is more valuable than either a second ticket or silence.

## The fingerprint

A defect is identified by a triple, hashed — never by its prose.

```
fp = sha256("<detector>\n<site>\n<variant>")[:12]
```

* **detector** — the event-level predicate that fired. Two findings from one
  predicate are the same kind of defect by construction.
* **site** — the repo-relative code site the detector implicates, declared on
  the detector and never derived from the run.
* **variant** — a *normalized* discriminator from the occurrence, so one
  detector can still separate genuinely different defects. Normalization erases
  everything that varies between two runs of one defect: quoted spans, absolute
  paths, the `/tmp/stella_candidate_<n>_<n>` workspace root, digits, whitespace.

None of the three holds a run id, a trial id, a task name, a timestamp, a cost,
a model name or a line number — which is why a recurrence next week hashes to
this week's value, and why two different reasons under one detector do not
collide.

**A variant discriminates defects, not payloads.** `witness_unavailable`
reasons are a small closed set the engine authors, so the reason *is* the
discriminator and each one is its own defect. A tool error message is command
output — pure run data — so keying on it shattered one defect into 23 tickets
the first time this was pointed at a real run; that detector's variant is a
constant, and its predicate carries the whole identity.

## The de-duplication gates

Three, in order, each a fallback for the previous one failing:

1. **The ledger** — [`fingerprints.json`](fingerprints.json), tracked in git.
   A bound fingerprint is that issue. No search, no similarity, no judgement.
2. **The marker** — every issue this tool opens carries
   `<!-- stella-trace-fingerprint: fp_… -->` in its body, so a lost or reset
   ledger is rebuilt from GitHub itself.
3. **A search across open AND closed**, whose hits are printed as *candidates*
   for a human to confirm. A candidate is never auto-bound: prose similarity is
   the instrument that filed the duplicate above, and it does not get a vote.

A finding that survives all three is new, and only then may an issue be opened.
A search that **errored** is not a search that found nothing: it fails closed
and blocks the new issue rather than risking the duplicate.

## Editing the ledger

`fingerprints.json` is meant to be corrected by hand. An entry whose `source` is
anything but `auto` is a human decision and is never overwritten by a later run.
Delete an entry to make the tool re-derive it from a search — the right move
when a mapping was *wrong* rather than merely stale.

`runs`, `tasks` and `peak_*` are the incidence already on the record. They are
what lets a comment say what an occurrence **adds** — a new run, a task the
issue never named, a higher rate — instead of restating the issue. When an
occurrence adds nothing, nothing is posted; the run is still recorded so the
next comparison is against the truth.

## Detectors

| Code | Fires on |
|---|---|
| `void-model-call` | `reasoning` deltas with zero `step_usage` — no model call ever completed |
| `agent-timeout` | harbor's own `AgentTimeoutError` classification |
| `witness-unavailable` | `proof` `step.kind == "witness_unavailable"`, split per reason |
| `oracle-flip-ungraded` | an oracle flip (baseline fail, candidate pass) on a trial graded 0 |
| `tool-error-envelope` | an error envelope wrapping substantial output before `[exit code: N]` |
| `repeated-identical-tool-call` | adjacent identical `(tool, args)` with byte-identical output |
| `role-model-census-mismatch` | `(role, model)` off `step_usage` against the run's declared seat |
| `no-post-verdict-file-change` | no `verdict` at all, or a verdict with no `file_change` after it |
| `cache-collapse` | pooled prompt cache hit below the floor measured across nine arms |
| `repeated-file-read` | one path named by five or more read-shaped calls in one trial |

`--list-detectors` prints the live registry.

The last two read their thresholds from [`bands.py`](bands.py), which records
the nine-arm survey they were measured off — and which is also where a metric
that *cannot* separate a healthy arm from a broken one is marked as such, so it
is reported and never concluded from.

## The postmortem

[`postmortem.py`](postmortem.py) answers the other question the same evidence
supports: not "file an issue about this shape" but "**say, on screen, where
this run went wrong**", so a defect is caught even when nobody reads the traces
by hand.

```sh
make postmortem RUN=arenabench-cloud/s5b2        # writes it beside the match
python3 bench/trace_triage/postmortem.py <dir> --stdout
```

It calls the same `detectors.run_all` over the same `run_trace.Run`, so the two
surfaces cannot disagree about whether a run is healthy — the report is a
second *rendering*, never a second detector set. It adds the band table, the
cohort split that separates "the run is broken" from "the agent did badly"
(a trial that completed zero model calls measured nothing, and its zero is not
a loss), and it writes `postmortem.md` / `postmortem.json` into the match
`arenabench assemble` folded the run into.

A healthy run gets one clean paragraph. That is the load-bearing behaviour, not
a nicety: a detector that always fires is noise, and noise is how a real
finding gets ignored.

### Adding one

One decorated function in [`detectors.py`](detectors.py), and nothing else:

```python
@detector(
    code="my-shape",
    title="what a new issue would be called",
    site="crates/stella-pipeline/src/verify.rs",  # or "" if none implicated
    search_terms=("phrase a reviewer would search",),
)
def _my_shape(run: Run) -> list[Finding]: ...
```

The decorator registers it; `run_all` picks it up; the plan, the rendering and
the de-duplication need no change. `tests/test_detectors.py::test_every_detector_declares_its_contract`
holds you to the contract.

Three rules a detector must obey, each of which this repository has paid for:

* **`proof` discriminates on `step.kind`**, not the top-level `type`. Use
  `trial.proofs(kind=…)`.
* **Carry a reason string in full.** They are truncated in every other surface
  and the truncated half is usually the point.
* **`file_change` is observability, never evidence.** `bash` names no path, so a
  heredoc, `patch`, `make` or `>` mutates the tree silently. A detector keyed on
  its absence reports an absent *event* and never an unchanged tree — and says
  so in its own output, so the sentence reaches the issue.

A detector that fires once must say "single occurrence" rather than quote
`1/20 = 5%`. The percentage is arithmetically true and epistemically a lie.

## Safety

* Dry run by default; `--apply` is the only thing that writes.
* `--max-new-issues` (default 5) caps one invocation. Anything past it is
  printed as `SUPPRESSED` **with its full evidence** and logged — never
  silently dropped.
* `--offline` skips every GitHub read and refuses to write, because a tool that
  cannot de-duplicate must not file.

## Tests

```sh
uv run --with pytest --no-project pytest -q bench/trace_triage/tests
```

Standard library only, no network. Fixtures are transcribed from real artifacts
under `runs/w10p/`, because a fixture that disagrees with the wire tests the
fixture.
