#!/usr/bin/env python3
"""Guard: no I/O in `stella-core`, read from the source rather than assumed.

AGENTS.md rule 2 says the engine does no I/O: anything that spawns a
process, reads a file, or hits the network belongs in an adapter crate and
reaches the engine as a port. The crate's own manifest says the same, and so
does the header of every module that takes a `Sleeper` or a `Clock`. Nothing
checked it. On 2026-09-09 an audit found the retry ladder drawing its jitter
from the OS entropy pool through `rand::rng()`, and the hook bus stamping
every event from `SystemTime::now()`, while both files' headers said the
crate reads nothing directly. A rule that only prose enforces is met until
somebody reads the prose.

## Three questions, one run

**The floor.** No shipping source in the crate may name a filesystem,
process, network, environment, entropy, wall-clock, or standard-stream API.
Absolute: one hit fails, and there is no baseline, because the tree has
none. `#[cfg(test)]` bodies, `tests/` directories, and `tests.rs` files are
stripped first — a test may read a fixture; the engine may not.

**The manifest.** `[dependencies]` may not name a crate whose purpose is I/O
or entropy, and `tokio` may take only the features that schedule (`sync`,
`macros`, `rt`) — not `time`, because the engine waits through
`retry::Sleeper` and never owns a timer. `[dev-dependencies]` is exempt for
the same reason `#[cfg(test)]` is.

**The ratchet.** Reads of the monotonic clock — `Instant::now()` — are not
I/O. They are ambient state all the same: a turn that reads the clock itself
cannot be replayed from its record, which is why `Sleeper::now` exists. Each
file's count is recorded in `scripts/core-no-io-baseline.txt` and may only
go down. The baseline reached empty on 2026-09-10, so every read now fails;
the ratchet stays so a read that lands reports as the count it is.
`--update` refuses to add a file or raise a count, so a red run is cleared
by reading the clock once through the port and passing `now` in, never by
recording the read. `.elapsed()` is the same read in disguise and sits on
the floor.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-core-no-io.py [ROOT]
    ./scripts/check-core-no-io.py --update [ROOT]   # shrink the baseline
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

CRATE = "crates/stella-core"
BASELINE = "scripts/core-no-io-baseline.txt"

# The strippers are the sibling guard's. That way both guards agree on what
# counts as shipping code.
_SIBLING = Path(__file__).with_name("check-core-reachability.py")
_spec = importlib.util.spec_from_file_location("core_reachability", _SIBLING)
if _spec is None or _spec.loader is None:
    sys.exit(f"check-core-no-io: cannot load {_SIBLING}")
_sibling = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sibling)
strip_comments = _sibling.strip_comments
strip_cfg_test = _sibling.strip_cfg_test

# The floor: a label, a pattern, and what to do instead. Comments, strings
# and test bodies are blanked first. So a pattern named in a doc comment, as
# this file's own header does, is not a hit.
FLOOR: list[tuple[str, re.Pattern[str], str]] = [
    (
        "std::fs",
        re.compile(r"\bstd\s*::\s*fs\b|\buse\s+std\s*::\s*\{[^}]*\bfs\b"),
        "the filesystem is a port — take a trait here, implement it in stella-tools or stella-cli",
    ),
    (
        "std::process",
        re.compile(r"\bstd\s*::\s*process\b|\buse\s+std\s*::\s*\{[^}]*\bprocess\b"),
        "process spawning belongs to stella-tools; the engine asks a ToolExecutor",
    ),
    (
        "std::net",
        re.compile(r"\bstd\s*::\s*net\b|\buse\s+std\s*::\s*\{[^}]*\bnet\b"),
        "the network is a Provider or an MCP adapter, never the engine's own socket",
    ),
    (
        "std::env",
        re.compile(r"\bstd\s*::\s*env\b|\buse\s+std\s*::\s*\{[^}]*\benv\b"),
        "the environment is read once by the binary and handed in as config",
    ),
    (
        "standard streams",
        re.compile(
            r"\bstd\s*::\s*io\s*::\s*std(?:in|out|err)\b"
            r"|\bio\s*::\s*std(?:in|out|err)\s*\("
            r"|\bstd(?:in|out|err)\s*\(\s*\)"
        ),
        "the engine talks through AgentEvent, not a terminal",
    ),
    (
        "tokio I/O",
        re.compile(r"\btokio\s*::\s*(?:fs|process|net|io)\b"),
        "tokio's I/O drivers belong to the host that owns the runtime",
    ),
    (
        "SystemTime::now",
        re.compile(r"\bSystemTime\s*::\s*now\b"),
        "take a ports::Clock counting from the Unix epoch, the way bus::HookBus does",
    ),
    (
        "a blocking sleep",
        re.compile(r"\bstd\s*::\s*thread\s*::\s*sleep\b|\bthread\s*::\s*sleep\s*\("),
        "the engine suspends through retry::Sleeper, never a thread",
    ),
    (
        "tokio's timer",
        re.compile(r"\btokio\s*::\s*time\b"),
        "the engine waits through retry::Sleeper — retry::bounded is the timeout, sleep is the sleep",
    ),
    (
        "Instant::elapsed",
        re.compile(r"\.\s*elapsed\s*\(\s*\)"),
        "a hidden Instant::now(): subtract two readings of Sleeper::now instead",
    ),
    (
        "an entropy source",
        re.compile(r"\brand\s*::\s*rng\b|\bthread_rng\b|\bOsRng\b|\bgetrandom\b"),
        "a draw is a port — retry::Sleeper::jitter is the one the backoff uses",
    ),
    (
        "a print macro",
        re.compile(r"\b(?:e?print(?:ln)?|dbg)\s*!"),
        "the engine reports through AgentEvent; a print reaches nothing a host reads",
    ),
]

# The ratchet: clock reads, counted per file.
INSTANT_NOW = re.compile(r"\bInstant\s*::\s*now\s*\(")

# The manifest: crates built for I/O or entropy, and the tokio features that
# do no I/O. `rand` heads the list. It is the one this guard was written over.
DENIED_DEPS = {
    "rand",
    "rand_core",
    "getrandom",
    "reqwest",
    "hyper",
    "hyper-util",
    "ureq",
    "rusqlite",
    "sqlx",
    "dirs",
    "home",
    "walkdir",
    "ignore",
    "notify",
    "tempfile",
    "libc",
    "nix",
    "mio",
    "socket2",
    "which",
    "git2",
}
TOKIO_FEATURES_ALLOWED = {"sync", "macros", "rt"}

SECTION = re.compile(r"^\s*\[([^\]]+)\]\s*$")
DEP_KEY = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*(?:\.\s*workspace\s*)?=")
FEATURES = re.compile(r"features\s*=\s*\[([^\]]*)\]")


def shipping_sources(src: Path) -> list[Path]:
    """Every `.rs` file under `src/` that is not a test file.

    A `tests/` directory anywhere under `src/`, and a file named `tests.rs`,
    are the two spellings this workspace uses for a module's tests; both are
    compiled only under `cfg(test)` by the `mod` line that names them, and
    the sibling guard skips them the same way.
    """
    out: list[Path] = []
    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(src)
        if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
            continue
        out.append(path)
    return out


def shipping_text(path: Path) -> str:
    return strip_cfg_test(strip_comments(path.read_text(encoding="utf-8", errors="replace")))


def line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def floor_hits(root: Path, src: Path) -> list[str]:
    """Every shipping line that names an I/O surface, with its remedy."""
    hits: list[str] = []
    for path in shipping_sources(src):
        text = shipping_text(path)
        rel = path.relative_to(root)
        for label, pattern, remedy in FLOOR:
            for match in pattern.finditer(text):
                hits.append(f"  {rel}:{line_of(text, match.start())}: {label} — {remedy}")
    return hits


def clock_reads(root: Path, src: Path) -> dict[str, int]:
    """`Instant::now()` reads per shipping file, files with none omitted."""
    counts: dict[str, int] = {}
    for path in shipping_sources(src):
        n = len(INSTANT_NOW.findall(shipping_text(path)))
        if n:
            counts[str(path.relative_to(root))] = n
    return counts


def manifest_hits(manifest: Path) -> list[str]:
    """Denied crates and tokio I/O features in `[dependencies]`."""
    if not manifest.is_file():
        return [f"  {manifest}: missing — nothing declares what this crate links"]
    hits: list[str] = []
    section = ""
    for lineno, line in enumerate(manifest.read_text(encoding="utf-8").splitlines(), 1):
        header = SECTION.match(line)
        if header:
            section = header.group(1).strip()
            # `[dependencies.tokio]`, the table form of one dependency.
            if section.startswith("dependencies."):
                name = section[len("dependencies.") :]
                if name in DENIED_DEPS:
                    hits.append(f"  Cargo.toml:{lineno}: `{name}` in [dependencies]")
                section = f"dependencies.{name}"
            continue
        if section == "dependencies":
            key = DEP_KEY.match(line)
            if not key:
                continue
            name = key.group(1)
            if name in DENIED_DEPS:
                hits.append(f"  Cargo.toml:{lineno}: `{name}` in [dependencies]")
            if name == "tokio":
                hits.extend(tokio_feature_hits(line, lineno))
        elif section == "dependencies.tokio":
            hits.extend(tokio_feature_hits(line, lineno))
    return hits


def tokio_feature_hits(line: str, lineno: int) -> list[str]:
    features = FEATURES.search(line)
    if not features:
        return []
    named = {f.strip().strip("\"'") for f in features.group(1).split(",") if f.strip()}
    extra = sorted(named - TOKIO_FEATURES_ALLOWED)
    return [
        f"  Cargo.toml:{lineno}: tokio feature `{f}` — only "
        f"{', '.join(sorted(TOKIO_FEATURES_ALLOWED))} schedule without I/O"
        for f in extra
    ]


def read_baseline(path: Path) -> dict[str, int]:
    """`<count> <path>` per line, comments and blanks skipped."""
    if not path.is_file():
        return {}
    out: dict[str, int] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        count, _, rel = line.partition(" ")
        out[rel.strip()] = int(count)
    return out


BASELINE_HEADER = """\
# `Instant::now()` reads in stella-core's shipping source, per file.
#
# A DOWN-ONLY ratchet. It records the clock reads that predate the guard. It
# may never gain a file or a larger count, and `--update` refuses both. To
# clear a red run, read the clock through `Sleeper::now` and pass `now` in.
# Never record the read. The list reached empty on 2026-09-10 and stays so.
#
# Regenerate after removing a read:  ./scripts/check-core-no-io.py --update
"""


def write_baseline(path: Path, counts: dict[str, int]) -> None:
    body = "".join(f"{n} {rel}\n" for rel, n in sorted(counts.items()))
    path.write_text(BASELINE_HEADER + body, encoding="utf-8")


def main() -> int:
    args = [a for a in sys.argv[1:] if a != "--update"]
    update = "--update" in sys.argv[1:]
    root = Path(args[0] if args else ".").resolve()
    src = root / CRATE / "src"
    baseline_path = root / BASELINE

    if not src.is_dir():
        print(f"check-core-no-io: no {CRATE}/src at {root} — nothing to check")
        return 0

    floor = floor_hits(root, src)
    manifest = manifest_hits(root / CRATE / "Cargo.toml")
    current = clock_reads(root, src)
    baseline = read_baseline(baseline_path)

    failed = False
    if floor:
        failed = True
        print("check-core-no-io: FAILED — I/O in the engine\n")
        print("These shipping lines in stella-core reach outside the process:\n")
        print("\n".join(floor))
        print(
            "\nAGENTS.md rule 2: the engine does no I/O. Anything that reads a\n"
            "file, spawns a process, hits the network, or draws from an ambient\n"
            "source is a port, implemented outside stella-core and handed in.\n"
            "There is no baseline for this — the tree has none, and gains none.\n"
        )
    if manifest:
        failed = True
        print("check-core-no-io: FAILED — an I/O crate in the manifest\n")
        print("These [dependencies] of stella-core link an I/O or entropy source:\n")
        print("\n".join(manifest))
        print(
            "\nA test that needs one lists it under [dev-dependencies]. Shipping\n"
            "code takes the capability as a port.\n"
        )

    if update:
        grew = sorted(
            rel for rel, n in current.items() if rel not in baseline or n > baseline[rel]
        )
        if grew:
            print("check-core-no-io: REFUSING to grow the baseline.\n")
            print("These files read Instant::now() more than the baseline records:")
            for rel in grew:
                print(f"  {rel}: {current[rel]} (baseline {baseline.get(rel, 0)})")
            print(
                "\nThe ratchet only goes down. Read the clock once at the edge and\n"
                "pass `now` in, or take ports::Clock — do not record the read here."
            )
            return 1
        if failed:
            return 1
        write_baseline(baseline_path, current)
        for rel in sorted(set(baseline) - set(current)):
            print(f"check-core-no-io: retired {rel} — no clock reads left")
        for rel in sorted(current):
            if current[rel] < baseline.get(rel, current[rel]):
                print(f"check-core-no-io: lowered {rel} to {current[rel]}")
        print(f"check-core-no-io: baseline holds {sum(current.values())} read(s) in {len(current)} file(s)")
        return 0

    grew = sorted(rel for rel, n in current.items() if n > baseline.get(rel, 0))
    if grew:
        failed = True
        print("check-core-no-io: FAILED — more clock reads than the baseline\n")
        for rel in grew:
            print(f"  {rel}: {current[rel]} Instant::now() read(s), baseline {baseline.get(rel, 0)}")
        print(
            "\nThe monotonic clock is ambient state: a turn that reads it cannot be\n"
            "replayed from its record. Read it once at the edge and pass `now` in,\n"
            "or take ports::Clock. Do NOT add a baseline entry — the ratchet only\n"
            "goes down.\n"
        )

    stale = sorted(
        rel for rel, n in baseline.items() if current.get(rel, 0) < n
    )
    if stale:
        failed = True
        print("check-core-no-io: baseline is STALE\n")
        for rel in stale:
            print(f"  {rel}: baseline {baseline[rel]}, now {current.get(rel, 0)}")
        print(
            "\nSomebody removed a clock read and left the ceiling where it was. Run\n"
            "`make core-no-io-update` to lower it; the ratchet never goes back up.\n"
        )

    if failed:
        return 1

    reads = sum(current.values())
    print(
        f"check-core-no-io: OK — {len(shipping_sources(src))} shipping file(s) name no I/O; "
        f"{reads} Instant::now() read(s) in {len(current)} file(s), at or under the baseline"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
