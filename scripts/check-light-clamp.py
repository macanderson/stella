#!/usr/bin/env python3
"""Hold a shipped light surface to the clamp its colour family declares (#4941).

`design/tokens/stella-tokens.json` states the light ground's law:

    "warm-paper": {
      "predicate": "r >= g >= b AND 100*g >= 97*r AND 100*b >= 93*r",
      "why": "The light ground's clamp: warm or neutral, never blue."
    }

Until this script, that law applied to nothing that ships. It was enforced in
exactly two places -- `gen-tokens.py::validate` and
`crates/stella-tui-theme/src/clamp.rs::satisfies` -- and both iterate the token
table, so the clamp only ever judged colours that were **already tokens**. A
hand-picked light neutral that is not a token was invisible to it, and being a
non-token is exactly the property that makes such a value the problem.

So this guard reads **surfaces**. Every light-scheme declaration on the five
web surfaces and the command deck's paper ramp is classified and judged:

  1. A value that IS a kit token is held to **that token's own declared
     clamp**. `--text: #0A0A0C` on a light page is the dark canvas token being
     reused; whether that is the right value is #4072's question, and it is
     not this guard's. Whether it satisfies the clamp it declares is.
  2. A value that is NOT a token is held to the clamp of the **family its
     surface declares** for that role, in `SURFACES` below.
  3. A value that is neither a token nor declared is **unclassifiable**, and
     that fails. It is the one outcome that must not be silent: a new
     hand-picked light neutral arriving under a new name is the exact event
     this guard exists to catch, and skipping what it cannot classify would
     make it blind to precisely that.

The predicates themselves are **not written here**. `gen-tokens.py`'s
`check_clamp` is imported and called, so there is one Python implementation of
`warm-paper` in the repository rather than two. A predicate written twice is
the drift this repository keeps paying for -- `clamp.rs` is the second
implementation and `check-tokens.py` is what holds it to the first.

## The ratchet

The tree does not pass this guard, and that is the finding rather than a bug in
the guard. #4072 is deciding which values the light scheme should have --
recolour the surfaces to the warm authority, or write a second cool clamp into
the authority -- and that decision is a palette owner's. This guard is the half
that is the same either way: something has to hold a shipped surface to a
clamp, or the next hand-picked neutral lands exactly as this one did.

So it ships as a **down-only ratchet** (`scripts/light-clamp-baseline.txt`), on
`scripts/contrast-baseline.txt`'s model and for the same stated reason: the
rule predates the guard, so the baseline records a debt that already existed
rather than granting new permission. It records the **measured value**, not a
count, so repainting a baselined role to a *different* off-clamp value fails --
the licence is for the hex that shipped, not for the name.

`--update` drops an entry whose value now satisfies its clamp and **refuses to
add one**, so the only way past a red gate is to move the colour. `--bootstrap`
writes the file once and refuses if it exists. The file is meant to reach
empty; adding an entry by hand is the expedient CLAUDE.md forbids, and widening
the clamp to make the gate green would delete the only law the light ground
has.
"""

from __future__ import annotations

import importlib.util
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS_REL = Path("design") / "tokens" / "stella-tokens.json"
BASELINE_REL = Path("scripts") / "light-clamp-baseline.txt"

BASELINE_HEADER = """\
# Down-only ratchet for scripts/check-light-clamp.py.
#
# Each line is `<file> <role> <hex>` -- a light-scheme value that fails the
# clamp its family declares, recorded at the value it shipped at. A repaint to
# a *different* off-clamp value fails: the licence is for this hex, not for
# this name.
#
# `make light-clamp-update` drops an entry that now satisfies its clamp and
# refuses to add one. The only way past a red gate is to move the colour.
#
# This file records the debt #4941 found, which #4072 is deciding how to pay.
# It is meant to reach empty.
"""


def load_generator(root: Path):
    """`gen-tokens.py`, imported so its predicates are not written twice.

    Read from `root` rather than from this script's own repository, so a tree
    passed on the command line is judged entirely by itself -- its clamps, its
    surfaces and its predicates. `scripts/test-light-clamp.sh` builds such a
    tree, and this is what makes the generator it copies in the one that runs.
    """
    path = root / "scripts" / "gen-tokens.py"
    spec = importlib.util.spec_from_file_location("stella_gen_tokens", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# ── What a surface declares ─────────────────────────────────────────────────
#
# `families` names the colour family for each light-scheme role whose value is
# NOT a kit token. A role carrying a token needs no entry: rule 1 reads the
# clamp off the token. A role carrying neither is unclassifiable and fails.
#
# `None` is an exemption and is reported but never failed on, the same shape
# and for the same kind of reason as `check-contrast.py`'s exempt pairings: a
# categorical hue identifies a *kind of thing*, and a category that changed
# colour with the ambient theme would stop being one. It is outside the
# ground's warm/cool law, not failing it.

CSS = "css"
RUST = "rust"

# `gate` names the light block to read. Discovery belongs to
# `crates/stella-cli/tests/design_token_parity.rs`, which finds every light
# block a surface declares and proves they agree with each other -- so one
# block read here is every block. That held for the media query and the
# `data-theme` attribute from the start, and for the bare `:root` block only
# since #4973: the benchmark pages state their light scheme there and repeat it
# verbatim under the attribute, and until that issue the base copy -- the one a
# reader who never touched the toggle gets -- was compared against nothing.
# This guard reads six files in two syntaxes, one of them Rust, where a CSS
# selector scan has nothing to find.
SURFACES = (
    {
        "file": "crates/stella-observatory/src/assets/index.html",
        "notation": CSS,
        "gate": (':root[data-theme="light"]{', "\n}"),
        "families": {
            "--ground": "warm-paper",
            "--surface": "warm-paper",
            "--raised": "warm-paper",
            "--hairline": "warm-paper",
            "--hairline-strong": "warm-paper",
            "--sunken": "warm-paper",
            "--text-emph": "warm-paper",
            "--ink": "warm-paper",
            "--identity-ink": "warm-paper",
            # The wordmark's darkest stop on paper. An ink like any other.
            "--mark-bright": "warm-paper",
        },
    },
    {
        "file": "crates/stella-cli/src/export.rs",
        "notation": CSS,
        "gate": (':root[data-theme="light"] {{', "\n  }}"),
        "families": {
            "--ground": "warm-paper",
            "--surface": "warm-paper",
            "--raised": "warm-paper",
            "--hairline": "warm-paper",
            "--hairline-strong": "warm-paper",
            "--sunken": "warm-paper",
            "--ink": "warm-paper",
            "--identity-ink": "warm-paper",
        },
    },
    {
        "file": "crates/stella-transcript/src/html/transcript.css",
        "notation": CSS,
        "gate": ("@media (prefers-color-scheme: light) {", "\n  }"),
        "families": {
            "--bg": "warm-paper",
            "--panel": "warm-paper",
            "--raised": "warm-paper",
            "--line": "warm-paper",
            "--line2": "warm-paper",
            "--sunken": "warm-paper",
            "--sunken-2": "warm-paper",
            "--hover": "warm-paper",
            "--hover-raised": "warm-paper",
            "--selected": "warm-paper",
            "--hairline-soft": "warm-paper",
            "--code": "warm-paper",
            "--hunk-bg": "warm-paper",
            # The three categorical inks. A category that changed colour with
            # the ambient theme would stop being one, which the file's own
            # header says; they identify a diff hunk, a prompt-quote and the
            # reader's own prose, and identity is not a ground.
            "--hunk-ink": None,
            "--pq-ink": None,
            "--you-prose": None,
        },
    },
    {
        "file": "docs/benchmarks/index.html",
        "notation": CSS,
        "gate": (':root[data-theme="light"]{', "}"),
        "families": {
            "--bg": "warm-paper",
            "--sub": "warm-paper",
            "--panel": "warm-paper",
            "--rule": "warm-paper",
            "--rule-2": "warm-paper",
        },
    },
    {
        "file": "docs/benchmarks/terminal-bench-2-1-glm-5-2.html",
        "notation": CSS,
        "gate": (':root[data-theme="light"]{', "}"),
        "families": {
            "--bg": "warm-paper",
            "--sub": "warm-paper",
            "--panel": "warm-paper",
            "--rule": "warm-paper",
            "--rule-2": "warm-paper",
            "--code": "warm-paper",
            # The verdict washes: a green and a red at paper lightness. They
            # are a verdict's ground, not the page's, and the kit's `verdict`
            # clamp is what governs a verdict's hue.
            "--pass-bg": "verdict",
            "--fail-bg": "verdict",
        },
    },
    {
        # The command deck's own paper ramp. Rust rather than CSS, which is why it needed
        # #4910's `Color::Rgb` notation before it could be read at all.
        "file": "crates/stella-tui/src/palette.rs",
        "notation": RUST,
        "gate": ("/// Light background", "// -- Data marks"),
        "families": {
            "PAPER": "warm-paper",
            "SNOW": "warm-paper",
            "PAPER_RAISED": "warm-paper",
            "PAPER_HAIRLINE": "warm-paper",
            "INK_MUTED": "warm-paper",
            "INK_DIM": "warm-paper",
            "INK_EMPHASIS": "warm-paper",
        },
    },
)

CSS_DECL = re.compile(r"(--[a-zA-Z0-9-]+)\s*:\s*(#[0-9A-Fa-f]{6})\b")
RUST_DECL = re.compile(
    r"pub const ([A-Z][A-Z0-9_]*)\s*:\s*Color\s*=\s*Color::Rgb\(\s*"
    r"(0[xX][0-9A-Fa-f]{1,2})\s*,\s*(0[xX][0-9A-Fa-f]{1,2})\s*,\s*"
    r"(0[xX][0-9A-Fa-f]{1,2})\s*\)"
)


def block(text: str, start: str, end: str, where: str) -> str:
    """The slice between two markers, exclusive of them."""
    try:
        head = text.index(start) + len(start)
    except ValueError:
        raise SystemExit(f"{where}: marker {start!r} not found") from None
    try:
        return text[head : head + text[head:].index(end)]
    except ValueError:
        raise SystemExit(f"{where}: marker {end!r} not found after {start!r}") from None


def declarations(surface: dict, root: Path) -> list[tuple[str, str]]:
    """`(role, #RRGGBB)` for every colour the surface's light scheme declares."""
    path = root / surface["file"]
    body = block(path.read_text(), *surface["gate"], surface["file"])
    if surface["notation"] == CSS:
        return [(name, hexv.upper()) for name, hexv in CSS_DECL.findall(body)]
    return [
        (m.group(1), "#%02X%02X%02X" % tuple(int(m.group(i), 16) for i in (2, 3, 4)))
        for m in RUST_DECL.finditer(body)
    ]


def audit(root: Path) -> tuple[list[tuple[str, str, str, str]], list[str], int]:
    """Every light declaration judged.

    Returns `(violations, unclassifiable, exempt_count)`, where a violation is
    `(file, role, hex, why)`.
    """
    generator = load_generator(root)
    doc = json.loads((root / TOKENS_REL).read_text())
    clamps = doc["clamps"]
    anchors = {t["name"]: t["hex"] for t in doc["tokens"]}
    by_hex = {t["hex"].upper(): (t["name"], t["clamp"]) for t in doc["tokens"]}

    violations: list[tuple[str, str, str, str]] = []
    unclassifiable: list[str] = []
    exempt = 0

    for surface in SURFACES:
        for role, hexv in declarations(surface, root):
            token = by_hex.get(hexv)
            if token:
                # Rule 1: a token answers to the clamp it declares.
                name, clamp = token
                label = f"{role} (kit `{name}`)"
            elif role in surface["families"]:
                clamp = surface["families"][role]
                if clamp is None:
                    exempt += 1
                    continue
                label = role
            else:
                # Rule 3. The one outcome that must never be silent.
                unclassifiable.append(
                    f"{surface['file']}: {role} is {hexv}, which is not a kit "
                    f"token and names no family in this guard's SURFACES table. "
                    f"Say which colour family it belongs to, or make it a token."
                )
                continue

            if clamp not in clamps:
                unclassifiable.append(
                    f"{surface['file']}: {role} declares family {clamp!r}, "
                    f"which design/tokens/stella-tokens.json does not define"
                )
                continue
            why = generator.check_clamp(label, hexv, clamp, clamps[clamp], anchors)
            if why:
                violations.append((surface["file"], role, hexv, why))

    return violations, unclassifiable, exempt


def read_baseline(root: Path) -> dict[tuple[str, str], str]:
    path = root / BASELINE_REL
    if not path.exists():
        return {}
    held: dict[tuple[str, str], str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        file, role, hexv = line.split()
        held[(file, role)] = hexv.upper()
    return held


def write_baseline(root: Path, held: dict[tuple[str, str], str]) -> None:
    body = "".join(f"{file} {role} {hexv}\n" for (file, role), hexv in sorted(held.items()))
    (root / BASELINE_REL).write_text(BASELINE_HEADER + body, encoding="utf-8")


def report(root: Path) -> int:
    violations, unclassifiable, exempt = audit(root)
    held = read_baseline(root)
    print(f"{'surface':<52} {'role':<20} value      verdict")
    for file, role, hexv, _ in violations:
        verdict = "held" if held.get((file, role)) == hexv else "FAIL"
        print(f"{file:<52} {role:<20} {hexv}    {verdict}")
    for line in unclassifiable:
        print(f"  unclassifiable: {line}")
    print(
        f"\n{len(violations)} violation(s), {len(held)} held by the ratchet, "
        f"{exempt} declared exempt. Every value is judged against the clamp its "
        "family declares in design/tokens/stella-tokens.json."
    )
    return 0


def main(argv: list[str]) -> int:
    flags = {a for a in argv[1:] if a.startswith("--")}
    positional = [a for a in argv[1:] if not a.startswith("--")]
    root = Path(positional[0]).resolve() if positional else REPO

    if "--report" in flags:
        return report(root)

    violations, unclassifiable, _ = audit(root)
    held = read_baseline(root)
    measured = {(file, role): hexv for file, role, hexv, _ in violations}

    # Neither writing mode may run while a role is unclassifiable. Which colour
    # family is this? is a question for a person, and a writer cannot answer it
    # -- so without this the ratchet is written cleanly around a role nobody has
    # judged and the run reports success. `test-light-clamp.sh`'s U2 found it in
    # `--update`; `--bootstrap` had it too, reachable only on a tree with no
    # ratchet yet, which is exactly the tree with nobody to notice.
    if unclassifiable and flags & {"--bootstrap", "--update"}:
        print(
            f"check-light-clamp: refusing to write the ratchet while "
            f"{len(unclassifiable)} role(s) are unclassifiable. Classify them "
            "first — a ratchet cannot stand in for a decision:\n",
            file=sys.stderr,
        )
        for line in unclassifiable:
            print(f"  {line}", file=sys.stderr)
        return 1

    if "--bootstrap" in flags:
        if (root / BASELINE_REL).exists():
            print(
                f"check-light-clamp: {BASELINE_REL} already exists — refusing to "
                "bootstrap over a ratchet. Use --update.",
                file=sys.stderr,
            )
            return 1
        write_baseline(root, measured)
        print(f"check-light-clamp: bootstrapped {len(measured)} entry/entries.")
        return 0

    if "--update" in flags:
        added = sorted(k for k in measured if k not in held)
        if added:
            print(
                "check-light-clamp: refusing to grandfather "
                f"{len(added)} new violation(s). A ratchet records debt that "
                "predates the guard; it does not hand out permission:\n",
                file=sys.stderr,
            )
            for file, role in added:
                print(f"  {file}: {role} is {measured[(file, role)]}", file=sys.stderr)
            return 1
        changed = sorted(k for k in measured if held.get(k) != measured[k])
        if changed:
            print(
                "check-light-clamp: refusing to move an entry to a different "
                "off-clamp value. The licence is for the hex that shipped:\n",
                file=sys.stderr,
            )
            for file, role in changed:
                print(
                    f"  {file}: {role} was {held[(file, role)]}, is now "
                    f"{measured[(file, role)]}",
                    file=sys.stderr,
                )
            return 1
        write_baseline(root, measured)
        print(f"check-light-clamp: retightened to {len(measured)} entry/entries.")
        return 0

    failures: list[str] = list(unclassifiable)
    for file, role, hexv, why in violations:
        recorded = held.get((file, role))
        if recorded == hexv:
            continue
        if recorded is None:
            failures.append(f"{file}: {why} — no baseline entry")
        else:
            failures.append(
                f"{file}: {role} was {recorded} and is now {hexv}; the ratchet "
                f"licenses the value that shipped, not the name. {why}"
            )
    stale = sorted(k for k in held if k not in measured)
    for file, role in stale:
        failures.append(
            f"{file}: {role} satisfies its clamp now — run "
            "`make light-clamp-update` so the ratchet retightens"
        )

    if failures:
        print("check-light-clamp: FAIL\n", file=sys.stderr)
        for line in failures:
            print(f"  {line}", file=sys.stderr)
        print(
            "\nA light neutral is fixed by moving the colour, never by widening "
            "the clamp — that would delete the only law the light ground has. "
            "See `make light-clamp-report` for every declaration and its family.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-light-clamp: {len(SURFACES)} shipped light surface(s) judged, "
        f"{len(held)} held by the ratchet, none off its declared clamp."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
