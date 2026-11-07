#!/usr/bin/env python3
"""Guard: no content-free prose or insider vocabulary in this repository's text.

Three down-only ratchets hold this repository's prose: a count of banned
constructions per file, a reading grade per file, and a mean module-header
length per crate. This file is the command line and the verdict. The
`scripts/prose/` package beside it holds what each ratchet reads, and each
module's docstring carries its own part of the rule:

- `prose.scan` -- what counts as prose, and the table of banned constructions
- `prose.count` -- the count ratchet's baseline, and a count read from git
- `prose.grade` -- the reading grade, and its baseline
- `prose.density` -- mean module-header length, and its baseline
- `prose.git_pairing` -- which paths are read, which tree a change is judged
  against, and how a moved or split file carries its entries

Adding a pattern is the one case a count legitimately goes up, and
`--adopt=<name>` is the only door: it records that pattern's pre-existing
hits and refuses to touch any other pattern's numbers, or to run twice for
one pattern.

Each ratchet is legitimate for the one reason a ratchet ever is: the rule
predates the guard. A baseline records debt that already existed; it grants
no new permission.

Each baseline is a shared cell in the sense AGENTS.md describes for
`Cargo.lock`: two branches each landing one ordinary header are green alone
and compose into a red `main`. Two doors close that, mirroring
`check-file-size.sh`'s own split:

- The plain check judges a count, a grade and a unit's mean against
  `max(its baseline, the same value in the base tree)`. Inherited drift is
  named on the way past and does not fail a branch that did not cause it, so
  landing one ordinary header, or leaving somebody else's hard sentence
  alone, cannot redden `main`. `--absolute` opts out of the base comparison
  for the one caller that must not get this mercy: a post-merge canary is
  exactly asking whether drift already reached `main`, and the base-relative
  reading would forgive the thing it exists to catch. A file the base tree
  does not have inherits nothing, so a first-time offender still fails.
- `--update` alone leaves every count, grade and unit ceiling where it
  stands, except for an entry a file move or a split re-based: a moved file's
  count and grade follow it to its new path, a split's new file takes the
  ceiling of the file it came out of, and a unit a move took a header out of
  or into is re-based against the same files' lengths, because a move changes
  a number with nobody having written a word. Only `--update --retighten`
  lowers every ceiling to its current value, as a deliberate,
  separately-landed pass. Retightening on every `--update` run is what put
  every unit at exactly its ceiling with zero headroom, and it also let a
  branch that never opened `AGENTS.md` write that file a lower grade ceiling
  and fail the merge commit on it.

Usage:

    ./scripts/check-prose.py [--update] [--adopt=NAME] [--report] [ROOT]

    --update      carry the count, grade and header-length entries a file
                  move re-based, and leave every other one where it stands
                  (`make prose-update`)
    --retighten   with --update, also lower every count, grade and
                  header-length ceiling to its current value -- a deliberate,
                  separate pass (`make prose-retighten`)
    --adopt=NAME  record the pre-existing debt of a pattern added to PATTERNS
                  after the baseline was written, and nothing else. Once per
                  pattern: a pattern already in the baseline is refused
                  (`make prose-adopt PATTERN=NAME`)
    --bootstrap   create all three baselines from the current tree; one-time,
                  and refuses to run when the count baseline already exists
    --bootstrap-density
                  create the density baseline alone; the same one-time door,
                  for the tree that already had a count baseline when the
                  density ratchet arrived
    --bootstrap-grade
                  create the reading-grade baseline alone; the same one-time
                  door, for the tree that already had the other baselines
                  when the grade ratchet arrived
    --report      print every offending line, grouped by file, then each
                  unit's mean header length; changes nothing
    --absolute    judge every count, grade and unit density against its
                  baseline alone, ignoring the base tree. For the post-merge
                  canary, which must catch drift a base-relative check would
                  forgive.
"""

from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

# The package sits beside this file. Python puts a script's directory on
# `sys.path` itself, except under `-P` or PYTHONSAFEPATH, so the entry point
# adds it rather than depend on how it was started.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from prose.count import (
    BASELINE,
    counts,
    counts_at_commit,
    read_baseline,
    write_baseline,
)
from prose.density import (
    DENSITY_BASELINE,
    NEW_UNIT_MEAN,
    base_tracked_paths,
    density,
    density_at_commit,
    read_density_baseline,
    units_a_move_touched,
    write_density_baseline,
)
from prose.git_pairing import (
    renamed_paths,
    resolve_base_commit,
    split_sources,
    tracked_files,
)
from prose.grade import (
    GRADE_BASELINE,
    NEW_FILE_GRADE,
    grade_at_commit,
    grades,
    read_grade_baseline,
    write_grade_baseline,
)
from prose.scan import PATTERNS


def main() -> int:
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = [a for a in sys.argv[1:] if a.startswith("--")]
    flagset = {a.split("=", 1)[0] for a in flags}
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    tracked = tracked_files(root)
    per_pair, detail = counts(root, tracked)
    files = {path for path, _ in per_pair}
    baseline_path = root / BASELINE
    density_path = root / DENSITY_BASELINE
    per_unit = density(root, tracked)
    grade_path = root / GRADE_BASELINE
    per_grade = grades(root, tracked)

    if "--report" in flagset:
        total = sum(per_pair.values())
        by_pattern: Counter[str] = Counter()
        for hits in detail.values():
            for _, name, _, _ in hits:
                by_pattern[name] += 1
        for path in sorted(detail):
            print(f"\n{path}")
            for lineno, name, text, remedy in detail[path]:
                print(f"  {lineno:>5}  [{name}] {text!r} -- {remedy}")
        print(f"\n{total} construction(s) in {len(files)} file(s)")
        for name, n in by_pattern.most_common():
            print(f"  {n:>5}  {name}")
        allowed = read_density_baseline(density_path) if density_path.exists() else {}
        print("\nmean module-header length, worst first")
        for unit, mean in sorted(per_unit.items(), key=lambda kv: -kv[1]):
            ceiling = allowed.get(unit, NEW_UNIT_MEAN)
            mark = "  OVER" if mean > ceiling else ""
            print(f"  {mean / 100:>7.2f}  (ceiling {ceiling / 100:.2f})  {unit}{mark}")
        graded = read_grade_baseline(grade_path) if grade_path.exists() else {}
        print("\nreading grade, worst first (top 20)")
        by_grade = sorted(per_grade.items(), key=lambda kv: -kv[1][0])[:20]
        for path, (grade, worst) in by_grade:
            ceiling = graded.get(path, NEW_FILE_GRADE)
            mark = "  OVER" if grade > ceiling else ""
            print(f"  {grade / 100:>7.2f}  (ceiling {ceiling / 100:.2f})  {path}{mark}")
            for sentence_grade, sentence in worst[:1]:
                print(f"           worst: [{sentence_grade:.1f}] {sentence[:110]}")
        return 0

    if "--bootstrap" in flagset:
        if baseline_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{BASELINE} already exists. Use --adopt=<pattern> to record "
                "the debt of a newly added pattern, or --update, which only "
                "ever lowers a count.",
                file=sys.stderr,
            )
            return 1
        write_baseline(baseline_path, per_pair)
        write_density_baseline(density_path, per_unit)
        write_grade_baseline(
            grade_path, {path: grade for path, (grade, _) in per_grade.items()}
        )
        print(
            f"check-prose: wrote {BASELINE} with "
            f"{sum(per_pair.values())} construction(s) in {len(files)} file(s), "
            f"{DENSITY_BASELINE} with {len(per_unit)} unit(s), "
            f"and {GRADE_BASELINE}."
        )
        return 0

    # The density ratchet arrived after the count ratchet, so `--bootstrap`
    # (which refuses once scripts/prose-baseline.txt exists) could not
    # introduce it. This is that one-time door, and it closes behind itself
    # for the reason `--bootstrap` does: a regenerated baseline records
    # today's tree as the ceiling.
    if "--bootstrap-density" in flagset:
        if density_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{DENSITY_BASELINE} already exists. Use --update, which only "
                "ever lowers a mean.",
                file=sys.stderr,
            )
            return 1
        write_density_baseline(density_path, per_unit)
        print(
            f"check-prose: wrote {DENSITY_BASELINE} with "
            f"{len(per_unit)} unit(s)."
        )
        return 0

    if "--bootstrap-grade" in flagset:
        if grade_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{GRADE_BASELINE} already exists. Use --update, which only "
                "ever lowers a grade.",
                file=sys.stderr,
            )
            return 1
        write_grade_baseline(
            grade_path, {path: grade for path, (grade, _) in per_grade.items()}
        )
        over_count = sum(
            1 for grade, _ in per_grade.values() if grade > NEW_FILE_GRADE
        )
        print(
            f"check-prose: wrote {GRADE_BASELINE} with {over_count} file(s) "
            f"above grade {NEW_FILE_GRADE / 100:.0f}. Every one is debt; "
            "take it down."
        )
        return 0

    if not density_path.exists():
        print(
            f"check-prose: {DENSITY_BASELINE} is missing. It records what each "
            "crate's module headers already average, and without it every unit "
            f"is held to {NEW_UNIT_MEAN / 100:.2f}. Restore it from git rather "
            "than regenerating it: a fresh one records today's tree as the "
            "ceiling, which is the grandfathering this ratchet exists to "
            "refuse.",
            file=sys.stderr,
        )
        return 2

    if not grade_path.exists():
        print(
            f"check-prose: {GRADE_BASELINE} is missing. It records each "
            "file's reading grade, and without it every file is held to "
            f"{NEW_FILE_GRADE / 100:.2f}. Restore it from git rather than "
            "regenerating it: a fresh one records today's tree as the "
            "ceiling.",
            file=sys.stderr,
        )
        return 2

    baseline = read_baseline(baseline_path)
    density_baseline = read_density_baseline(density_path)
    grade_baseline = read_grade_baseline(grade_path)

    # `--adopt=<pattern>[,<pattern>]` records the debt of a pattern added to
    # PATTERNS after the baseline was written, and touches no other pattern's
    # numbers. This is the one legitimate way a count in this file goes up, and
    # it is legitimate for the reason --bootstrap was: the prose predates the
    # check, so the entries record debt that already existed rather than
    # granting permission to write more. A pattern that already appears in the
    # baseline has been adopted, and is refused.
    adopt = next((a for a in flags if a.startswith("--adopt")), None)
    if adopt is not None:
        _, _, raw = adopt.partition("=")
        wanted = [name.strip() for name in raw.split(",") if name.strip()]
        known = {name for name, _, _ in PATTERNS}
        if not wanted:
            print(
                "check-prose: --adopt needs a pattern name: "
                f"--adopt={sorted(known)[0]}",
                file=sys.stderr,
            )
            return 2
        unknown = [name for name in wanted if name not in known]
        if unknown:
            print(
                f"check-prose: no such pattern(s): {', '.join(unknown)}. "
                f"Known: {', '.join(sorted(known))}.",
                file=sys.stderr,
            )
            return 2
        already = [
            name for name in wanted if any(p == name for _, p in baseline)
        ]
        if already:
            print(
                "check-prose: refusing to adopt -- these pattern(s) already "
                f"have baseline entries: {', '.join(already)}. Adoption is "
                "once per pattern; use --update, which only lowers a count.",
                file=sys.stderr,
            )
            return 1
        merged = dict(baseline)
        added = 0
        for (path, pattern), n in per_pair.items():
            if pattern in wanted:
                merged[(path, pattern)] = n
                added += n
        write_baseline(baseline_path, merged)
        print(
            f"check-prose: adopted {', '.join(wanted)} -- "
            f"{added} pre-existing construction(s) recorded. "
            "Every one of them is debt; take it down."
        )
        return 0

    if "--update" in flagset:
        # A moved file's debt follows it, so the same sentences in a new home
        # are not read as newly written. Applied before anything else looks at
        # `baseline`, so the carried entry goes through the identical `min`
        # below and a move can still only lower a count.
        moved = renamed_paths(root)
        if moved:
            baseline = {
                (moved.get(path, path), pattern): n
                for (path, pattern), n in baseline.items()
            }
            carried = sorted(
                (old, new)
                for old, new in moved.items()
                if any(path == new for path, _ in baseline)
            )
            for old, new in carried:
                print(f"check-prose: carried {old} -> {new}")
        # The split half of that carry, and the reason the module header gives:
        # forgiving it at check time alone kept the entry out of the baseline,
        # and `--absolute` then charged the carried prose to the new-file
        # ceiling (#6408). A rename remaps the key because the old path is
        # gone; a split looks the source up, because the source is still there.
        split_from = split_sources(root, resolve_base_commit(root, absolute=False))
        for new, old in sorted(split_from.items()):
            print(f"check-prose: {new} carries the ceiling of {old} (split)")
        # A pair absent from the baseline is held to zero, so `.get(pair, 0)`
        # is what makes --update refuse to grandfather a first-time offender.
        # A split falls back to its source's allowance for that pattern, capped
        # by the same `min`, so it can still only lower a count.
        retighten = "--retighten" in flagset
        moved_in = set(moved.values())
        carried_in = moved_in | set(split_from)

        def pair_ceiling(pair: tuple[str, str]) -> int:
            if pair in baseline:
                return baseline[pair]
            source = split_from.get(pair[0])
            return baseline.get((source, pair[1]), 0) if source else 0

        def grade_ceiling(path: str) -> int:
            if path in grade_baseline:
                return grade_baseline[path]
            source = split_from.get(path)
            if source is not None and source in grade_baseline:
                return grade_baseline[source]
            return NEW_FILE_GRADE
        if retighten:
            merged = {p: min(n, pair_ceiling(p)) for p, n in per_pair.items()}
        else:
            # Every entry stays where it stands, apart from one a move carried.
            # A global reclaim here lowers the ceiling of files the branch never
            # opened, measured on whatever that checkout happens to hold, and the
            # next merge with `main` then fails the branch on a number the branch
            # itself wrote (`#6274`). `--retighten` is the deliberate pass.
            merged = dict(baseline)
            for pair, n in per_pair.items():
                if pair[0] in carried_in:
                    merged[pair] = min(n, pair_ceiling(pair))
        raised = {
            p: (pair_ceiling(p), n)
            for p, n in per_pair.items()
            if n > pair_ceiling(p)
        }
        if raised:
            print(
                "check-prose: refusing to update -- these files gained prose:",
                file=sys.stderr,
            )
            for (path, pattern), (was, now) in sorted(raised.items()):
                print(f"  {path} [{pattern}]: {was} -> {now}", file=sys.stderr)
            print(
                "\nDelete the constructions instead. "
                "`./scripts/check-prose.py --report` names every line.\n"
                "If one of these files was MOVED rather than written, stage "
                "the move (`git mv`, or `git add` after `mv`) and re-run: an "
                "unstaged move reads as a delete plus a new file, so its "
                "entry cannot be carried.",
                file=sys.stderr,
            )
            return 1
        # A unit absent from the density baseline is held to NEW_UNIT_MEAN, so
        # `.get(unit, NEW_UNIT_MEAN)` is what stops a new crate grandfathering
        # its own headers the first time anyone runs --update. A unit a move
        # touched is judged against the same files' lengths at HEAD instead --
        # see `base_tracked_paths` for why a set change is not a prose change.
        # Density takes a move and not a split, by arithmetic rather than
        # omission: the mean is header lines over file count, so a split that
        # moves header text lowers it. It rises only when a split ADDS lines,
        # and a header written during a split is new prose.
        rebased = {
            unit: density_at_commit(
                root,
                "HEAD",
                unit,
                base_tracked_paths(root, "HEAD", unit, tracked, moved),
            )
            for unit in units_a_move_touched(moved)
            if unit in per_unit
        }

        def unit_ceiling(unit: str) -> int:
            return max(density_baseline.get(unit, NEW_UNIT_MEAN), rebased.get(unit, 0))

        loosened = {
            unit: (unit_ceiling(unit), mean)
            for unit, mean in per_unit.items()
            if mean > unit_ceiling(unit)
        }
        if loosened:
            print(
                "check-prose: refusing to update -- these units' module "
                "headers grew:",
                file=sys.stderr,
            )
            for unit, (was, now) in sorted(loosened.items()):
                print(
                    f"  {unit}: {was / 100:.2f} -> {now / 100:.2f} mean lines",
                    file=sys.stderr,
                )
            print(
                "\nCut the headers instead. "
                "`./scripts/check-prose.py --report` lists every unit's mean.",
                file=sys.stderr,
            )
            return 1
        if moved:
            grade_baseline = {
                moved.get(path, path): n for path, n in grade_baseline.items()
            }
        grade_raised = {
            path: (grade_ceiling(path), grade)
            for path, (grade, _) in per_grade.items()
            if grade > grade_ceiling(path)
        }
        if grade_raised:
            print(
                "check-prose: refusing to update -- these files' reading "
                "grade rose:",
                file=sys.stderr,
            )
            for path, (was, now) in sorted(grade_raised.items()):
                print(
                    f"  {path}: {was / 100:.2f} -> {now / 100:.2f}",
                    file=sys.stderr,
                )
            print(
                "\nRewrite the sentences instead. "
                "`./scripts/check-prose.py --report` names the worst ones.",
                file=sys.stderr,
            )
            return 1
        # The grade ratchet takes the same split, for the same reason: #6266's
        # branch never contained AGENTS.md and still wrote that file a lower
        # ceiling, off a checkout cut before two AGENTS.md edits landed. CI then
        # failed the merge commit on three sentences its author never opened.
        if retighten:
            merged_grades = {
                path: min(grade, grade_ceiling(path))
                for path, (grade, _) in per_grade.items()
            }
        else:
            merged_grades = dict(grade_baseline)
            for path, (grade, _) in per_grade.items():
                if path in carried_in:
                    merged_grades[path] = min(grade, grade_ceiling(path))
        write_grade_baseline(grade_path, merged_grades)
        if retighten:
            # Pairs that reached zero drop out entirely; the ratchet retightens.
            for pair in baseline:
                if pair not in per_pair:
                    merged.pop(pair, None)
        write_baseline(baseline_path, merged)
        # Retightening every unit to its current mean on every `--update` run
        # is what left each crate sitting at exactly its ceiling with zero
        # headroom: the reclaim is unconditional and global, so clearing one
        # crate's drift silently removes every other crate's slack too. Split
        # the same way `check-file-size.sh --update`/`--retighten` are:
        # `--update` alone touches only the units a move re-based, and
        # `--retighten` is the deliberate, separately-landed pass that reclaims
        # slack across every unit at once.
        if retighten:
            tightened = {
                unit: min(mean, max(density_baseline.get(unit, NEW_UNIT_MEAN), rebased.get(unit, 0)))
                for unit, mean in per_unit.items()
            }
            write_density_baseline(density_path, tightened)
            density_msg = f"{DENSITY_BASELINE} retightened to {len(tightened)} unit(s)."
        elif rebased:
            # A crate split moves headers between units, and both means change
            # with nobody having written a word. The entry follows the files,
            # exactly as the count and grade entries above do -- capped at the
            # unit's own mean, so a move can never buy a unit more room than
            # the headers it now holds.
            carried = dict(density_baseline)
            for unit in sorted(rebased):
                carried[unit] = min(per_unit[unit], unit_ceiling(unit))
                print(f"check-prose: re-based {unit} to {carried[unit] / 100:.2f} mean lines")
            write_density_baseline(density_path, carried)
            density_msg = f"{DENSITY_BASELINE} re-based {len(rebased)} moved unit(s)."
        else:
            density_msg = f"{DENSITY_BASELINE} left alone -- pass --retighten to reclaim slack."
        if retighten:
            count_msg = f"{BASELINE} retightened to {sum(merged.values())}"
        else:
            count_msg = (
                f"{BASELINE} left alone -- pass --retighten to reclaim slack"
            )
        print(f"check-prose: {count_msg}, {density_msg}")
        return 0

    # All three ratchets are judged against max(the recorded ceiling, the same
    # thing's value in the base tree) -- the rule check-file-size.sh uses, and
    # for the same reason: a file or unit already over its ceiling before this
    # change landed is inherited drift, and failing the branch that merely did
    # not fix it is what turned these ratchets into main-red generators.
    # `--absolute` (the post-merge canary) skips the base entirely, because
    # that is exactly the drift a canary exists to catch. Inherited drift is
    # still named on the way past, so forgiving it never means hiding it.
    absolute = "--absolute" in flagset
    base_commit = resolve_base_commit(root, absolute)
    base_moves = renamed_paths(root, base_commit) if base_commit else {}
    came_from = {new: old for old, new in base_moves.items()}
    # A split is a move git cannot name, and its prose is no newer for that.
    if base_commit:
        for new, old in split_sources(root, base_commit).items():
            came_from.setdefault(new, old)
    inherited: list[str] = []

    base_counts: dict[str, dict[str, int]] = {}

    def counts_in_base(path: str) -> dict[str, int]:
        if path not in base_counts:
            base_counts[path] = counts_at_commit(
                root, base_commit, came_from.get(path, path)
            )
        return base_counts[path]

    over = []
    for unit, mean in sorted(per_unit.items()):
        ceiling = density_baseline.get(unit, NEW_UNIT_MEAN)
        if mean <= ceiling:
            continue
        if base_commit:
            base_paths = base_tracked_paths(root, base_commit, unit, tracked, base_moves)
            was = density_at_commit(root, base_commit, unit, base_paths)
            if mean <= was:
                inherited.append(
                    f"{unit}: {mean / 100:.2f} mean header lines, "
                    f"{ceiling / 100:.2f} allowed, {was / 100:.2f} in the base tree"
                )
                continue
            ceiling = max(ceiling, was)
        if mean > ceiling:
            over.append((unit, ceiling, mean))

    failures = []
    for pair in sorted(set(per_pair) | set(baseline)):
        path, pattern = pair
        now = per_pair.get(pair, 0)
        allowed = baseline.get(pair, 0)
        if now <= allowed:
            continue
        if base_commit:
            was = counts_in_base(path).get(pattern, 0)
            if now <= was:
                inherited.append(
                    f"{path} [{pattern}]: {now} found, {allowed} allowed, "
                    f"{was} in the base tree"
                )
                continue
            allowed = max(allowed, was)
        failures.append((pair, allowed, now))

    grade_failures = []
    for path, (grade, worst) in sorted(per_grade.items()):
        allowed_grade = grade_baseline.get(path, NEW_FILE_GRADE)
        if grade <= allowed_grade:
            continue
        if base_commit:
            was = grade_at_commit(root, base_commit, came_from.get(path, path))
            if was is not None and grade <= was:
                inherited.append(
                    f"{path}: grade {grade / 100:.2f}, "
                    f"{allowed_grade / 100:.2f} allowed, "
                    f"{was / 100:.2f} in the base tree"
                )
                continue
            allowed_grade = max(allowed_grade, was or 0)
        grade_failures.append((path, allowed_grade, grade, worst))

    if inherited:
        print(
            "check-prose: drift inherited from the base tree, which this "
            "change did not cause and does not fail on:"
        )
        for line in inherited:
            print(f"  {line}")

    # Each ratchet reports before any of them decides the exit code. Returning
    # on the first failure hid every later one, so a banned phrase kept five
    # grade failures out of the post-merge canary's report (#6230).
    if failures:
        print("check-prose: FAIL -- content-free prose added.\n", file=sys.stderr)
        for (path, pattern), allowed, now in failures:
            print(
                f"  {path} [{pattern}]: {allowed} allowed, {now} found",
                file=sys.stderr,
            )
            for lineno, name, text, remedy in detail.get(path, []):
                if name != pattern:
                    continue
                print(f"      {lineno}: [{name}] {text!r} -- {remedy}", file=sys.stderr)
        print(
            "\nThese constructions announce writing instead of doing it; "
            "deleting one costs the reader nothing.\n"
            "Fix the prose. Do not add a baseline entry. "
            "docs/prose-guidelines.md is the full rule.",
            file=sys.stderr,
        )

    if grade_failures:
        if failures:
            print(file=sys.stderr)
        print(
            "check-prose: FAIL -- prose got harder to read.\n",
            file=sys.stderr,
        )
        for path, allowed_grade, grade, worst in grade_failures:
            print(
                f"  {path}: grade {allowed_grade / 100:.2f} allowed, "
                f"{grade / 100:.2f} found",
                file=sys.stderr,
            )
            for sentence_grade, sentence in worst:
                print(
                    f"      [{sentence_grade:.1f}] {sentence[:110]}",
                    file=sys.stderr,
                )
        print(
            "\nShorten the sentences and use plainer words. The target is a "
            "5th grade reading level (docs/prose-guidelines.md, hard rule 5). "
            f"Do not add a line to {GRADE_BASELINE}.",
            file=sys.stderr,
        )

    if over:
        if failures or grade_failures:
            print(file=sys.stderr)
        print(
            "check-prose: FAIL -- module headers got longer.\n",
            file=sys.stderr,
        )
        for unit, ceiling, mean in over:
            print(
                f"  {unit}: {ceiling / 100:.2f} allowed, "
                f"{mean / 100:.2f} mean header lines",
                file=sys.stderr,
            )
        print(
            "\nA header is what this file does and what a reader must not do "
            "to it -- two to four sentences. History belongs in the pull "
            "request, a design document, or the tracker.\n"
            "Cut a header. Do not raise the number in "
            f"{DENSITY_BASELINE}.",
            file=sys.stderr,
        )

    if failures or grade_failures or over:
        return 1

    total = sum(per_pair.values())
    # Say which mode ran, mirroring check-file-size.sh's mode_note. The
    # base-relative rule is silent by nature -- it only shows itself when
    # something drifts -- so a checkout too shallow to resolve a base falls
    # back to strict and reads as an ordinary green line forever. That is
    # exactly how guard-self-tests.yml shipped this ratchet as a whole-tree
    # check while every one of its three allowances believed otherwise
    # (#6273); naming the mode is what makes the difference legible in a log
    # rather than a thing to re-derive.
    if base_commit:
        mode_note = f" Judged against {base_commit[:12]}."
    else:
        mode_note = " No base resolved -- strict whole-tree check."
    print(
        f"check-prose: OK -- {total} grandfathered construction(s) "
        f"in {len(files)} file(s), none added; "
        f"{len(per_unit)} unit(s) within their header-length ceiling; "
        f"{len(per_grade)} file(s) within their reading-grade ceiling."
        f"{mode_note}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
