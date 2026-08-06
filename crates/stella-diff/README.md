# stella-diff

A pure, line-oriented **unified diff**: trim the common prefix and suffix, run
an exact longest-common-subsequence over what is left, and group the edit
script into `@@ -a,b +c,d @@` hunks with context lines — git's exact shape, so
an editor's diff mode and a human's muscle memory both parse it.

## Boundary

Zero dependencies, zero I/O, no types from any other stella crate: two `&str`
documents in, a [`Diff`] out. That emptiness is the point. The differ began
life inside `stella-cli` (`stella inspect --diff`), and the Observatory —
which deliberately links almost no workspace crate, because an observer must
not pull in the machinery it observes — would have had to take a fourth
acknowledged copy to render "what changed between two model calls" (#1511).
A leaf crate is the `stella-home` precedent (#1139): shared by linking,
without costing any caller its isolation.

## Semantics worth knowing

- **Line semantics follow `str::lines()`**: `""` is zero lines and a trailing
  newline adds none, so a trailing-newline-only difference is not a change.
- **Removals precede additions** at a change point, as in git.
- **`Diff::minimal`** reports whether the script is the exact minimal one.
  Inputs whose DP table would exceed `LCS_AREA_CAP` cells degrade to a
  correct-but-blunt replace-everything script, flagged `minimal: false` —
  surfaces are expected to say so rather than present it as precise. (The
  coarse companion in `stella-tools`, `file_touch::changed_region_diff`, is
  honest for file edits; it cannot express "one paragraph inserted into four
  hundred stable lines", which is the case this crate exists for.)
- **Empty hunk list = byte-identical** — the honest "no change" answer, not
  an error.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.
