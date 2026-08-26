#!/usr/bin/env python3
"""Guard the colour system: no retired hex anywhere, no stray hex where it matters.

Four checks, different in strength, because "never use this colour
again", "use only tokens here", "quote this token correctly" and "something
renders this" are different promises and only one of them can be made about the
whole tree today.

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

4. **Paint.** Every token declares a `paint` posture in the token JSON, and this
   is what holds the declaration to the tree, in **both** directions: a
   `"painted"` token with no site fails, and a `{ "gap": "#N" }` token with a
   site fails too. See `paint_report` for what the sweep can and cannot see.

   Checks 1-3 are all about a *value*; none of them can ask whether anything
   renders it. That is how `comment` survived (#4946): a published token with a
   role sentence, a `--st-comment` declaration on four surfaces, a `COMMENT`
   constant in the generated `token.rs`, three design renderings using its hex
   and a row in `scripts/contrast-baseline.txt` -- so one of the sub-AA pairings
   #4063 was deciding about was a colour no reader had ever seen. Every check
   above passed it, because every value involved was live.

   This is AGENTS.md rule #10 pointed at the palette, and
   `crates/stella-protocol/src/event/consumers.rs` is the shape it copies. The
   difference is what enforces totality: `consumers.rs` gets it from the
   compiler over an enum, and a colour's consumers are spread across Rust, four
   stylesheets, HTML and SVG, so "is it painted?" is a text question. The door
   is `scripts/gen-tokens.py`'s `check_paint`, which refuses a token with no
   posture; this is the half that refuses a posture that is not true.

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


# Files whose reference to a token cannot tell a painted one from an unpainted
# one, so check 4 does not read them.
#
# The generated artifacts are here for the obvious reason: they are emitted from
# the token table, so they name every token by construction and would make the
# sweep vacuously green.
#
# `fallback.rs` is here for a subtler version of the same reason, and it is the
# entry that decides whether this check means anything. `ansi16` is **total over
# `token::ALL`** -- `every_token_has_a_fallback` is the test that keeps it that
# way -- so it names all twenty terminal tokens whether or not a cell ever takes
# one. Counting it would have passed six tokens nothing paints, four of them
# under values the deck does not even use (`theme::DIFF_ADD_BG` is `#1B2921`
# where `token::DIFF_ADD_BG` is `#10201A`).
#
# An **alias** is NOT excluded: `pub const TEXT_EMPHASIS: Color =
# token::SILVER_TYPE;` counts as a site. Resolving an alias down to a cell needs
# transitive analysis this sweep does not do, and the claim it makes is
# correspondingly bounded -- see `paint_report`.
PAINT_BLIND = (
    "design/tokens/stella-tokens.json",
    "design/tokens/stella-tokens.css",
    "crates/stella-tui-theme/src/token.rs",
    "crates/stella-tui-theme/src/fallback.rs",
    "scripts/gen-tokens.py",
    "scripts/check-tokens.py",
)

# A token being *used*, in the two notations this tree writes. A declaration --
# `--st-bg: #0A0A0C` in a mirrored stylesheet -- is not one of them:
# `comment` had four of those and no reader ever saw the colour.
VAR_USE = re.compile(r"var\(\s*(--st-[a-z0-9-]+)")
RUST_USE = re.compile(r"\btoken::([A-Z][A-Z0-9_]*)\b")

# How many example sites a failure prints before it stops.
PAINT_EXAMPLES = 3


def paint_report(root: Path, doc: dict, files: list[Path]) -> list[str]:
    """Hold every token's `paint` posture to the tree. Returns the failures.

    **What this sees.** One `var(--st-<name>)` or `token::<RUST>` outside
    `PAINT_BLIND`, anywhere in a tracked text file. That is a NAME sweep, not a
    render trace. A guard whose reach is guessed at is a guard that gets
    trusted past it, so the bound is:

    - It cannot follow a **value copied under another name**. The site ships
      every one of the eleven web-only stops as a literal under `--stella-*`
      rather than through `var(--st-*)`, so they read as gaps here and are
      declared as gaps -- with #4978, where completing those sheets is being
      decided, as the citation. The posture records the mechanism, not the
      pixel.
    - It cannot follow an **alias chain** to a cell, so an alias counts as a
      site (see `PAINT_BLIND`).
    - It does not care whether the site is production or a test. No token in
      this tree is named only by tests, so no verdict here turns on that; if one
      ever is, this is the sentence that has to change rather than a number.
    - **Prose counts.** A doc comment writing `token::DIFF_ADD_BG` is a site as
      far as this is concerned. A scanner that reads source cannot tell a
      mention from a use without being taught to, and #4986 is the same hazard
      on another guard: `embedder_backend_sealed_cli.rs` counted a module doc
      naming `CARGO_BIN_EXE_stella` as a spawn site and failed a correctly
      sealed file with a billing-leak message. Read that issue before
      "fixing" this line -- it is the shape, and the reasoning there applies
      here unchanged.

      The asymmetry is what makes it tolerable rather than a defect to fix
      now. A stray mention makes `painted` fractionally easier to satisfy,
      which is quiet; it makes a real `gap` read as stale, which is loud, names
      the file and line, and is cleared by moving the sentence or declaring the
      token painted. That direction is how this guard found a scratch file on
      its own PR (#4996). A structural fix means parsing Rust and CSS, which is
      a language server rather than a guard.

    **Which files.** `tracked_files` -- `git ls-files --cached --others
    --exclude-standard` -- so an un-ignored file in a working tree counts even
    before it is committed. Deliberate, and the same enumeration the three
    checks above take since #4960: a file that is neither ignored nor committed
    is still a file a `git add -A` will publish, and this check found exactly
    that case on its own PR. The cost is that a developer's un-ignored scratch
    note mentioning a token can turn their local run red; the failure names the
    path, and the answer is an ignore rule.

    What it does answer is exactly the question `comment` failed: does any
    hand-written surface name this token at all?
    """
    used: dict[str, list[str]] = {}
    for rel in files:
        posix = rel.as_posix()
        if rel.suffix not in TEXT_SUFFIXES or posix in PAINT_BLIND:
            continue
        try:
            text = (root / rel).read_text(errors="ignore")
        except OSError:
            continue
        for lineno, line in enumerate(text.splitlines(), 1):
            for match in VAR_USE.finditer(line):
                used.setdefault(match.group(1), []).append(f"{posix}:{lineno}")
            for match in RUST_USE.finditer(line):
                used.setdefault(match.group(1), []).append(f"{posix}:{lineno}")

    failures: list[str] = []
    for tok in doc["tokens"]:
        sites = list(used.get(tok["css"], []))
        if tok.get("rust"):
            sites += used.get(tok["rust"], [])
        paint = tok.get("paint")
        if paint == "painted" and not sites:
            failures.append(
                f"{tok['name']}: declares `painted`, and nothing in the tree "
                f"names {tok['css']} or token::{tok.get('rust', '-')}. Paint it, "
                f"retire it, or declare the gap with the issue where that is "
                f"being decided"
            )
        elif isinstance(paint, dict) and sites:
            shown = ", ".join(sites[:PAINT_EXAMPLES])
            more = f" (+{len(sites) - PAINT_EXAMPLES} more)" if len(sites) > PAINT_EXAMPLES else ""
            failures.append(
                f"{tok['name']}: declares a gap citing {paint['gap']}, but "
                f"{len(sites)} site(s) name it: {shown}{more}. A gap that has "
                f"been closed is redeclared as `painted`, not left standing"
            )
    return failures


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

    files = tracked_files(root)
    paint_hits = paint_report(root, doc, files)

    for rel in files:
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
    if paint_hits:
        failed = True
        print(
            f"\n{len(paint_hits)} token(s) declare a `paint` posture the tree "
            "does not bear out:\n",
            file=sys.stderr,
        )
        for hit in paint_hits:
            print(f"  {hit}", file=sys.stderr)
        print(
            "\n`paint` is declared per token in design/tokens/stella-tokens.json "
            "and is the answer to \"what renders this?\" — the question nothing "
            "asked when `comment` shipped unpainted (#4975).",
            file=sys.stderr,
        )

    if failed:
        return 1

    scope = len(TOKEN_ONLY)
    pending = f", {len(MIGRATING)} path(s) still migrating" if MIGRATING else ""
    painted = sum(1 for t in doc["tokens"] if t.get("paint") == "painted")
    gaps = len(doc["tokens"]) - painted
    print(
        f"tokens: no retired hex in the tree; {scope} token-only path(s) clean; "
        f"{citations_ok} token citation(s) quote the palette; {painted} token(s) "
        f"painted, {gaps} declared gap(s){pending}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
