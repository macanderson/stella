# stella-diff

A pure, line-oriented **unified diff**: trim the common prefix and suffix, run
an exact longest-common-subsequence over what is left, and group the edit
script into `@@ -a,b +c,d @@` hunks with context lines — git's exact shape, so
an editor's diff mode and a human's muscle memory both parse it.

Four modules, each a different verb over that one shape:

| Module | What it does |
|---|---|
| root (`unified_diff`) | two documents → hunks |
| [`parse`] | unified-diff **text** → hunks, for the diffs measured by shelling to git and carried on `AgentEvent::FileChange` |
| [`view`] | how much of a diff a surface draws, and which part |
| [`json`] | the wire shape a diff-serving payload carries (opt-in, off by default) |

## Boundary

Zero dependencies by default, zero I/O, no types from any other stella crate:
two `&str` documents in, a [`Diff`] out. That emptiness is the point. The differ began
life inside `stella-cli` (`stella inspect --diff`), and the Observatory —
which deliberately links almost no workspace crate, because an observer must
not pull in the machinery it observes — would have had to take a fourth
acknowledged copy to render "what changed between two model calls" (#1511).
A leaf crate is the `stella-home` precedent (#1139): shared by linking,
without costing any caller its isolation.

That shape is the composability Stella is building toward everywhere else — one
turn loop, capabilities reached through ports, and everything wrapped around it a
plugin (`doc:turn-loop-wrappers`). A dependency-free leaf is the cheapest version
of the same idea: the differ is depend-able from a surface, a wrapper, or an
observer without any of them learning about the others. Which is also why the
answer to "can this take one small dependency?" stays no.

The one exception proves the rule rather than bending it. [`json`] sits behind
an **off-by-default `json` feature**, so a caller that wants only the differ
still links nothing; a caller that opts in was already linking `serde_json`.
It exists because the alternative was three crates hand-writing the same hunk
serializer for the same dashboard renderer — the acknowledged copy this crate
was extracted to end.

## Semantics worth knowing

- **Line semantics follow `str::lines()`**: `""` is zero lines and a trailing
  newline adds none, so a trailing-newline-only difference is not a change.
- **Removals precede additions** at a change point, as in git.
- **`Diff::minimal`** reports whether the script is the exact minimal one.
  Inputs whose DP table would exceed `LCS_AREA_CAP` cells degrade to a
  correct-but-blunt replace-everything script, flagged `minimal: false` —
  surfaces are expected to say so rather than present it as precise.
- **Empty hunk list = byte-identical** — the honest "no change" answer, not
  an error.
- **A parsed diff round-trips**: what `unified_diff` renders, [`parse::hunks`]
  reads back to the same hunks. There is a test for it, because the two halves
  disagreeing about their own format is the failure nobody would notice.
- **`parse` recomputes hunk counts from the body** rather than trusting the
  `@@` header, and treats `+++ `/`--- ` as file headers **only before the
  first `@@` of a file**. A removed line whose own text begins `-- ` is
  textually a header once the diff prefixes it, and a prefix-only parser drops
  it silently.

## How much of a diff a surface shows — [`view`]

A diff is already only the changed lines and their context; no surface may
fall back to printing a file. But that is not a bound: a created
two-thousand-line file is two thousand changed lines. So every surface caps
the rendering, and [`view`] is the **one** answer to how — the deck, the plain
`stella run` scrollback, the Observatory and an exported dashboard are four
views of one run, and a reader who sees three different amounts of one edit
cannot tell which is the edit.

The policy fills from **both ends** and elides the middle, which is the shape
a reviewer already knows from a collapsed hunk on a pull request. Filling only
from the front — what shipped before — is silent about the shape of what it
hides: a long edit rendered as its first twenty lines reads as one that starts
here and trails off, when the reader's actual question is usually answered at
the other end.

Two levels, because a diff has two natural units: **whole hunks** taken
alternately front and back while they fit (a cut inside a hunk lands routinely
between a `-` line and the `+` that replaces it, leaving a change that reads as
a pure deletion), and **a line window at each end** when no whole hunk fits at
all — one enormous hunk, which is exactly the created-file shape.

Either way there is **at most one elision**, so a caller has exactly one place
to draw the marker and one number to put in it.

**The policy has a second implementation, and it is checked.** The arena
transcript — in the [arenabench repo](https://github.com/macanderson/arenabench)
since the ejection (#2380) — is a Next.js client with no way to call into this
workspace, so it reimplements `plan` in TypeScript (`ui/lib/diff-view.ts`
there). The two share a file rather than a test runner:
`tests/fixtures/view-plan-matrix.txt` is generated from the Rust and pinned by
`cargo test -p stella-diff --test view_plan_matrix` (re-bless with `BLESS=1`,
then read the diff); that repo vendors the matrix under `ui/golden/` and its
CI asserts the TypeScript reproduces it. A re-blessed golden here must be
synced there by copying the regenerated file.

That check found a bug the first time it ran, in the Rust: `Plan::fold_before`
drew the elision marker *after* the only surviving hunk whenever the budget
kept a tail and no head. Thirty-eight unit tests passed over it, because every
hand-written case happened to keep a head. `view::elide` expresses that
for structured hunks by returning the split pieces as **separate hunks with
recomputed `@@` headers**, so the result stays a valid diff: a renderer walking
line numbers from each header cannot be misled by a gap it does not know about.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.
