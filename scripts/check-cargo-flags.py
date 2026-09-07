#!/usr/bin/env python3
"""Guard: no Makefile recipe hands cargo a flag pair cargo refuses.

For seven hours the first line of `doc-warnings-schema` read
`cargo clean --doc $(SCHEMA_CRATES)`. Cargo turns that pair down at once:

    $ cargo clean --doc -p stella-protocol
    error: --doc cannot be used with -p

The recipe died on that line every time. The whole step was dead with it.

No check saw it, and that is the part worth fixing. One workflow runs
`doc-warnings-schema`, and that is `wire-schema.yml`. Its `paths:` list asks
one thing: could this diff have made the files under `docs/wire/` stale? A
`Makefile` edit cannot. So the `Makefile` is not on that list, and it must
not go on it. The pre-push hook picks its rung from the same list, through
`scripts/wire-schema-paths.sh`, and a wider list would send the hook up a
rung for no reason. The upshot: the one runner of the recipe never sees the
recipe change. The diff that broke this line touched the `Makefile` and
nothing else. Its own run did not execute a word of what it had rewritten,
and it merged green.

This shuts the hole from the other side. It needs no toolchain, so it sits at
`guards-fast`. It runs in the job in `guard-self-tests.yml` that has no
`paths:` filter, which fires on every pull request.

What it checks: every cargo command in every Makefile recipe, against a table
of flag pairs cargo refuses. Each row quotes what cargo said, on the
toolchain `rust-toolchain.toml` pins.

What it does not check: whether a recipe that runs does the right thing. A
flag pair cargo takes and a command that works are two claims. Only the first
can be settled with no toolchain.

Make does the expanding. Recipes reach cargo through variables, and
`$(SCHEMA_CRATES)` is how the refused `-p` got in, so this asks
`make --dry-run` for the commands instead of expanding them again here. A
second expander is a second thing to keep right, and the bug lives in that
very gap.

Usage:

    ./scripts/check-cargo-flags.py [--makefile PATH]

`--makefile` aims the run at another tree. That is what lets
`scripts/test-cargo-flags.py` build fixture Makefiles and watch this guard
fail on them.
"""

from __future__ import annotations

import argparse
import os
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

# ── The table ────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Refusal:
    """One flag combination cargo rejects, and the words it rejects it with."""

    subcommand: str
    requires: tuple[str, ...]
    forbids: tuple[str, ...]
    cargo_says: str
    remedy: str


# Add a row only after you run the pair and read the error. A made-up row
# bans a spelling cargo may well take. That costs an author a day.
REFUSALS = (
    Refusal(
        subcommand="clean",
        requires=("--doc",),
        forbids=("-p", "--package"),
        cargo_says="--doc cannot be used with -p",
        remedy=(
            "Drop the package filter. `cargo clean --doc` takes the whole "
            "doc tree of one target directory, so scope it with "
            "CARGO_TARGET_DIR instead."
        ),
    ),
)

# Words that can sit in front of a command without being the command. Shell
# keywords, plus the `VAR=value` form the recipes here use.
KEYWORDS = frozenset(
    {"!", "(", "{", "do", "elif", "else", "exec", "if", "then", "time", "while"}
)

ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")

# A target line is a name and a colon. No `=` may follow the colon, since
# `:=` and `::=` set a variable. A leading dot marks a directive.
TARGET_LINE = re.compile(r"^(?P<name>[A-Za-z0-9_][A-Za-z0-9_.-]*)\s*:(?![=:])")


class GuardError(Exception):
    """A condition that stops the parse rather than narrowing it."""


# ── Reading the Makefile ─────────────────────────────────────────────────────


def targets_running_cargo(makefile: Path) -> list[str]:
    """Every target whose recipe mentions cargo, in file order.

    A textual scan, because it only has to find candidate names -- make
    expands them below, and a target whose recipe turns out to run no cargo
    costs one printed line and nothing else.
    """
    found: list[str] = []
    current: str | None = None
    mentions_cargo = False

    def close() -> None:
        nonlocal current, mentions_cargo
        if current is not None and mentions_cargo and current not in found:
            found.append(current)
        current = None
        mentions_cargo = False

    for raw in makefile.read_text(encoding="utf-8").splitlines():
        if raw.startswith("\t"):
            if "cargo" in raw:
                mentions_cargo = True
            continue
        close()
        match = TARGET_LINE.match(raw)
        if match:
            current = match.group("name")
    close()
    return found


def expand(makefile: Path, targets: list[str]) -> str:
    """The recipe lines make would run, with every variable already expanded.

    One invocation for every target rather than one each: make parses the file
    once, and the file's `$(shell ...)` variables run once with it.
    """
    if not targets:
        return ""
    env = dict(os.environ)
    # A parent make exports its own flags. A jobserver is noise for a run
    # that only prints.
    env.pop("MAKEFLAGS", None)
    env.pop("MFLAGS", None)
    env.pop("MAKELEVEL", None)
    result = subprocess.run(
        ["make", "-f", makefile.name, "--dry-run", "--no-print-directory", *targets],
        cwd=makefile.parent,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise GuardError(
            f"`make --dry-run` failed for {makefile}:\n{result.stderr.strip()}"
        )
    return result.stdout


def logical_lines(printed: str) -> list[str]:
    """Recipe lines, with backslash continuations joined back together."""
    joined: list[str] = []
    pending = ""
    for line in printed.splitlines():
        pending += line
        if pending.endswith("\\"):
            pending = pending[:-1] + " "
            continue
        joined.append(pending.replace("\t", " "))
        pending = ""
    if pending:
        joined.append(pending.replace("\t", " "))
    return joined


# ── Reading a command out of a line ──────────────────────────────────────────


def simple_commands(line: str) -> list[str]:
    """Split a recipe line on the shell operators that end a command.

    Quotes are honoured, so an operator inside a quoted string stays part of
    it. Only `;`, `|`, `&` and their doubled forms separate; a redirection
    does not, and neither does anything inside `$(...)`, which is left whole
    because a command substitution running cargo is still a command this
    reads.
    """
    parts: list[str] = []
    buffer: list[str] = []
    quote: str | None = None
    index = 0
    while index < len(line):
        char = line[index]
        if quote is not None:
            buffer.append(char)
            if char == "\\" and quote == '"' and index + 1 < len(line):
                index += 1
                buffer.append(line[index])
            elif char == quote:
                quote = None
            index += 1
            continue
        if char in ("'", '"'):
            quote = char
            buffer.append(char)
            index += 1
            continue
        if char == "\\" and index + 1 < len(line):
            buffer.append(char)
            index += 1
            buffer.append(line[index])
            index += 1
            continue
        if char in (";", "|", "&"):
            parts.append("".join(buffer))
            buffer = []
            index += 1
            if index < len(line) and line[index] == char:
                index += 1
            continue
        buffer.append(char)
        index += 1
    parts.append("".join(buffer))
    return [part for part in parts if part.strip()]


def cargo_argv(command: str) -> list[str] | None:
    """The cargo invocation in this command, or None when there is not one.

    Leading environment assignments and shell keywords are stepped over. A
    command that merely names cargo in an argument -- a `printf` telling
    somebody to run `cargo install cargo-deny`, a script called
    `check-cargo-install-pins.sh` -- is not one.
    """
    if "cargo" not in command:
        return None
    try:
        tokens = shlex.split(command)
    except ValueError as error:
        raise GuardError(
            f"could not read this recipe command as shell words: {command!r}\n"
            f"  {error}\n"
            "  Teach this parse the shape rather than letting it skip a line: "
            "a skipped line is an unchecked cargo invocation."
        ) from error
    while tokens and (tokens[0] in KEYWORDS or ASSIGNMENT.match(tokens[0])):
        tokens.pop(0)
    if not tokens or Path(tokens[0]).name != "cargo":
        return None
    return tokens


def subcommand_and_flags(argv: list[str]) -> tuple[str, list[str]]:
    """The cargo subcommand, and the flags cargo itself is asked to read.

    Everything after a bare `--` belongs to the tool cargo runs -- the
    `-- -D warnings` every clippy recipe ends with -- so the scan stops there.
    """
    rest = argv[1:]
    while rest and rest[0].startswith("+"):
        rest.pop(0)
    if not rest:
        return "", []
    subcommand = ""
    flags: list[str] = []
    for token in rest:
        if token == "--":
            break
        if token.startswith("-"):
            flags.append(token.split("=", 1)[0])
        elif not subcommand:
            subcommand = token
    return subcommand, flags


# ── The verdict ──────────────────────────────────────────────────────────────


def offences(command: str) -> list[tuple[Refusal, str]]:
    argv = cargo_argv(command)
    if argv is None:
        return []
    subcommand, flags = subcommand_and_flags(argv)
    if not subcommand:
        return []
    hits: list[tuple[Refusal, str]] = []
    for refusal in REFUSALS:
        if refusal.subcommand != subcommand:
            continue
        if not all(flag in flags for flag in refusal.requires):
            continue
        clash = next((flag for flag in refusal.forbids if flag in flags), None)
        if clash is not None:
            hits.append((refusal, clash))
    return hits


def check(makefile: Path) -> int:
    targets = targets_running_cargo(makefile)
    printed = expand(makefile, targets)
    failures: list[str] = []
    checked = 0
    for line in logical_lines(printed):
        for command in simple_commands(line):
            if cargo_argv(command) is None:
                continue
            checked += 1
            for refusal, clash in offences(command):
                failures.append(
                    f"FAIL — cargo refuses this recipe command:\n"
                    f"       {command.strip()}\n"
                    f"       cargo says: error: {refusal.cargo_says}\n"
                    f"       `{' '.join(refusal.requires)}` cannot be used "
                    f"with `{clash}` on `cargo {refusal.subcommand}`.\n"
                    f"       {refusal.remedy}"
                )

    for failure in failures:
        print(f"check-cargo-flags: {failure}", file=sys.stderr)
    if failures:
        print(
            "check-cargo-flags: a recipe that dies on its first line is a gate "
            "step that never ran.",
            file=sys.stderr,
        )
        return 1
    print(
        f"check-cargo-flags: OK — {checked} cargo invocations across "
        f"{len(targets)} recipes, none carrying a flag pair cargo refuses."
    )
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--makefile",
        default=str(Path(__file__).resolve().parent.parent / "Makefile"),
        help="the Makefile to read (default: this repository's).",
    )
    args = parser.parse_args(argv)
    makefile = Path(args.makefile).resolve()
    if not makefile.is_file():
        print(f"check-cargo-flags: {makefile} does not exist.", file=sys.stderr)
        return 2
    try:
        return check(makefile)
    except GuardError as error:
        print(f"check-cargo-flags: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
