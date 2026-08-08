"""Every caller of this adapter must still bind to it — pin the call shape.

Two layers, in one module because they are one subject read at two altitudes.

**The seam (#2182).** The adapter's host-side selector entry points
(`_benchmark_engine_posture`, `_benchmark_assurance_tiers`) were renamed
`witness_author` → `verifier`; `make_manifest.py` was left calling them with
`witness_author=`. That is a `TypeError`, raised inline while the manifest dict
is being assembled, under a `try/except` that catches `ImportError` only — so
the run host wrote **no manifest at all**, and a benchmark run measured while
that held cannot be interpreted afterwards.

**Why the witness has to live here and not beside its subject.** The evidence
suite runs `uv run --with pytest --no-project`, so `stella_harbor` is not
importable there, so the `except ImportError` early-return is the only branch
its tests ever take: the defective line is unreachable in the one suite that
owns the file. The adapter is importable exactly and only where the crash
happens — the run host, and this suite. The guard that makes `make_manifest.py`
testable off-host is the same guard that hides the defect, so a check that
reads the *source* from the side that holds the live signature is the only
placement that can fail.

Two questions, deliberately separate:

- **Does the call bind?** Read the keyword names `make_manifest.py` passes out
  of its AST, compare them against `inspect.signature` of the live callee. This
  fails if either side is renamed without the other, which no test in either
  suite could see before.
- **Does it still bind to the right thing?** A crash is also "fixable" by
  dropping the selector, which would silently declare the control arm's posture
  and tier hashes for a treatment run — a mislabelled paid run, strictly worse
  than the crash. So the selector is asserted *present*, derived from the
  adapter rather than spelled as a literal here.

The shape is copied from `test_posture.py::TestOutputCeilingParity` and
`test_assurance_tiers.py::test_the_engine_still_resolves_a_role_in_the_order_this_module_mirrors`:
a check whose two halves live in different suites reads one of them as source
text, and says which literal to edit when the path moves.

**The class (#2203).** Everything above names its two seams as literals — a
`_CALLEES` dict and one directory — which is the same ratchet #2182 was, one
notch tighter. The next rename strands the *third* caller, and it fails the
same way: silent in every suite, loud only on the run host, mid-paid-run.

So the second half of this module derives its subject from the repository
instead of from a list. It sweeps every file outside the adapter package for
the three ways something outside `stella_harbor` reaches into it, and checks
each against the live module:

- `.py` — read the AST. Every `from stella_harbor… import X` / `import
  stella_harbor…` must resolve, and every call through a name that import bound
  must pass keywords the live `inspect.signature` accepts, and no more
  positional arguments than it can take.
- `.sh` / `.bash` / `.md` / `.mdx` — read the `from stella_harbor… import …`
  lines textually. Shell is not parseable as Python, and this is the half that
  catches `witness_ab.sh`: an `ImportError` inside a heredoc, behind `|| exit
  1`, which took the *treatment* arm off the air for weeks.
- Everything textual — resolve the `module:attribute` entry-point specs
  (`--agent-import-path stella_harbor:StellaAgent`). Harbor resolves that
  string at launch, so a rename breaks it in `.sh`, in `.md`, in `.mdx`, and in
  `bench/loop-bench/src/main.rs` — a caller in a language no Python check would
  otherwise reach.

**A sweep that finds nothing is worse than no sweep**, because it reads as a
passing gate; that is precisely how #2182 survived. So `_require_non_vacuous`
fails on a zero count for any of the three, on a repository root that does not
contain the adapter package, and on an adapter submodule that will not import —
and `TestSweepAntiVacuity` exercises each of those failures against a synthetic
tree rather than trusting them.

Known limits, declared rather than silent: a call whose keywords arrive by
`**splat` is not checkable from source and is skipped (the manifest seam forbids
splats outright, above); a callee taking `**kwargs` accepts anything and is
skipped; and a name rebound after import is resolved to the imported object.
"""

from __future__ import annotations

import ast
import functools
import importlib
import importlib.util
import inspect
import os
import re
from pathlib import Path
from types import ModuleType
from typing import Any, NamedTuple

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _benchmark_assurance_tiers,
    _benchmark_engine_posture,
)

_REPO_ROOT = Path(__file__).resolve().parents[3]
_EVIDENCE = _REPO_ROOT / "bench" / "evidence"
_MAKE_MANIFEST = _EVIDENCE / "make_manifest.py"
#: Shell drivers that embed Python importing the adapter in a heredoc.
_EVIDENCE_RUN = _EVIDENCE / "run"

#: Callee name in `make_manifest.py` → the live function it must bind to.
_CALLEES = {
    "_benchmark_engine_posture": _benchmark_engine_posture,
    "_benchmark_assurance_tiers": _benchmark_assurance_tiers,
}

# The arms the manifest has to be able to describe. `None` is the control arm —
# the one `finalize.sh` runs by default, and the one whose number is published.
_WORKER = "openrouter/z-ai/glm-5.1"
_VERIFIER = "openrouter/deepseek/deepseek-v4-pro"


def _manifest_source() -> str:
    """The manifest builder's source, or a skip that names what is missing.

    Skipped rather than passed when the file is genuinely absent: a parity
    check that silently finds nothing to check is decoration, and this one
    already failed once by being unreachable.
    """
    if not _MAKE_MANIFEST.is_file():
        pytest.skip(
            f"{_MAKE_MANIFEST} is not present in this checkout; if it moved, "
            "correct `_MAKE_MANIFEST` in this file"
        )
    return _MAKE_MANIFEST.read_text(encoding="utf-8")


def _called_name(node: ast.Call) -> str | None:
    """The bare name of a call target, through either import style."""
    func = node.func
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _call_sites() -> dict[str, list[ast.Call]]:
    """Every call to a tracked adapter function, keyed by callee name."""
    tree = ast.parse(_manifest_source(), filename=str(_MAKE_MANIFEST))
    sites: dict[str, list[ast.Call]] = {name: [] for name in _CALLEES}
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            name = _called_name(node)
            if name in sites:
                sites[name].append(node)
    return sites


def _selector_keywords() -> set[str]:
    """The host-side selector, read off the adapter instead of spelled here.

    `_benchmark_assurance_tiers` exists only for the callers that hold the
    host-side inputs — `make_manifest.py` and the preregistration tooling — so
    its keyword-only parameters *are* that selector. Intersecting with the
    posture builder's keeps the tuning-only knobs (`worker_effort`,
    `triage_model`, …) out: those are the harness's business, not the
    manifest's.
    """
    return _keyword_only(_benchmark_assurance_tiers) & _keyword_only(
        _benchmark_engine_posture
    )


def _keyword_only(function: Any) -> set[str]:
    return {
        name
        for name, parameter in inspect.signature(function).parameters.items()
        if parameter.kind is inspect.Parameter.KEYWORD_ONLY
    }


def _load_make_manifest() -> ModuleType:
    """Import `make_manifest.py` by path — it is a script, not a package."""
    _manifest_source()  # skips with the actionable message when it is gone
    spec = importlib.util.spec_from_file_location("_tb_make_manifest", _MAKE_MANIFEST)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_manifest_builder_is_readable_and_calls_the_adapter() -> None:
    """Guard the guard: an unreachable parity check passes vacuously.

    #2182 is exactly a check that could not run where it mattered, so the two
    assertions below come before the ones that matter.
    """
    assert _MAKE_MANIFEST.is_file(), f"{_MAKE_MANIFEST} is missing"
    sites = _call_sites()
    for name, calls in sites.items():
        assert calls, (
            f"{_MAKE_MANIFEST.name} no longer calls `{name}` — either the "
            "manifest stopped reading its posture/tier declaration from the "
            "adapter (a regression: a hand-written value can drift from what "
            "ran) or this file's callee list is stale"
        )


def test_every_keyword_the_manifest_passes_exists_on_the_live_callee() -> None:
    """The crash itself: `witness_author=` against `verifier` (#2182).

    Uncaught by construction — the `try/except` around each call catches
    `ImportError`, and the adapter *is* importable on the run host, which is
    the only place this call is reached. So the manifest was not degraded, it
    was absent.
    """
    for name, calls in _call_sites().items():
        accepted = set(inspect.signature(_CALLEES[name]).parameters)
        for call in calls:
            passed = {keyword.arg for keyword in call.keywords if keyword.arg}
            unknown = passed - accepted
            assert not unknown, (
                f"{_MAKE_MANIFEST.name}:{call.lineno} calls `{name}` with "
                f"{sorted(unknown)}, which the adapter's signature does not "
                f"accept ({sorted(accepted)}). One side was renamed without "
                "the other; a TypeError here writes no manifest at all (#2182)"
            )
            assert not any(keyword.arg is None for keyword in call.keywords), (
                f"{_MAKE_MANIFEST.name}:{call.lineno} splats `**kwargs` into "
                f"`{name}`, which puts the binding beyond what this check can "
                "read — pass the keywords literally"
            )


def test_the_manifest_still_passes_the_arm_selector() -> None:
    """Dropping the kwarg also "fixes" the crash, and is worse than it.

    A manifest built without the selector records the control arm's posture and
    tier digests for whichever arm actually ran, with no outward sign — the
    mislabelled-arm failure `_arm_mismatch` was written to refuse.
    """
    selector = _selector_keywords()
    assert selector, (
        "the adapter's two host-side entry points share no keyword-only "
        "parameter, so this check derives nothing — the selector shape moved "
        "and this test has to follow it"
    )
    for name, calls in _call_sites().items():
        for call in calls:
            passed = {keyword.arg for keyword in call.keywords if keyword.arg}
            assert selector <= passed, (
                f"{_MAKE_MANIFEST.name}:{call.lineno} calls `{name}` without "
                f"{sorted(selector - passed)}: the manifest would declare the "
                "control arm's digests for whatever arm actually ran (#1007)"
            )


@pytest.mark.parametrize("verifier", [None, _VERIFIER], ids=["control", "treatment"])
def test_the_manifest_blocks_bind_against_the_live_adapter(
    verifier: str | None,
) -> None:
    """Call the two blocks for real, on the side where the adapter imports.

    The AST checks above are the durable ones — they fail on a rename in either
    direction. This is the direct demonstration: on the old code both calls
    raise `TypeError: got an unexpected keyword argument 'witness_author'`
    before either block returns.
    """
    module = _load_make_manifest()

    posture = module._posture(_WORKER, verifier)
    assert "error" not in posture, (
        f"the adapter must be importable in this suite: {posture['error']}"
    )
    assert posture["normalized_sha256"]
    assert posture["posture"]["default_model"] == _WORKER

    assurance = module._assurance(_WORKER, verifier)
    assert "error" not in assurance, assurance["error"]
    assert assurance["tiers"]["verifier_model"] == verifier
    assert assurance["tiers"]["arm"] == (
        "witness-off" if verifier is None else "witness-on"
    )


def test_every_adapter_name_the_run_scripts_import_still_exists() -> None:
    """The same rename, one seam further out — `witness_ab.sh` (#2182).

    `#1394` renamed `_validated_witness_author` to `_validated_verifier` and
    left the witness A/B driver importing the old name inside a heredoc. Under
    `ARM=on` that is an `ImportError` behind `|| exit 1`, so the *treatment*
    arm refuses to launch at all — the same shape as the manifest crash, in the
    one script whose whole job is to produce the witness-on number.

    Shell is not parseable as Python, so this reads the `from stella_harbor…
    import …` lines textually. That is enough: the failure being pinned is a
    name that no longer resolves, and the name is right there in the text.
    """
    if not _EVIDENCE_RUN.is_dir():
        pytest.skip(
            f"{_EVIDENCE_RUN} is not present in this checkout; if it moved, "
            "correct `_EVIDENCE_RUN` in this file"
        )
    pattern = re.compile(
        r"^\s*from\s+(stella_harbor(?:\.[\w.]+)?)\s+import\s+([^\n#]+)", re.MULTILINE
    )
    checked = 0
    for script in sorted(_EVIDENCE_RUN.rglob("*.sh")):
        text = script.read_text(encoding="utf-8")
        for module_name, imported in pattern.findall(text):
            module = importlib.import_module(module_name)
            for name in (part.strip() for part in imported.split(",")):
                bare = name.split(" as ")[0].strip()
                if not bare or bare == "*":
                    continue
                checked += 1
                assert hasattr(module, bare), (
                    f"{script.relative_to(_REPO_ROOT)} imports `{bare}` from "
                    f"`{module_name}`, which no longer defines it. The import "
                    "sits behind `|| exit 1`, so the arm this script drives "
                    "refuses to launch (#2182)"
                )
    assert checked, (
        f"no `from stella_harbor … import …` line was found under "
        f"{_EVIDENCE_RUN} — this check has stopped reading its subject"
    )


def test_the_two_blocks_disclose_the_arm_in_their_digests() -> None:
    """The selector has to reach the hash, not merely be accepted.

    A parameter can bind and be ignored — which is what a rename repaired by
    deleting the argument would look like from the AST. The digests are what a
    reader of committed evidence compares, so they are what must move.
    """
    module = _load_make_manifest()
    for block in (module._posture, module._assurance):
        control = block(_WORKER, None)
        treatment = block(_WORKER, _VERIFIER)
        assert control["normalized_sha256"] != treatment["normalized_sha256"], (
            f"{block.__name__} hashes the two arms identically: the arm would "
            "be invisible in the registered digest (#1007)"
        )


# ---------------------------------------------------------------------------
# The class-level sweep (#2203): every caller, derived from the tree
# ---------------------------------------------------------------------------

#: The adapter package, relative to the repository root. Everything under it is
#: excluded from the `.py` sweep — an intra-package import is the package's own
#: business, and it is the one thing a rename cannot leave behind.
_PACKAGE_PARTS = ("bench", "harbor_adapter", "stella_harbor")

#: Directory names never descended into. Build output and virtualenvs hold
#: vendored copies of this adapter's own dependencies, and `.claude/` holds
#: agent worktrees — each a second checkout of this repository, which would be
#: swept as though its files were this one's.
_PRUNED_DIRS = frozenset(
    {
        ".git",
        ".claude",
        ".mypy_cache",
        ".next",
        ".pytest_cache",
        ".ruff_cache",
        ".venv",
        "__pycache__",
        "node_modules",
        "target",
        "venv",
    }
)

#: Files that can embed Python the interpreter never sees as a `.py` — a shell
#: heredoc, a fenced block in a runbook. Read textually, because they are not
#: parseable as Python and the failure being pinned is a name, not a call.
_EMBEDDED_SUFFIXES = frozenset({".bash", ".md", ".mdx", ".sh", ".zsh"})

#: Files that can carry a `module:attribute` entry-point spec. Deliberately
#: wider than the two above: Harbor resolves that string at launch, so the spec
#: is a live call in whatever language happens to be holding it — including
#: `bench/loop-bench/src/main.rs`.
_SPEC_SUFFIXES = frozenset(
    {
        ".bash",
        ".json",
        ".md",
        ".mdx",
        ".py",
        ".rs",
        ".sh",
        ".toml",
        ".txt",
        ".yaml",
        ".yml",
        ".zsh",
    }
)

#: `from stella_harbor… import a, b as c` — the same pattern the per-seam check
#: above uses, applied to the whole tree rather than one directory.
_EMBEDDED_IMPORT = re.compile(
    r"^\s*from\s+(stella_harbor(?:\.[\w.]+)?)\s+import\s+([^\n#]+)", re.MULTILINE
)

#: `--agent-import-path stella_harbor:StellaAgent`, and the packaging spelling
#: of the same thing (`stella_harbor.secure_launcher:main`).
_ENTRY_POINT_SPEC = re.compile(
    r"\bstella_harbor(\.[A-Za-z_][\w.]*)?:([A-Za-z_]\w*)"
)


class _Sweep(NamedTuple):
    """One repository-wide pass: what was checked, and what did not hold.

    The counts are not decoration — they are the anti-vacuity evidence. A
    sweep reporting no problems and no checks has not proved the tree is
    clean; it has proved only that it stopped reading (#2182).
    """

    problems: tuple[str, ...]
    resolved_imports: int
    checked_calls: int
    embedded_imports: int
    entry_points: int
    caller_files: tuple[str, ...]


def _adapter_package(root: Path) -> Path:
    """The adapter package under `root`, or a failure naming what to edit."""
    package = root.joinpath(*_PACKAGE_PARTS)
    assert package.is_dir(), (
        f"{package} is not a directory, so this sweep is not looking at a "
        "Stella checkout and every count below would be zero. If the layout "
        "moved, correct `_REPO_ROOT` / `_PACKAGE_PARTS` in this file"
    )
    return package


def _walk(root: Path, suffixes: frozenset[str]) -> list[Path]:
    """Every file under `root` with one of `suffixes`, pruned and sorted.

    Sorted so a failure list is stable across machines: the tree is walked to
    produce a diffable report, not just a boolean.
    """
    found: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(name for name in dirnames if name not in _PRUNED_DIRS)
        for name in sorted(filenames):
            if Path(name).suffix in suffixes:
                found.append(Path(dirpath) / name)
    return found


def _read(path: Path) -> str | None:
    """The file's text, or `None` when it is not UTF-8 text at all."""
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def _member(module: ModuleType, name: str) -> Any | None:
    """`getattr`, but importing a submodule that nothing has pulled in yet.

    `from stella_harbor import posture` binds a submodule the parent package
    may not have imported, so a bare `hasattr` reports a live name as missing.
    Treating that as a rename would be a false red on a benchmark gate, which
    costs as much trust as the miss it is guarding against.
    """
    if hasattr(module, name):
        return getattr(module, name)
    try:
        importlib.import_module(f"{module.__name__}.{name}")
    except ImportError:
        return None
    return getattr(module, name, None)


def _attribute(obj: Any, parts: list[str]) -> Any | None:
    """Resolve a dotted chain off an already-bound object."""
    for part in parts:
        found = _member(obj, part) if inspect.ismodule(obj) else getattr(obj, part, None)
        if found is None:
            return None
        obj = found
    return obj


def _dotted(node: ast.expr) -> list[str] | None:
    """`a.b.c` → `["a", "b", "c"]`; anything else → `None`."""
    parts: list[str] = []
    current = node
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if not isinstance(current, ast.Name):
        return None
    parts.append(current.id)
    return list(reversed(parts))


def _import_module(name: str, where: str, problems: list[str]) -> ModuleType | None:
    """Import an adapter module, recording a failure instead of raising.

    An unimportable `stella_harbor.<thing>` is the module-level spelling of
    #2182 — the caller names something the package no longer has — so it has
    to read as a parity failure, not as a broken test run.
    """
    try:
        return importlib.import_module(name)
    except ImportError as exc:
        problems.append(
            f"{where}: `{name}` will not import ({exc}). A caller naming a "
            "module the adapter no longer has fails at launch, not at import "
            "of this suite (#2203)"
        )
        return None


def _check_call(
    node: ast.Call, target: Any, label: str, where: str, problems: list[str]
) -> bool:
    """Compare one call against the live signature. True when it was read.

    Returns False for the shapes source cannot answer — a callee that takes
    `**kwargs` accepts anything, and a caller splatting `**mapping` names
    nothing statically. Both are declared limits in the module docstring
    rather than silent passes.
    """
    if not callable(target):
        return False
    try:
        signature = inspect.signature(target)
    except (TypeError, ValueError):
        return False
    parameters = list(signature.parameters.values())
    if any(p.kind is inspect.Parameter.VAR_KEYWORD for p in parameters):
        return False
    if any(keyword.arg is None for keyword in node.keywords):
        return False

    accepted = set(signature.parameters)
    unknown = sorted({keyword.arg for keyword in node.keywords if keyword.arg} - accepted)
    if unknown:
        problems.append(
            f"{where}:{node.lineno}: `{label}` is called with {unknown}, which "
            f"its live signature does not accept ({sorted(accepted)}). One "
            "side of this seam was renamed without the other — a TypeError on "
            "the run host, in code no suite reaches off it (#2182)"
        )

    starred = any(isinstance(arg, ast.Starred) for arg in node.args)
    variadic = any(p.kind is inspect.Parameter.VAR_POSITIONAL for p in parameters)
    if not starred and not variadic:
        seats = sum(
            1
            for p in parameters
            if p.kind
            in (inspect.Parameter.POSITIONAL_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD)
        )
        if len(node.args) > seats:
            problems.append(
                f"{where}:{node.lineno}: `{label}` is called with "
                f"{len(node.args)} positional arguments; its live signature "
                f"takes at most {seats}. The callee's arity moved and this "
                "caller did not (#2203)"
            )
    return True


def _bind_adapter_names(
    tree: ast.Module, where: str, problems: list[str]
) -> tuple[dict[str, Any], int]:
    """Every name this module's imports bind to something in the adapter."""
    bound: dict[str, Any] = {}
    resolved = 0
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            if not node.module or node.module.split(".")[0] != "stella_harbor":
                continue
            module = _import_module(node.module, f"{where}:{node.lineno}", problems)
            if module is None:
                continue
            for alias in node.names:
                if alias.name == "*":
                    continue
                resolved += 1
                member = _member(module, alias.name)
                if member is None:
                    problems.append(
                        f"{where}:{node.lineno}: imports `{alias.name}` from "
                        f"`{node.module}`, which no longer defines it (#2182)"
                    )
                    continue
                bound[alias.asname or alias.name] = member
        elif isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name.split(".")[0] != "stella_harbor":
                    continue
                resolved += 1
                module = _import_module(alias.name, f"{where}:{node.lineno}", problems)
                if module is None:
                    continue
                if alias.asname:
                    bound[alias.asname] = module
                else:
                    bound["stella_harbor"] = importlib.import_module("stella_harbor")
    return bound, resolved


def _sweep_python(
    root: Path, package: Path, problems: list[str]
) -> tuple[int, int, set[str]]:
    """AST-check every `.py` outside the package. → (imports, calls, files)."""
    resolved = 0
    checked = 0
    files: set[str] = set()
    for path in _walk(root, frozenset({".py"})):
        if package in path.parents:
            continue
        text = _read(path)
        if text is None or "stella_harbor" not in text:
            continue
        where = path.relative_to(root).as_posix()
        try:
            tree = ast.parse(text, filename=str(path))
        except SyntaxError as exc:
            problems.append(f"{where}: will not parse as Python ({exc})")
            continue

        bound, found = _bind_adapter_names(tree, where, problems)
        resolved += found
        if found:
            files.add(where)
        if not bound:
            continue

        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            chain = _dotted(node.func)
            if not chain or chain[0] not in bound:
                continue
            target = bound[chain[0]] if len(chain) == 1 else _attribute(bound[chain[0]], chain[1:])
            label = ".".join(chain)
            if target is None:
                problems.append(
                    f"{where}:{node.lineno}: `{label}` does not resolve on the "
                    "live adapter — the attribute it reaches through is gone "
                    "(#2203)"
                )
                continue
            if _check_call(node, target, label, where, problems):
                checked += 1
    return resolved, checked, files


def _sweep_embedded(root: Path, problems: list[str]) -> tuple[int, set[str]]:
    """Resolve `from stella_harbor… import …` in shell and prose."""
    checked = 0
    files: set[str] = set()
    for path in _walk(root, _EMBEDDED_SUFFIXES):
        text = _read(path)
        if text is None or "stella_harbor" not in text:
            continue
        where = path.relative_to(root).as_posix()
        for module_name, imported in _EMBEDDED_IMPORT.findall(text):
            line = text[: text.index(f"from {module_name} import")].count("\n") + 1
            module = _import_module(module_name, f"{where}:{line}", problems)
            if module is None:
                files.add(where)
                continue
            for entry in imported.split(","):
                bare = entry.strip().split(" as ")[0].strip().strip("()")
                if not bare or bare == "*":
                    continue
                checked += 1
                files.add(where)
                if _member(module, bare) is None:
                    problems.append(
                        f"{where}:{line}: imports `{bare}` from `{module_name}`, "
                        "which no longer defines it. Embedded in a heredoc "
                        "behind `|| exit 1`, this takes the arm off the air "
                        "with no other symptom (#2182)"
                    )
    return checked, files


def _sweep_entry_points(root: Path, problems: list[str]) -> tuple[int, set[str]]:
    """Resolve every `module:attribute` spec a launcher would import."""
    checked = 0
    files: set[str] = set()
    for path in _walk(root, _SPEC_SUFFIXES):
        text = _read(path)
        if text is None or "stella_harbor" not in text:
            continue
        where = path.relative_to(root).as_posix()
        for line_no, line in enumerate(text.splitlines(), 1):
            for match in _ENTRY_POINT_SPEC.finditer(line):
                module_name = "stella_harbor" + (match.group(1) or "")
                attribute = match.group(2)
                checked += 1
                files.add(where)
                module = _import_module(module_name, f"{where}:{line_no}", problems)
                if module is None:
                    continue
                if _member(module, attribute) is None:
                    problems.append(
                        f"{where}:{line_no}: names the entry point "
                        f"`{module_name}:{attribute}`, which the adapter no "
                        "longer defines. Harbor resolves this string at "
                        "launch, so the run dies before its first turn (#2203)"
                    )
    return checked, files


@functools.cache
def _sweep(root: Path) -> _Sweep:
    """One pass over `root`. Cached: three tests read the same walk."""
    package = _adapter_package(root)
    problems: list[str] = []
    resolved, checked, python_files = _sweep_python(root, package, problems)
    embedded, embedded_files = _sweep_embedded(root, problems)
    specs, spec_files = _sweep_entry_points(root, problems)
    return _Sweep(
        problems=tuple(problems),
        resolved_imports=resolved,
        checked_calls=checked,
        embedded_imports=embedded,
        entry_points=specs,
        caller_files=tuple(sorted(python_files | embedded_files | spec_files)),
    )


def _require_non_vacuous(sweep: _Sweep) -> None:
    """Fail when the sweep examined nothing — the #2182 failure mode itself.

    Separated from the assertions that read `problems` so it can be pointed at
    a synthetic tree and *observed* failing, rather than believed.
    """
    empty = [
        name
        for name, count in (
            ("resolved imports", sweep.resolved_imports),
            ("checked call sites", sweep.checked_calls),
            ("embedded shell/prose imports", sweep.embedded_imports),
            ("entry-point specs", sweep.entry_points),
        )
        if count == 0
    ]
    assert not empty, (
        f"the adapter-caller sweep found zero {' and zero '.join(empty)}. A "
        "parity check that examines nothing reads as a passing gate, which is "
        "strictly worse than no gate — it is how #2182 survived. Either the "
        "callers moved out from under `_PRUNED_DIRS` / `_PACKAGE_PARTS` in "
        "this file, or the sweep stopped reading its subject"
    )


def test_the_sweep_reads_the_repository_before_it_judges_it() -> None:
    """Guard the guard, first — everything below is vacuous without this."""
    _require_non_vacuous(_sweep(_REPO_ROOT))


def test_every_caller_in_the_tree_still_binds_to_the_live_adapter() -> None:
    """The class #2182 belongs to: any caller, any language, any seam.

    `_CALLEES` and `_EVIDENCE_RUN` above name two seams. This names none: it
    derives the callee set from the live module and the caller set from the
    tree, so the third caller — the one no one has written yet — is covered on
    the day it lands rather than on the day it strands.
    """
    sweep = _sweep(_REPO_ROOT)
    _require_non_vacuous(sweep)
    assert not sweep.problems, "\n".join(("", *sweep.problems))


def test_the_bench_workflow_runs_when_a_caller_changes() -> None:
    """A sweep that reads a file must be triggered by that file.

    Same argument as `test_posture.py::TestOutputCeilingParity::
    test_the_bench_workflow_runs_when_the_catalog_changes`, one class wider:
    this suite is gated behind a path filter, and it now reads callers the
    filter did not name — `bench/tb21_preregistration.py` was outside it.

    Documentation is exempt, and the exemption is the honest half. A stale
    `stella_harbor:StellaAgent` in a published guide misleads a reader; it does
    not take an arm off the air, and the sweep still catches it on the next
    push to `main`, where the filter does not apply. Widening the filter to
    every `.md`/`.mdx` in the repository would run this suite on prose changes
    forever to shorten that window, which is the wrong trade.
    """
    workflow = _REPO_ROOT / ".github" / "workflows" / "bench.yml"
    assert workflow.is_file(), f"{workflow} is missing"
    match = re.search(r"grep -Eq '(\^\([^']+\))'", workflow.read_text(encoding="utf-8"))
    assert match, (
        f"{workflow.name} no longer carries a `grep -Eq '^(…)'` scope filter "
        "in the shape this check reads; if it was restructured, correct this "
        "test rather than deleting it"
    )
    scope = re.compile(match.group(1))
    uncovered = [
        path
        for path in _sweep(_REPO_ROOT).caller_files
        if Path(path).suffix not in {".md", ".mdx"} and not scope.search(path)
    ]
    assert not uncovered, (
        f"{workflow.name}'s scope filter does not match {uncovered}, so a PR "
        "touching only those callers skips this suite — including the sweep "
        "whose whole subject they are"
    )


#: The sweep reads this file too, so a fixture below that spelled a stranded
#: name literally would be found — correctly — by the real sweep and fail it.
#: Every fixture therefore assembles the module name at runtime through this.
_ADAPTER = "stella_harbor"


class TestSweepAntiVacuity:
    """Prove each way the sweep can stop reading actually fails.

    #2182 was not missed for want of a check; it was missed because the check
    that existed could not run where the defect was, and said nothing about
    that. So every "this cannot silently pass" claim above is exercised here
    against a synthetic tree, rather than asserted in a docstring.
    """

    @staticmethod
    def _skeleton(root: Path) -> Path:
        """A tree that looks like a checkout but contains no callers."""
        package = root.joinpath(*_PACKAGE_PARTS)
        package.mkdir(parents=True)
        (package / "__init__.py").write_text("", encoding="utf-8")
        return package

    def test_a_tree_that_is_not_a_checkout_names_the_constant_to_edit(
        self, tmp_path: Path
    ) -> None:
        with pytest.raises(AssertionError) as caught:
            _sweep(tmp_path / "not-a-checkout")
        message = str(caught.value)
        assert "_REPO_ROOT" in message
        assert "_PACKAGE_PARTS" in message

    def test_a_tree_with_no_callers_fails_rather_than_passes(
        self, tmp_path: Path
    ) -> None:
        """The whole point: zero findings over zero subjects is not green."""
        self._skeleton(tmp_path)
        sweep = _sweep(tmp_path)
        assert not sweep.problems, "the empty tree must be clean, just vacuous"
        with pytest.raises(AssertionError) as caught:
            _require_non_vacuous(sweep)
        message = str(caught.value)
        assert "resolved imports" in message
        assert "entry-point specs" in message

    def test_an_adapter_module_a_caller_names_but_lacks_is_a_failure(
        self, tmp_path: Path
    ) -> None:
        """A rename at module granularity, not just at name granularity."""
        self._skeleton(tmp_path)
        caller = tmp_path / "caller.py"
        caller.write_text(
            f"from {_ADAPTER}.no_such_module import thing\nthing()\n",
            encoding="utf-8",
        )
        problems = _sweep(tmp_path).problems
        assert any("no_such_module" in problem for problem in problems), problems
        assert any("will not import" in problem for problem in problems), problems

    def test_a_stranded_keyword_is_named_with_its_file_and_line(
        self, tmp_path: Path
    ) -> None:
        """The #2182 shape itself, reproduced against the live adapter."""
        self._skeleton(tmp_path)
        caller = tmp_path / "caller.py"
        caller.write_text(
            "\n".join(
                (
                    f"from {_ADAPTER} import _benchmark_engine_posture",
                    "",
                    "_benchmark_engine_posture(",
                    f'    "{_WORKER}",',
                    f'    witness_author="{_VERIFIER}",',
                    ")",
                    "",
                )
            ),
            encoding="utf-8",
        )
        problems = _sweep(tmp_path).problems
        assert any(
            "caller.py:3" in problem and "witness_author" in problem
            for problem in problems
        ), problems

    def test_a_stranded_entry_point_spec_is_a_failure(self, tmp_path: Path) -> None:
        """The `--agent-import-path` seam, which no AST walk can see."""
        self._skeleton(tmp_path)
        (tmp_path / "run.sh").write_text(
            f"harbor run --agent-import-path {_ADAPTER}:NoSuchAgent\n",
            encoding="utf-8",
        )
        problems = _sweep(tmp_path).problems
        assert any(
            "run.sh:1" in problem and "NoSuchAgent" in problem for problem in problems
        ), problems

    def test_a_stranded_heredoc_import_is_a_failure(self, tmp_path: Path) -> None:
        """`witness_ab.sh`'s shape: a name inside shell, behind `|| exit 1`."""
        self._skeleton(tmp_path)
        (tmp_path / "arm.sh").write_text(
            "\n".join(
                (
                    "python3 - <<'PY' || exit 1",
                    f"from {_ADAPTER}.posture import _validated_witness_author",
                    "PY",
                    "",
                )
            ),
            encoding="utf-8",
        )
        problems = _sweep(tmp_path).problems
        assert any(
            "arm.sh:2" in problem and "_validated_witness_author" in problem
            for problem in problems
        ), problems
