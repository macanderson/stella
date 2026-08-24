# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Credential half of ``scripts/arena-local.sh`` — resolve, report, then launch.

``scripts/arena-local.sh`` owns the questions a shell answers well (is the
checkout current, should we pull, which interpreter). This module owns the one
it does not: **which values from a dotenv file actually reach which seat**, and
whether the binary about to be measured is fresh enough to be worth paying for.

Both answers are computed with ArenaBench's *own* code — :func:`~.config.load_match`,
:func:`~.config.required_env`, :func:`~.model.screen_env` — rather than a second
implementation that agrees today and drifts by Thursday. The report and the
launch therefore cannot disagree: they are the same function called twice.

WHY A SEAT DOES NOT SIMPLY GET EVERY VARIABLE

Two fences stand between a dotenv file and a contestant's subprocess, and both
are required:

* :func:`~.model.screen_env` allows only *credential-shaped* names — those
  ending ``_API_KEY``, ``_API_TOKEN``, ``_AUTH_TOKEN``, ``_OAUTH_TOKEN``,
  ``_ACCESS_TOKEN``, ``_BASE_URL``, ``_API_BASE``. It allows shapes rather than
  denying names on purpose: ``PYTHONPATH``, ``LD_PRELOAD``, ``STELLA_BINARY``
  and ``DOCKER_HOST`` are each a way to make a seat run something other than the
  benchmark, and a denylist is only ever as current as the last variable
  somebody thought of.
* :func:`~.config.required_env` narrows again to the names *that seat* declared
  (or, undeclared, the ones its provider could use). A Claude Code seat holding
  an OpenRouter key is not a credentialled seat; it is a leaked key.

So "inject both contestants with everything" is refused by design, and this
module's job is to make the refusal legible instead of silent: every name in
the file is placed in exactly one bucket, and the operator reads where it went.
``--inject-all`` exists for the case where that is genuinely wanted anyway; it
widens both seats to every credential-shaped name in the file and says so.

WHY FRESHNESS IS CHECKED HERE AND NOT LEFT TO ARENABENCH

:func:`~.sut.sut_problem_for` already refuses a match whose pinned binary is
missing or unbuildable — but a *stale* binary is neither. It is present, it is
healthy, and it measures code that is no longer the code anyone means by
"Stella today". Spending real provider money to learn how a fortnight-old
commit performs is the failure this check exists to prevent, so the default
ceiling here is deliberately tighter than ArenaBench's own.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

#: How many commits behind ``origin/main`` a SUT binary may be before a paid
#: match is refused. Deliberately far tighter than the freshness checker's own
#: default of 25: that number governs "is this binary plausibly current", while
#: this one governs "is this binary worth spending money to measure".
DEFAULT_MAX_BEHIND = 5

BOLD, DIM, RED, GREEN, YELLOW, RESET = "", "", "", "", "", ""
if sys.stdout.isatty():
    BOLD, DIM = "\033[1m", "\033[2m"
    RED, GREEN, YELLOW, RESET = "\033[31m", "\033[32m", "\033[33m", "\033[0m"


def _warn(message: str) -> None:
    # Flushed first, always: stderr is unbuffered and stdout is not once this
    # is piped, so without it every diagnostic surfaces *above* the report it
    # is about — which reads as a refusal with no stated reason.
    sys.stdout.flush()
    print(f"{YELLOW}warning:{RESET} {message}", file=sys.stderr)


def _die(message: str) -> int:
    sys.stdout.flush()
    print(f"{RED}error:{RESET} {message}", file=sys.stderr)
    return 2


def _plural(count: int, noun: str) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


def arena_home() -> Path:
    """This machine's ArenaBench state directory.

    Honours ``ARENABENCH_HOME`` the same way :func:`~.server.default_workspace`
    and :func:`~.credentials.credentials_path` do, so relocating one relocates
    all three — an operator who moved the workspace did not mean to leave one
    generated file behind in the old place.
    """
    override = os.environ.get("ARENABENCH_HOME")
    return Path(override).expanduser() if override else Path.home() / ".arenabench"


# ---------------------------------------------------------------------------
# the dotenv file
# ---------------------------------------------------------------------------


def read_env_file(path: Path) -> dict[str, str]:
    """Parse a dotenv file with ArenaBench's own forgiving parser.

    Using :func:`~.model.parse_dotenv` rather than ``source``-ing the file in
    the shell is what makes ``export FOO=bar``, quoted values and multi-line
    PEM blobs behave identically here and in the arena's paste box — and it is
    also what keeps a file the operator edits from being able to *run* anything
    on the way in. A shell that sources ``$(rm -rf ~)`` executes it; this parser
    returns it as a string.
    """
    from arenabench.model import parse_dotenv

    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise SystemExit(_die(f"cannot read {path}: {exc}")) from exc
    return parse_dotenv(text)


def partition(env: dict[str, str]) -> tuple[dict[str, str], list[str]]:
    """Split a parsed file into what a seat may ever carry, and what it may not."""
    from arenabench.model import screen_env

    return screen_env(env)


def seed_environment(values: dict[str, str]) -> tuple[list[str], list[str]]:
    """Fill gaps in ``os.environ`` from the file. Returns ``(filled, shadowed)``.

    Gaps only, never overwrites — the same ordering ArenaBench uses everywhere
    else (a value supplied for *this* run beats a stored default), so an
    operator can override one key for one match with a plain ``export`` without
    editing the file.

    ``shadowed`` names the other half of that ordering: a variable already
    exported with a *different* value, where the file will therefore not be
    what the run uses. Reported because the alternative is an operator who
    edits the file, sees no change, and blames the agent. Names only — a value
    is never printed, compared in the clear to anything but its own successor,
    or written anywhere.
    """
    filled: list[str] = []
    shadowed: list[str] = []
    for name, value in sorted(values.items()):
        current = os.environ.get(name)
        if current is None or current == "":
            os.environ[name] = value
            filled.append(name)
        elif current != value:
            shadowed.append(name)
    return filled, shadowed


# ---------------------------------------------------------------------------
# the per-seat report
# ---------------------------------------------------------------------------


def resolve_seats(spec) -> tuple[object, list[str]]:
    """Apply the exact fill layers ``arenabench run`` applies, in its order.

    Three of them, each filling only gaps left by the one above:

    1. the launching process's environment, for the names
       :func:`~.config.required_env` says this seat may hold;
    2. the saved credential set at ``~/.arenabench/credentials.env``;
    3. this machine's own Claude Code login, for a seat that declared the
       subscription token (:func:`~.claude_oauth.apply_local_claude_login`).

    Replaying them here rather than inspecting ``os.environ`` is the whole
    point of this function. A report built from the environment alone is right
    about layer 1 and wrong about the other two, and it is wrong in the
    direction that costs the most: it calls a seat unauthenticated, refuses a
    launch that would have worked, and sends the operator hunting for a
    credential the machine already had. That is not hypothetical — it is what
    the first run of this script did to the Claude Code seat, which is
    authenticated by a local login and holds no environment variable at all.

    Returns the resolved spec and the provenance notes layer 3 emits, which say
    where a credential came from without ever saying what it was.
    """
    from arenabench.claude_oauth import apply_local_claude_login
    from arenabench.config import required_env
    from arenabench.credentials import apply_saved_credentials

    needed = required_env(spec)
    seated = [
        replace(
            contestant,
            env={
                name: os.environ[name]
                for name in needed.get(contestant.id, [])
                if os.environ.get(name)
            },
        )
        for contestant in spec.contestants
    ]
    spec = replace(spec, contestants=tuple(seated))
    spec = apply_saved_credentials(spec)
    return apply_local_claude_login(spec)


def seat_report(spec) -> list[tuple[str, list[str], list[str]]]:
    """Per seat: ``(name, will_carry, declared_but_still_unset)``.

    Reads the resolved ``contestant.env`` produced by :func:`resolve_seats`, so
    it describes the run that is about to happen rather than the template in
    the abstract.
    """
    from arenabench.config import required_env

    needed = required_env(spec)
    rows: list[tuple[str, list[str], list[str]]] = []
    for contestant in spec.contestants:
        candidates = needed.get(contestant.id, [])
        carried = sorted(name for name in candidates if contestant.env.get(name))
        absent = [name for name in candidates if not contestant.env.get(name)]
        rows.append((contestant.name, carried, absent))
    return rows


def widen(spec, names: list[str]):
    """Give every seat every credential-shaped name in the file (``--inject-all``).

    Additive: a seat keeps the names it declared and gains the rest, so a
    template that deliberately withholds a metered key from one arm loses that
    property — which is exactly why this is opt-in and printed loudly rather
    than being the default. The widened set is still fenced by
    :func:`~.model.is_credential_name`, because a declaration is a promise to
    read that variable out of the launching process and ``required = ["PATH"]``
    must never become sayable.
    """
    from arenabench.config import required_env
    from arenabench.model import is_credential_name

    extra = [name for name in names if is_credential_name(name)]
    needed = required_env(spec)
    seats = []
    for contestant in spec.contestants:
        declared = list(needed.get(contestant.id, []))
        merged = declared + [name for name in extra if name not in declared]
        seats.append(replace(contestant, required_env=tuple(merged)))
    return replace(spec, contestants=tuple(seats))


# ---------------------------------------------------------------------------
# SUT freshness
# ---------------------------------------------------------------------------


def _git(repo: Path, *args: str) -> str:
    try:
        out = subprocess.run(
            ["git", *args],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout.strip() if out.returncode == 0 else ""


def sut_freshness(spec, repo: Path) -> list[tuple[str, str, int | None, bool]]:
    """Per Stella seat: ``(seat, commit, commits_behind_origin_main, staged)``.

    ``None`` for the count means the question could not be answered — an
    unresolvable ref, a commit this checkout has never fetched, or no git at
    all. It is reported as unknown rather than as zero, because a freshness
    check that fails open is worse than none: it produces the reassuring number
    without the reassurance.

    ``staged`` rides along because "current" and "present" are independent
    failures that look identical in a one-line summary. A pin can resolve to
    the tip commit and have no binary built for it, which is a green freshness
    verdict on something that cannot run. :func:`~.sut.sut_problem_for` refuses
    that at launch — this column is so the refusal is not a surprise.
    """
    from arenabench.sut import resolve_pin

    rows: list[tuple[str, str, int | None, bool]] = []
    for seat in spec.contestants:
        if seat.agent != "stella":
            continue
        override = getattr(seat, "sut_ref", None)
        ref = getattr(spec, "sut_ref", "") if override is None else override
        if not ref:
            rows.append((seat.name, "(unpinned — ambient binary)", None, False))
            continue
        pin = resolve_pin(ref)
        if not pin.commit:
            rows.append((seat.name, f"{ref} (unresolved)", None, False))
            continue
        count = _git(repo, "rev-list", "--count", f"{pin.commit}..origin/main")
        behind = int(count) if count.isdigit() else None
        rows.append((seat.name, pin.commit[:12], behind, pin.staged is not None))
    return rows


# ---------------------------------------------------------------------------
# entry point
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="arena-local",
        description="Launch an ArenaBench match with credentials from a dotenv file.",
    )
    parser.add_argument("template", type=Path, help="the match TOML to run")
    parser.add_argument(
        "--env-file",
        type=Path,
        default=Path(os.environ.get("ARENA_ENV_FILE", "~/.env.global.local")),
        help="dotenv file to seed credentials from (default: ~/.env.global.local)",
    )
    parser.add_argument(
        "--inject-all",
        action="store_true",
        help="give BOTH seats every credential-shaped name in the file, not "
        "only the ones each declared",
    )
    parser.add_argument(
        "--max-behind",
        type=int,
        default=DEFAULT_MAX_BEHIND,
        help=f"refuse a SUT more than N commits behind origin/main "
        f"(default: {DEFAULT_MAX_BEHIND}; 0 disables the check)",
    )
    parser.add_argument(
        "--allow-stale-sut",
        action="store_true",
        help="launch even when the SUT is past --max-behind (say why, out loud)",
    )
    parser.add_argument(
        "--dry-run",
        "-n",
        action="store_true",
        help="resolve and report everything, launch nothing",
    )
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path.cwd(),
        help="checkout to measure SUT freshness against (default: cwd)",
    )
    # REMAINDER, and therefore last: argparse stops option parsing at the first
    # positional, so every flag this parser owns has to arrive *before*
    # `template` on the command line. `arena-local.sh` builds the argv that
    # way. The trap is silent in the worst way — with the flags after the
    # template, `--dry-run` was swallowed into the passthrough and handed
    # straight to `arenabench run`, which rejected it as an unknown argument
    # from inside a launch that had already printed a clean preflight.
    parser.add_argument(
        "passthrough",
        nargs=argparse.REMAINDER,
        help="arguments forwarded verbatim to `arenabench run` (put them last)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    from arenabench.config import MatchTemplateError, dump_match, load_match

    template = args.template.expanduser()
    if not template.is_file():
        return _die(f"no match template at {template}")

    env_path = args.env_file.expanduser()
    print(f"{BOLD}credentials{RESET}")
    print(f"  file       {env_path}")

    parsed = read_env_file(env_path)
    allowed, refused = partition(parsed)
    filled, shadowed = seed_environment(allowed)

    print(f"  parsed     {_plural(len(parsed), 'variable')}")
    if filled:
        print(f"  {GREEN}exported{RESET}   {', '.join(filled)}")
    if shadowed:
        print(
            f"  {YELLOW}shadowed{RESET}   {', '.join(shadowed)}"
            f"  {DIM}(already exported with a different value — the shell wins){RESET}"
        )
    if refused:
        # Named, never dropped in silence: a credential that quietly did not
        # arrive is indistinguishable from an agent that cannot code.
        print(
            f"  {DIM}not a credential shape, withheld from every seat:{RESET}\n"
            f"    {DIM}{', '.join(refused)}{RESET}"
        )

    try:
        spec = load_match(template)
    except MatchTemplateError as exc:
        for problem in exc.problems:
            print(f"error: {problem}", file=sys.stderr)
        return 2

    launch_template = template
    if args.inject_all:
        spec = widen(spec, sorted(allowed))
        # Written to this machine's ArenaBench state, never beside the source
        # template. A generated file dropped into `matches/` is an untracked
        # file in a tracked directory that outlives the run, gets committed by
        # the next `git add -A`, and then reads as a reviewed match template
        # while being nothing of the sort. It holds no secrets either way —
        # `dump_match` emits names only — but the workspace is where machine
        # state belongs, and it is outside every checkout by construction.
        out_dir = arena_home() / "inject-all"
        out_dir.mkdir(parents=True, exist_ok=True)
        launch_template = out_dir / f"{template.stem}.toml"
        launch_template.write_text(dump_match(spec), encoding="utf-8")
        print(
            f"  {YELLOW}--inject-all{RESET}: every seat widened to "
            f"{_plural(len(allowed), 'credential name')}\n"
            f"    {DIM}running {launch_template} — the committed template is untouched{RESET}"
        )

    spec, oauth_notes = resolve_seats(spec)
    for note in oauth_notes:
        print(f"  {GREEN}login{RESET}      {note}")

    print(f"\n{BOLD}seats{RESET}")
    unauthenticated: list[str] = []
    for name, carried, absent in seat_report(spec):
        mark = f"{GREEN}✔{RESET}" if carried else f"{RED}✗{RESET}"
        print(f"  {mark} {name}")
        print(f"      carries  {', '.join(carried) if carried else '(nothing)'}")
        if absent:
            print(f"      {DIM}declared but unset: {', '.join(absent)}{RESET}")
        if not carried:
            unauthenticated.append(name)

    if unauthenticated:
        # An unauthenticated arm does not error — it scores 0.0, which is
        # indistinguishable from a real loss on a scoreboard. Refusing here is
        # cheaper than reading that number later and believing it.
        return _die(
            "these seats would launch with no credential and score a silent "
            "0.0:\n  " + "\n  ".join(unauthenticated)
        )

    print(f"\n{BOLD}system under test{RESET}")
    stale: list[str] = []
    for name, commit, behind, staged in sut_freshness(spec, args.repo.expanduser()):
        note = "" if staged else f"  {YELLOW}no binary staged for this commit{RESET}"
        if behind is None:
            print(f"  {name}  {commit}  {DIM}(freshness unknown){RESET}{note}")
            continue
        drift = _plural(behind, "commit") + " behind origin/main"
        if args.max_behind and behind > args.max_behind:
            print(f"  {RED}✗{RESET} {name}  {commit}  {RED}{drift}{RESET}{note}")
            stale.append(f"{name}: {commit} is {drift}")
        else:
            print(f"  {GREEN}✔{RESET} {name}  {commit}  {drift}{note}")

    if stale and not args.allow_stale_sut:
        return _die(
            "refusing to spend real money measuring stale code:\n  "
            + "\n  ".join(stale)
            + "\n  Build from tip first, or pass --allow-stale-sut and say why."
        )
    if stale:
        _warn("launching against a stale SUT because --allow-stale-sut was passed")

    forwarded = [a for a in args.passthrough if a != "--"]
    command = [sys.executable, "-m", "arenabench", "run", str(launch_template), *forwarded]

    if args.dry_run:
        print(f"\n{DIM}--dry-run: would launch{RESET}  {' '.join(command)}")
        return 0

    print(f"\n{BOLD}launching{RESET}  {' '.join(command)}\n")
    return subprocess.call(command)


if __name__ == "__main__":
    raise SystemExit(main())
