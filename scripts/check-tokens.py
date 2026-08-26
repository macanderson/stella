#!/usr/bin/env python3
"""Guard the colour system: no retired hex anywhere, no stray hex where it matters.

Three checks, deliberately different in strength, because "never use this colour
again", "use only tokens here" and "quote this token correctly" are different
promises and only one of them can be made about the whole tree today.

1. **The ban.** Every hex in `banned.values` in the token JSON fails wherever it
   appears, in any tracked file. These are the anchor values of retired brand
   kits. The failure mode they name is real and has happened twice: a surface
   left on a retired ramp while a doc comment claimed it mirrored the current
   one, so a reader crossing from the site to the Observatory watched the brand
   change hue. A retired hex has no legitimate use, so this check has no
   allowlist.

2. **Token-only paths.** Inside `TOKEN_ONLY` the rule is stronger: every hex must
   be a live token. This is where colour is *authored* — the stylesheets and
   renderers a designer reaches for — and it is the only scope where "no hex
   literals, only tokens" is a promise the tree can actually keep. Raster and
   vector assets are out of scope by construction; they are checked by the ban
   instead.

3. **Citations.** Wherever a document writes a token's CSS name with a hex beside
   it — a kit table row, a stylesheet declaration, a design brief — that hex must
   be the value the token holds today. Checks 1 and 2 are both *membership*
   questions and neither can see a mis-quote: a superseded value is not banned
   (nothing retired it, the palette simply moved past it), and a value bound to
   the wrong name is still a live token, so it satisfies TOKEN_ONLY while saying
   something false.

   Both failures were in the tree when this check was written (#3653). The kit
   page and the design brief each published the whole paper family plus
   `--st-ink`/`--st-ink-muted` at the pre-#4282 cool values, nine rows apiece, so
   the two documents a designer reads to learn the palette stated nine colours
   the palette does not have. And `docs/brand/css/tokens.css` and
   `website/src/app/tokens.css` both bound the deck's `--st-text` to the value
   `--st-paper-text` carries — the on-deck primary text collapsed onto the
   off-deck one, on the live marketing site, under a guard that reported clean
   because both values are live tokens.

   A page's own chrome is out of scope by construction: this fires only where a
   document names an `--st-*` token, so a marketing surface keeping its own
   accent and status hues under its own variable names is untouched. That is the
   line #2594 drew, and this check does not cross it.

`MIGRATING` is the one escape, and it is a ledger, not an allowlist: a path
listed there is a surface this system has not reached yet, each entry carrying
the issue that finishes it. The guard **refuses to add entries** -- there is no
`--update`. The only way a path leaves the ledger is by being migrated.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS_REL = "design/tokens/stella-tokens.json"

# Where every hex must be a live token, not merely a non-retired one.
TOKEN_ONLY = (
    "design/tokens/",
    "website/src/app/tokens.css",
    "docs/brand/css/tokens.css",
)

# Files that define the system itself, and so must be able to name retired
# values in order to ban them.
#
# The last three are **ban sites**: tests whose whole job is asserting that a
# retired value does not ship, on a surface this script cannot see. A hex sweep
# reads them as offenders, and that is exactly backwards -- the list *is* the
# enforcement. Leaving them out is not a theoretical hazard: the v5.0 migration
# swept both, rewriting `#ffb000`/`#0b0b0c` into the live gold and canvas, so
# `brand-parity.test.ts` spent one merge banning the brand it exists to protect
# and reported nineteen offenders that were all correct (#4066).
# `crates/stella-tui/src/theme/tests.rs` joined them when RUST_RGB below gave
# this script eyes on Rust: its `RETIRED_*` constants are the ban list for that
# surface, so the first thing the new matcher saw was the one file whose whole
# job is naming those values (#4910). They earn the
# exemption for the same reason the four above do, and they need it more,
# because they are the only files here a sweep would happily "fix".
#
# The exemption is narrow by construction, and check 3 is what the narrowness
# now buys: it suppresses the *ban* only. A ban site names a retired value on
# purpose; that is no licence to misquote a live one, so its citations are still
# checked. Neither file is a TOKEN_ONLY path either, so nothing here lets a live
# surface carry a retired hex.
#
# Nothing is exempt from all three. The suite needs no entry here because every
# fixture reads its colours out of the token file at run time — see the note in
# scripts/test-tokens.sh, which is the discipline that keeps this tuple from
# growing.
SELF = (
    "design/tokens/stella-tokens.json",
    "scripts/check-tokens.py",
    "scripts/gen-tokens.py",
    "crates/stella-tui-theme/src/token.rs",
    "design/tokens/stella-tokens.css",
    "website/src/lib/brand-parity.test.ts",
    "crates/stella-cli/src/export/tests.rs",
    "crates/stella-tui/src/theme/tests.rs",
)

# Surfaces this system has not reached yet. Each entry names the issue that
# closes it. This list is meant to reach empty; nothing in this script can add
# to it.
#
# Every entry here is a place where a hex remap would have made the file
# *worse*, not a place the migration ran out of patience. That distinction is
# the only thing separating a ledger from an allowlist, so it is stated per
# entry rather than assumed.
# Empty, and #4056 / #4057 are what emptied it. Both entries were correct while
# they stood — a hex remap really would have made those files worse, which is
# what a ledger records and an allowlist does not. They came off the way the
# comment above asks: the guidelines page's §03 was rewritten against v5.0 from
# design/tokens/stella-tokens.json rather than remapped, and the two design
# briefs were rewritten (stella-design-system-prompt.md) and marked superseded
# by design/tui-v2/SPEC.md §2-4 with their retired hexes removed
# (stella-tui-prompt.md). Adding an entry back is not a way to make this script
# green.
MIGRATING: dict[str, str] = {}

HEX = re.compile(r"#[0-9A-Fa-f]{6}\b")

# The same colours, written the other way. CSS accepts `rgb(197 138 50)` and
# `rgba(7,11,16,.28)` for values a hex sweep cannot see, and both spellings were
# live in this tree: 17 retired values survived the first pass of the v5.0
# migration purely by not being written in hex, including a bronze gradient
# sweep on the marketing site. A guard that watches one notation of a two-
# notation language is a guard that reports zero and means nothing.
#
# Not a general colour parser: `hsl()`, `color()` and `oklch()` are
# not matched, and neither is a computed value. This closes the gap that was
# actually open rather than pretending to close all of them — see the note in
# the report for what remains unwatched.
RGB_FUNC = re.compile(
    r"rgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})", re.IGNORECASE
)
# The same colours again, in the notation Rust writes them: ratatui spells a
# colour `Color::Rgb(0x0A, 0x0A, 0x0C)`, and the hex-literal channels are what
# no matcher here could see. #4910 reports this as Rust being invisible
# outright; it is one notation narrower than that, and the suite is what caught
# the difference. RGB_FUNC is IGNORECASE, so `Rgb(11, 11, 12)` already matched
# as an `rgb(` call and the decimal spelling was covered by accident. The `0x`
# spelling was not, and it is the one this tree actually writes — the single
# banned value in the tree when this landed was `Color::Rgb(0x0B, 0x0B, 0x0C)`,
# and twenty-one more literals in `crates/stella-tui/src/palette.rs` had never
# been checked against anything.
#
# Both channel spellings are matched anyway rather than hex alone, so the two
# notations stop depending on an unrelated flag on another pattern. A bare
# `Rgb(` counts as well as `Color::Rgb(`: the name can be imported, and a false
# positive requires three channels that spell a retired brand colour, which is
# worth flagging wherever it is written.
RUST_RGB = re.compile(
    r"\bRgb\s*\(\s*(0[xX][0-9A-Fa-f]{1,2}|\d{1,3})\s*,"
    r"\s*(0[xX][0-9A-Fa-f]{1,2}|\d{1,3})\s*,"
    r"\s*(0[xX][0-9A-Fa-f]{1,2}|\d{1,3})\s*[,)]"
)


def channel(text: str) -> int:
    """One `Color::Rgb` channel, hex-literal or decimal."""
    return int(text, 16) if text[:2].lower() == "0x" else int(text)


# A token's CSS name with a hex beside it: `--st-bg: #0A0A0C`, `<td>--st-bg</td>
# <td>#0A0A0C</td>`, `` `--st-bg` | `#0A0A0C` ``. One line, because every one of
# the citations in the tree writes the name and the value together and the widest
# gap between them is twelve characters -- so the window below is four times what
# any real citation needs and still cannot reach across a table row.
#
# A second `--st-` inside the gap ends the match: on a minified line declaring
# several tokens in a row, the hex after the gap belongs to that later name, and
# pairing it with the earlier one would report both as wrong.
CITATION = re.compile(r"(--st-[a-z0-9-]+)\b((?:(?!--st-)[^\n]){0,48}?)(#[0-9A-Fa-f]{6})\b")


# Text extensions only. A hex "found" inside a PNG is a coincidence.
TEXT_SUFFIXES = {
    ".css",
    ".scss",
    ".html",
    ".htm",
    ".svg",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".mjs",
    ".rs",
    ".py",
    ".md",
    ".mdx",
    ".json",
    ".toml",
    ".yml",
    ".yaml",
    ".sh",
    ".ps1",
    ".txt",
}


def tracked_files(root: Path) -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [Path(p) for p in out.split("\0") if p]


def main(argv: list[str]) -> int:
    # A root argument is what lets the suite point this at a fixture tree; with
    # no argument it reads its own repository, as every caller does today.
    # Without it the guard could not be shown to fire, which is the same defect
    # one notation over from the one it exists to catch.
    root = Path(argv[1]).resolve() if len(argv) > 1 else REPO
    doc = json.loads((root / TOKENS_REL).read_text())
    live = {t["hex"].upper() for t in doc["tokens"]}
    banned = {e["hex"].upper(): e["was"] for e in doc["banned"]["values"]}
    by_css = {t["css"]: t["hex"].upper() for t in doc["tokens"]}

    ban_hits: list[str] = []
    stray_hits: list[str] = []
    citation_hits: list[str] = []
    citations_ok = 0
    # One line, one report. The notations overlap — RGB_FUNC's IGNORECASE and
    # RUST_RGB both match `Rgb(11, 11, 12)` — so without this a single literal
    # is named twice and the count above the list says two places when there is
    # one. Keyed on the value as well as the position, because two different
    # retired colours on one line are two findings.
    seen: set[tuple[str, int, str]] = set()

    def record(posix: str, lineno: int, hexv: str, hit: str) -> None:
        if (posix, lineno, hexv) in seen:
            return
        seen.add((posix, lineno, hexv))
        ban_hits.append(hit)

    for rel in tracked_files(root):
        posix = rel.as_posix()
        if rel.suffix not in TEXT_SUFFIXES:
            continue
        if any(posix.startswith(p) or posix == p for p in MIGRATING):
            continue
        path = root / rel
        try:
            text = path.read_text(errors="ignore")
        except OSError:
            continue

        # SELF suppresses the *ban* and nothing else, which is what the comment
        # above that tuple promises.
        is_self = posix in SELF
        token_only = any(posix.startswith(p) or posix == p for p in TOKEN_ONLY)

        for lineno, line in enumerate(text.splitlines(), 1):
            for match in CITATION.finditer(line):
                name, hexv = match.group(1), match.group(3).upper()
                want = by_css.get(name)
                if want is None:
                    citation_hits.append(
                        f"{posix}:{lineno}: {name} is given the value "
                        f"{match.group(3)}, but no token declares that name"
                    )
                elif want != hexv:
                    citation_hits.append(
                        f"{posix}:{lineno}: {name} is quoted as {match.group(3)} "
                        f"but holds {want}"
                    )
                else:
                    citations_ok += 1
            for match in HEX.finditer(line):
                hexv = match.group(0).upper()
                if hexv in banned:
                    if not is_self:
                        record(
                            posix,
                            lineno,
                            hexv,
                            f"{posix}:{lineno}: {match.group(0)} — {banned[hexv]}",
                        )
                elif token_only and hexv not in live:
                    stray_hits.append(
                        f"{posix}:{lineno}: {match.group(0)} is not a token in "
                        f"design/tokens/stella-tokens.json"
                    )
            for match in RGB_FUNC.finditer(line):
                channels = tuple(int(c) for c in match.groups())
                if any(c > 255 for c in channels):
                    continue
                hexv = "#%02X%02X%02X" % channels
                if hexv in banned and not is_self:
                    record(
                        posix,
                        lineno,
                        hexv,
                        f"{posix}:{lineno}: {match.group(0)}) = {hexv} — {banned[hexv]}",
                    )
            for match in RUST_RGB.finditer(line):
                channels = tuple(channel(c) for c in match.groups())
                if any(c > 255 for c in channels):
                    continue
                hexv = "#%02X%02X%02X" % channels
                if hexv in banned and not is_self:
                    record(
                        posix,
                        lineno,
                        hexv,
                        f"{posix}:{lineno}: {match.group(0)} = {hexv} — {banned[hexv]}",
                    )

    failed = False
    if ban_hits:
        failed = True
        print(
            f"retired brand hexes found in {len(ban_hits)} place(s). These belong to "
            "superseded kits and have no legitimate use:\n",
            file=sys.stderr,
        )
        for hit in ban_hits:
            print(f"  {hit}", file=sys.stderr)
        print(
            "\nreplace each with the token that carries the same role — see "
            "design/tokens/stella-tokens.json.",
            file=sys.stderr,
        )
    if stray_hits:
        failed = True
        print(
            f"\nhex literals inside token-only paths ({len(stray_hits)}):\n",
            file=sys.stderr,
        )
        for hit in stray_hits:
            print(f"  {hit}", file=sys.stderr)
    if citation_hits:
        failed = True
        print(
            f"\ntoken values misquoted in {len(citation_hits)} place(s). A document "
            "that publishes a token's value is read as the palette:\n",
            file=sys.stderr,
        )
        for hit in citation_hits:
            print(f"  {hit}", file=sys.stderr)
        print(
            "\nquote the value design/tokens/stella-tokens.json holds, or stop "
            "naming the token.",
            file=sys.stderr,
        )

    if failed:
        return 1

    scope = len(TOKEN_ONLY)
    pending = f", {len(MIGRATING)} path(s) still migrating" if MIGRATING else ""
    print(
        f"tokens: no retired hex in the tree; {scope} token-only path(s) clean; "
        f"{citations_ok} token citation(s) quote the palette{pending}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
