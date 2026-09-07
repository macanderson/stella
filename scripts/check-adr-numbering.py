#!/usr/bin/env python3
"""Every ADR number identifies exactly one record, and the index lists it.

`docs/adr/` is a shared cell in the sense AGENTS.md describes for `Cargo.lock`
and `scripts/file-size-baseline.txt`: two branches each pick "the next ADR
number", both are correct against their own base, and they compose into a
directory where a number no longer identifies a record. No pre-merge run can
catch that from one side, because neither author's tree is wrong -- so the
check has to be a property of the merged tree.

A pull request's checkout IS the merged tree (`refs/pull/N/merge`), so this
answers the composition question there. What it cannot answer is a run that
went stale: nothing re-runs a green check when `main` moves under it, and the
second branch to merge carries a verdict taken before the first one landed.
That is why `scripts/main-canary.sh` runs this again after the merge, beside
`Cargo.lock` and `scripts/file-size-baseline.txt` (`#5930`).

`main` carried two 0015 records before this guard existed (#5175), and PR
#5165 had to renumber by hand twice while racing them.

Four properties, all fatal. None is ratcheted: a duplicate number is never
acceptable debt, and the other three are cheap to satisfy the moment a record
is authored.

1. **Unique number.** No two files share a 4-digit prefix.
2. **The heading's number agrees with the filename.** A rename that repoints
   the file and forgets the heading is the failure this catches -- #5165
   shipped one where the file, the frontmatter, the manifest and both code
   citations moved and a prose line did not.

   Two heading styles are in use (`# ADR 0017: ...` and MADR's `# 16. ...`)
   and both are accepted. The number is what this guard is about; which style
   the directory settles on is a separate decision, and inventing one here
   would fail records that are correct.
3. **The frontmatter `id`'s number agrees with the filename**, when there is
   frontmatter. Two id conventions are in use -- `0001-semantic-taxonomy` and
   `adr/0017-plan-graph-persistence` -- and each record's `docs/manifest.json`
   key matches its own declaration, so both are internally consistent. This
   guard compares the number, not the prefix, for the same reason as above.

   Seven legacy records have no frontmatter and are therefore not citable by
   id at all (`doc:` citation, AGENTS.md); this check does not require them to
   grow one, because whether an old ADR should become citable is its own
   decision. It only holds the ones that made the claim.
4. **The index lists it.** Every record appears in `docs/adr/README.md`, which
   turns that table from prose into a checked list -- the move `god-files`
   makes for the file-size baseline.
"""

from __future__ import annotations

import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

# The directory to check. Defaults to the repository's own; the hermetic
# self-test (`scripts/test-adr-numbering.sh`) points it at a fixture tree so
# it can prove this guard still fails without touching `docs/adr/`.
ADR_DIR = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("docs/adr")
INDEX = ADR_DIR / "README.md"
FILENAME = re.compile(r"^(\d{4})-[a-z0-9-]+\.md$")
# `# ADR 0017: ...` or MADR's `# 16. ...` — the number is what matters.
HEADING = re.compile(r"^#\s+(?:ADR\s+)?(\d{1,4})[.:\s]", re.MULTILINE)
FRONTMATTER_ID = re.compile(r"^id:\s*(\S+)\s*$", re.MULTILINE)


def next_free_number(by_number: dict[str, list[str]]) -> str:
    """The number a renumber should take: one past the highest in use."""
    return f"{max(int(n) for n in by_number) + 1:04d}"


def bare_citations(number: str) -> list[str]:
    """Files writing `ADR NNNN` in prose, outside `docs/adr/` itself.

    A renumber has to move every citation with the record, and the two link
    checks in this repository read neither of these: `check-doc-links.py`
    follows `doc:` ids and markdown links, and `make line-citations` reads
    line numbers. A bare number in a sentence is invisible to both, so
    `#6240`'s renumber repointed seven of them by hand across four crates and
    a spec after git had already reported the merge clean.

    Best-effort and advisory: it never changes the verdict, and it is skipped
    entirely when this guard is pointed at a fixture directory, so the
    hermetic self-test greps no real tree.
    """
    if ADR_DIR != Path("docs/adr"):
        return []
    try:
        proc = subprocess.run(
            ["git", "grep", "-l", "-F", f"ADR {number}", "--", ":!docs/adr"],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    return sorted(line for line in proc.stdout.splitlines() if line)


def main() -> int:
    if not ADR_DIR.is_dir():
        print(f"check-adr-numbering: no {ADR_DIR}/ -- nothing to check.")
        return 0

    records = sorted(p for p in ADR_DIR.glob("*.md") if FILENAME.match(p.name))
    if not records:
        print("check-adr-numbering: no ADR records found.")
        return 0

    index_text = INDEX.read_text(encoding="utf-8") if INDEX.exists() else ""
    failures: list[str] = []

    by_number: dict[str, list[str]] = defaultdict(list)
    for path in records:
        number = FILENAME.match(path.name).group(1)  # type: ignore[union-attr]
        by_number[number].append(path.name)

    free = next_free_number(by_number)
    for number, names in sorted(by_number.items()):
        if len(names) > 1:
            joined = "\n      ".join(sorted(names))
            message = (
                f"  {number} identifies {len(names)} records:\n      {joined}\n"
                f"      Renumber all but one to {free}. Move its `id:`, its "
                f"heading, its docs/manifest.json entry and every "
                f"`doc:adr/...` citation with it."
            )
            citations = bare_citations(number)
            if citations:
                listed = "\n      ".join(citations)
                message += (
                    "\n      These write the number in prose, where no link "
                    f"check reads it:\n      {listed}"
                )
            failures.append(message)

    for path in records:
        number = FILENAME.match(path.name).group(1)  # type: ignore[union-attr]
        stem = path.stem
        text = path.read_text(encoding="utf-8")

        heading = HEADING.search(text)
        if heading is None:
            failures.append(
                f"  {path.name} has no numbered heading "
                f"(`# ADR NNNN: ...` or `# NN. ...`)."
            )
        elif int(heading.group(1)) != int(number):
            failures.append(
                f"  {path.name} is numbered {number} and its heading says "
                f"{heading.group(1)} -- a half-finished renumber."
            )

        # Frontmatter is optional (see the module docstring); an `id` that
        # exists must agree with the filename.
        if text.startswith("---"):
            found = FRONTMATTER_ID.search(text.split("---", 2)[1] if text.count("---") >= 2 else "")
            if found:
                declared = found.group(1).removeprefix("adr/")
                if declared != stem:
                    failures.append(
                        f"  {path.name} declares id `{found.group(1)}`; its "
                        f"filename stem is `{stem}`."
                    )

        if stem not in index_text:
            failures.append(
                f"  {path.name} is not listed in {INDEX} -- add a row so the "
                f"index is a checked list rather than prose."
            )

    if failures:
        print("check-adr-numbering: FAIL\n")
        for line in failures:
            print(line)
        print(
            "\nAn ADR number is an address: two records sharing one makes every "
            "citation ambiguous.\nSee docs/adr/README.md and #5175."
        )
        return 1

    print(
        f"check-adr-numbering: OK -- {len(records)} record(s), "
        f"every number unique, every heading and id in agreement, "
        f"every record indexed."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
