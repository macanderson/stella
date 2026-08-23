"""The catalog-ceiling ratchet: what the frozen posture may cap a model at.

Split from `test_posture.py` for the reason that file was split from
`test_adapter.py` — the file-size gate treats a separable concern as its own
module rather than as more length on an existing one. #3862 is what made it
separable in practice as well as in principle: the seed rows moved out of
`catalog.rs` into `catalog/seed/<provider>.rs`, this reader had to follow, and
the parent file crossed 1500 lines on the way.

The subject is #2411: the benchmark's frozen posture is a literal, the model's
own ceiling is a literal in the Rust tree, and nothing connected the two — so a
catalog that learns a higher ceiling left the benchmark quietly capped at the
old one. Two layers are covered: the parser's own blind spots, pinned against
synthetic source, and the parity check that reads the catalog this repo ships.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _benchmark_engine_posture,
)

# Imported from the module rather than the package: the set of slugs an arm can
# book is internal to the posture's policy, not part of what the adapter
# re-exports.
from stella_harbor.posture import _BENCHMARKED_SLUGS  # noqa: E402

_REPO_ROOT = Path(__file__).resolve().parents[3]
_BENCH_WORKFLOW = _REPO_ROOT / ".github" / "workflows" / "bench.yml"

#: Where the seeded model rows live. A directory since #3862 split them one
#: module per provider — `catalog.rs` still exists and still holds `Catalog`,
#: it just holds no ceilings, so a reader anchored on it would have parsed
#: cleanly and found nothing.
_CATALOG_SEED_DIR = (
    _REPO_ROOT / "crates" / "stella-model" / "src" / "catalog" / "seed"
)
def _parse_output_ceilings(source: str) -> dict[str, int]:
    """Collect each shipping model's completion ceiling out of catalog.rs text.

    Parsed rather than duplicated, because duplicating it is the defect this
    exists to catch. Entries with no `with_max_output_tokens` are omitted:
    the engine falls back to its global default for those, and the posture
    has no per-model ceiling to match.

    Split out from the file read so the parser's own blind spots can be
    tested against synthetic source — see `TestCatalogCeilingParser`.
    """
    # Stop at the test module. Chunking on `CatalogEntry::new(` bounds every
    # chunk by the *next* entry, but the seed table's last row has no next
    # seeded entry — its chunk otherwise runs on into `#[cfg(test)]` and
    # adopts the first ceiling it finds among the fixtures there. Skipping
    # `test-only` slugs cannot prevent that: it filters an entry's own
    # identity, not the extent of the chunk before it. The module boundary
    # is what actually closes the region.
    source = source.split("#[cfg(test)]")[0]
    ceilings: dict[str, int] = {}
    for chunk in source.split("CatalogEntry::new(")[1:]:
        head = re.match(r'\s*"([^"]+)"\s*,\s*"([^"]+)"', chunk)
        if head is None:
            continue
        slug, provider = head.groups()
        if slug.startswith("test-only"):
            continue  # fixtures for the catalog's own tests, never benchmarked
        # Within the seed table `split` already consumed the delimiter, so a
        # chunk ends exactly where the next entry begins and the first
        # ceiling in it is this entry's own.
        ceiling = re.search(r"with_max_output_tokens\(Some\(([\d_]+)\)\)", chunk)
        if ceiling is None:
            continue
        # Always provider-qualified, never "the slug already looks qualified".
        # The gateway rows are seeded under slugs like
        # `anthropic/claude-fable-5`, which is what a caller writes as
        # `--model openrouter/anthropic/claude-fable-5`. Treating that slug as
        # already-qualified collapses it onto the direct-Anthropic row's key,
        # and since the gateway rows come later in the file they overwrite it
        # — the ratchet then silently checks one model's posture against
        # another model's ceiling. Caught by deliberately drifting a single
        # row and finding this test still green.
        ceilings[f"{provider}/{slug}"] = int(ceiling.group(1).replace("_", ""))
    return ceilings


def _read_catalog_sources() -> list[str]:
    """Every seed module's text, or a reason instead of a bare `OSError`.

    `_CATALOG_SEED_DIR` is a literal assembled here, and the tree it names
    lives on the Rust side — so a crate move retargets it with nothing on the
    Rust side able to notice. That is not hypothetical: the move under
    `crates/` left this pointing one segment short, and the two tests that read
    the catalog without asserting on it first died with a bare
    `FileNotFoundError`. The traceback named the stale path but not the thing a
    reader has to know, which is that the path is hand-written *in this file*
    and is fixed here.

    An empty directory is the same failure as a missing one and is reported the
    same way: a reader that finds no modules parses no ceilings, and a ratchet
    over nothing is decoration.

    Raising `AssertionError` rather than letting the `OSError` through is what
    makes the failures legible: pytest renders it as a failed assertion with
    the message, not as an unexpected exception in a `pathlib` frame.
    """
    try:
        modules = sorted(_CATALOG_SEED_DIR.glob("*.rs"))
    except OSError as exc:
        modules = []
        reason: str | None = exc.strerror
    else:
        reason = None if modules else "no `*.rs` seed module in it"
    if reason is not None:
        raise AssertionError(
            f"cannot read the model catalog seed at {_CATALOG_SEED_DIR}: "
            f"{reason}. That path is a literal in this file, not a resolved "
            "crate location — if the crate moved, update `_CATALOG_SEED_DIR` "
            "to match."
        )
    sources: list[str] = []
    for module in modules:
        try:
            sources.append(module.read_text(encoding="utf-8"))
        except OSError as exc:
            raise AssertionError(
                f"cannot read the model catalog seed at {_CATALOG_SEED_DIR}: "
                f"{module.name}: {exc.strerror}. That path is a literal in "
                "this file, not a resolved crate location — if the crate "
                "moved, update `_CATALOG_SEED_DIR` to match."
            ) from exc
    return sources


def _bench_change_filter() -> re.Pattern[str]:
    """The `grep -Eq` alternation `bench.yml` gates its expensive half on.

    Read out of the workflow and compiled, so the check below tests what CI
    actually runs. A POSIX ERE of this shape — `^`, `$`, alternation, `\\.` —
    means the same thing to Python's `re`.
    """
    workflow = _BENCH_WORKFLOW.read_text(encoding="utf-8")
    match = re.search(r"grep -Eq '([^']+)'", workflow)
    assert match is not None, (
        f"could not find the `grep -Eq` change filter in "
        f"{_BENCH_WORKFLOW.name} — the gate moved or was rewritten, and this "
        "file's reader must be repointed at it"
    )
    return re.compile(match.group(1))


def _seeded_output_ceilings() -> dict[str, int]:
    """`_parse_output_ceilings` over the real catalog this repo ships.

    Parsed per module and merged, never over the concatenation: chunking on
    `CatalogEntry::new(` bounds a chunk by the *next* entry, so a module whose
    last row declares no ceiling would otherwise adopt the first ceiling of the
    module pasted after it. That is the misread `TestCatalogCeilingParser`
    already pins for the `#[cfg(test)]` boundary, one seam further out.
    """
    ceilings: dict[str, int] = {}
    for source in _read_catalog_sources():
        ceilings.update(_parse_output_ceilings(source))
    return ceilings


class TestCatalogCeilingParser:
    """The parser's own blind spots, pinned against synthetic source.

    The parity check below is only as trustworthy as what this reads out of
    `catalog.rs`, and both ways it has been wrong so far were silent: a
    number was still produced, just the wrong model's. Neither would have
    reddened anything. So the two known misreads get fixtures rather than a
    comment, because a comment does not fail.
    """

    _SHIPPING_ROW = """
                CatalogEntry::new(
                    "claude-sonnet-5",
                    "anthropic",
                    "claude",
                    200_000,
                )
                .with_max_output_tokens(Some(64_000)),
    """

    # The seed table's *last* row, and — as in `catalog.rs` today — one that
    # declares no ceiling of its own. That is what leaves its chunk open: with
    # no ceiling to find first, the next one anywhere below becomes "its".
    _UNCAPPED_LAST_ROW = """
                CatalogEntry::new(
                    "anthropic/claude-haiku-4.5",
                    "openrouter",
                    "claude",
                    200_000,
                ),
    """

    _TEST_MODULE_BARE_BUILDER = """
#[cfg(test)]
mod tests {
    #[test]
    fn a_row_carries_its_own_ceiling() {
        let entry = base.with_max_output_tokens(Some(8_000));
        assert_eq!(entry.max_output_tokens, Some(8_000));
    }
}
"""

    def test_a_fixtures_ceiling_is_not_attributed_to_the_last_shipping_row(
        self,
    ) -> None:
        """The seed table's last row must not adopt a number from the tests.

        Nothing closes its chunk, and it has no ceiling of its own to be found
        first. The bare builder call below is the shape `catalog.rs` actually
        contains — a fixture asserting a ceiling round-trips, reached with no
        `CatalogEntry::new(` in between — so an uncapped shipping row came back
        capped at a number written to exercise the setter. Silent either way:
        green if the posture happens to sit at 8000, otherwise red about a
        model that never declared a ceiling at all.
        """
        source = (
            self._SHIPPING_ROW
            + self._UNCAPPED_LAST_ROW
            + self._TEST_MODULE_BARE_BUILDER
        )
        assert _parse_output_ceilings(source) == {"anthropic/claude-sonnet-5": 64000}

    def test_a_fixture_entry_does_not_become_a_benchmarked_model(self) -> None:
        # The other way test source leaks: a fixture with its own
        # `CatalogEntry::new(` becomes a chunk, and a phantom row the parity
        # check then demands the posture cap at. Its slug is whatever the
        # fixture author picked, so the `test-only` prefix is not a guarantee.
        source = (
            self._SHIPPING_ROW
            + """
#[cfg(test)]
mod tests {
    #[test]
    fn a_row_carries_its_own_ceiling() {
        let entry = CatalogEntry::new(
            "some-fixture",
            "anthropic",
            "claude",
            200_000,
        )
        .with_max_output_tokens(Some(8_000));
    }
}
"""
        )
        assert _parse_output_ceilings(source) == {"anthropic/claude-sonnet-5": 64000}

    def test_a_gateway_slug_keeps_its_own_key(self) -> None:
        # `anthropic/claude-sonnet-5` seeded under `openrouter` is a distinct
        # row from the direct-Anthropic one. Reading the already-slashed slug
        # as fully qualified collapses the two, and the later row wins — so
        # one model's posture gets checked against the other's ceiling.
        source = (
            self._SHIPPING_ROW
            + """
                CatalogEntry::new(
                    "anthropic/claude-sonnet-5",
                    "openrouter",
                    "claude",
                    200_000,
                )
                .with_max_output_tokens(Some(48_000)),
    """
        )
        assert _parse_output_ceilings(source) == {
            "anthropic/claude-sonnet-5": 64000,
            "openrouter/anthropic/claude-sonnet-5": 48000,
        }


class TestOutputCeilingParity:
    """#1211 §6.2: the posture's cap must be the model's own ceiling.

    `params.max_tokens` exists to stop Stella capping itself below what the
    comparator gets — "never be the side that stops first". It is a literal
    in `posture.py`, and the model's actual ceiling is a literal in
    `crates/stella-model/src/catalog.rs`. Nothing connected the two, so a
    catalog that learns a higher ceiling leaves the benchmark quietly capped
    at the old one: the exact handicap the constant was introduced to remove,
    now invisible because both numbers still look deliberate.
    """

    def test_catalog_is_readable_and_seeds_ceilings(self) -> None:
        # If this fails, the parser below is silently checking nothing —
        # which is how a ratchet becomes decoration.
        assert _CATALOG_SEED_DIR.is_dir(), f"{_CATALOG_SEED_DIR} is missing"
        assert _seeded_output_ceilings(), "no seeded ceiling was parsed"

    def test_a_moved_catalog_says_where_the_path_is_fixed(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        """A missing catalog must name the path *and* where to correct it.

        The test above asserts `is_dir()` and so fails legibly on its own;
        the two below read the catalog through `_seeded_output_ceilings` and
        used to surface a bare `FileNotFoundError` from inside `pathlib`. All
        three now share one guarded read, and this pins that: the failure has
        to point at `_CATALOG_SEED_DIR` as the thing to edit, because the last
        time this broke, the cause was a Rust crate move and the fix was a
        literal in this file that no Rust check could have flagged.
        """
        monkeypatch.setattr(
            sys.modules[__name__], "_CATALOG_SEED_DIR", tmp_path / "gone" / "seed"
        )
        with pytest.raises(AssertionError) as caught:
            _seeded_output_ceilings()
        message = str(caught.value)
        assert "seed" in message
        assert "_CATALOG_SEED_DIR" in message

    def test_an_empty_seed_directory_fails_rather_than_parsing_nothing(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        """A directory that exists but holds no seed module is not "no
        ceilings" — it is a reader that has stopped reading, and the whole
        class below would pass vacuously on it.

        #3862 made this reachable: the seed is now a directory rather than one
        file, so "the path resolves" and "there is something to parse" became
        two different questions.
        """
        empty = tmp_path / "seed"
        empty.mkdir()
        monkeypatch.setattr(sys.modules[__name__], "_CATALOG_SEED_DIR", empty)
        with pytest.raises(AssertionError) as caught:
            _seeded_output_ceilings()
        assert "no `*.rs` seed module" in str(caught.value)

    def test_the_bench_workflow_runs_when_the_catalog_changes(self) -> None:
        """A ratchet that reads a file must be triggered by that file.

        The suite's expensive half is gated on a path filter, and every
        pattern in it names something under `bench/` or `arenabench/`. This
        class reads files in the Rust tree, so a PR that only raises a model's
        ceiling used to set `changed=false` and skip the whole suite — the
        parity check guaranteed not to run on the one class of change it exists
        to catch. The path is a hand-written literal on both sides, which is
        how the last crate move broke it with Rust CI all-green.

        The filter is **applied** to each real seed module rather than searched
        for as a substring. A substring check answers an adjacent question: a
        filter naming only `crates/stella-model/src/catalog.rs` still contains
        the text `crates/stella-model/src/catalog`, so it would have reported
        green while skipping every seed change — which is exactly the state
        #3862 left it in.
        """
        pattern = _bench_change_filter()
        modules = sorted(_CATALOG_SEED_DIR.glob("*.rs"))
        assert modules, f"no seed module under {_CATALOG_SEED_DIR} to check"
        for module in modules:
            relative = module.relative_to(_REPO_ROOT).as_posix()
            assert pattern.search(relative), (
                f"{_BENCH_WORKFLOW.name}'s change filter does not match "
                f"{relative}, so a PR touching only that file skips this suite "
                "— including the ceiling parity check below, whose only input "
                "is that file."
            )

    def test_every_bookable_model_has_a_seeded_ceiling(self) -> None:
        """An arm can only book a model the catalog has a ceiling for.

        Without this, the check below passes vacuously for any model whose
        seed row lost its ceiling: `_parse_output_ceilings` omits rows with no
        `with_max_output_tokens`, so the model silently drops out of the
        parity loop and inherits the engine's global 16384 — the exact
        failure the loop exists to catch, hidden by the loop's own filter.
        """
        seeded = _seeded_output_ceilings()
        by_slug = {model.rsplit("/", 1)[-1] for model in seeded}
        missing = sorted(_BENCHMARKED_SLUGS - by_slug)
        assert not missing, (
            f"these models can be booked by an arm but carry no seeded "
            f"ceiling: {missing}. They would run at the engine default "
            f"(16384) instead of their own budget."
        )

    def test_no_role_of_any_bookable_model_asks_for_an_output_cap(self) -> None:
        """The posture sends no `max_tokens`, for any role, on any model.

        This replaces a check that a cap EQUALLED the catalog ceiling unless a
        rationale excused the gap. Both of that check's outcomes are now
        wrong to write: matching the ceiling is a copy of a number the engine
        already reads from the authority itself, and sitting under it is the
        handicap #2411 measured — an arm stopped where the work finishes,
        scored as the other arm being better.

        Absence is what asks for the model's maximum. `tuned_engine_config`
        seeds `max_output_tokens` from the catalog entry and only an explicit
        cap can lower it, so the strongest thing this suite can assert is that
        no explicit cap is ever sent.
        """
        checked = 0
        for model in _seeded_output_ceilings():
            if model.rsplit("/", 1)[-1] not in _BENCHMARKED_SLUGS:
                # Not bookable, so it has no posture to be wrong about; see
                # `_BENCHMARKED_SLUGS` for why the scope is deliberate.
                continue
            posture, _, _ = _benchmark_engine_posture(model)
            for role, agent in posture["agents"].items():
                checked += 1
                assert "max_tokens" not in (agent.get("params") or {}), (
                    f"{model} role {role} pins an output cap. Every value it "
                    "could hold is either the catalog's number restated or a "
                    "ceiling the comparator does not run under — delete the "
                    "key and the engine takes the model's own maximum (#2411)."
                )
        assert checked, "no bookable model was checked — the loop is inert"

    def test_no_role_of_any_bookable_model_is_handed_a_spend_cap(self) -> None:
        """The dollar axis of the same rule.

        An output cap and a per-trial budget are one ceiling in two units, and
        the budget is the one that actually decided a match: in
        ``5292a68cdabf`` all three of the capped seat's losses were the guard
        firing at roughly a third of the task's 900s, each within a step or
        four of done. Nothing in a frozen posture may reintroduce it under
        another spelling.
        """
        banned = {"budget", "budget_usd", "max_tokens", "turn_budget_usd"}
        for model in _seeded_output_ceilings():
            if model.rsplit("/", 1)[-1] not in _BENCHMARKED_SLUGS:
                continue
            posture, _, _ = _benchmark_engine_posture(model)
            offenders = sorted(banned & set(posture)) + sorted(
                f"agents.{role}.{key}"
                for role, agent in posture["agents"].items()
                for key in banned & set(agent)
            )
            assert not offenders, f"{model}: posture declares {offenders}"
