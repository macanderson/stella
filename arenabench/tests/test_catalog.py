# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The role-model catalog (#2065): the parser, the benchmarked flag, and the
honest-degradation contract.

The synthetic-source tests pin the parser's blind spots (the same ones the
harbor adapter's ``TestCatalogCeilingParser`` learned the hard way); the
live tests read the real files in this repository, so a catalog.rs or
posture.py shape change fails here before it ships a broken select.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from arenabench import catalog as cat

_REPO_ROOT = Path(__file__).resolve().parents[2]

_SYNTHETIC = """
impl Catalog {
    pub fn seed() -> Self {
        Self {
            entries: vec![
                CatalogEntry::new(
                    "glm-5.2",
                    "zai",
                    "glm",
                    200_000,
                    ToolDialect::OpenaiJson,
                    Pricing { input_usd_per_mtok: 0.60 },
                )
                .with_max_output_tokens(Some(131_072)),
                CatalogEntry::new(
                    "anthropic/claude-sonnet-5",
                    "openrouter",
                    "claude",
                    200_000,
                    ToolDialect::Anthropic,
                    Pricing { input_usd_per_mtok: 3.0 },
                ),
                CatalogEntry::new(
                    "test-only-fixture",
                    "zai",
                    "glm",
                    1,
                    ToolDialect::OpenaiJson,
                    Pricing { input_usd_per_mtok: 0.0 },
                ),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    // A fixture entry the region bound must exclude:
    // CatalogEntry::new("ghost", "nowhere", ...)
    //     .with_max_output_tokens(Some(1)),
}
"""


class TestParser:
    def test_entries_carry_slug_provider_and_optional_cap(self) -> None:
        entries = cat.parse_catalog(_SYNTHETIC)
        assert {e["slug"] for e in entries} == {"glm-5.2", "anthropic/claude-sonnet-5"}
        by_slug = {e["slug"]: e for e in entries}
        assert by_slug["glm-5.2"]["provider"] == "zai"
        assert by_slug["glm-5.2"]["output_cap"] == 131_072
        # No declared ceiling is None, never a guessed number.
        assert by_slug["anthropic/claude-sonnet-5"]["output_cap"] is None

    def test_the_test_module_and_fixtures_are_outside_the_region(self) -> None:
        slugs = {e["slug"] for e in cat.parse_catalog(_SYNTHETIC)}
        assert "ghost" not in slugs
        assert "test-only-fixture" not in slugs

    def test_benchmarked_slugs_read_via_ast_not_import(self) -> None:
        source = (
            "import something_unimportable\n"
            "_BENCHMARKED_SLUGS = frozenset({'claude-sonnet-5', 'glm-5.2'})\n"
        )
        assert cat.parse_benchmarked_slugs(source) == {"claude-sonnet-5", "glm-5.2"}
        assert cat.parse_benchmarked_slugs("x = 1\n") == frozenset()


class TestLiveSources:
    """Against the real files in this repository — the shape ratchet."""

    def test_the_real_catalog_parses_to_entries(self) -> None:
        source = (_REPO_ROOT / cat._CATALOG_REL).read_text(encoding="utf-8")
        entries = cat.parse_catalog(source)
        assert entries, "the shipping catalog parsed to zero entries"
        assert all(e["slug"] and e["provider"] for e in entries)

    def test_the_real_posture_yields_benchmarked_slugs(self) -> None:
        source = (_REPO_ROOT / cat._POSTURE_REL).read_text(encoding="utf-8")
        benchmarked = cat.parse_benchmarked_slugs(source)
        assert "claude-sonnet-5" in benchmarked

    def test_payload_groups_by_provider_and_flags_benchmarked(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ARENABENCH_STELLA_REPO", str(_REPO_ROOT))
        payload = cat.models_payload()
        providers = payload["providers"]
        assert providers, "no providers in the payload"
        flat = [entry for listing in providers.values() for entry in listing]
        assert any(entry["benchmarked"] for entry in flat), (
            "no benchmarked slug surfaced — the _BENCHMARKED_SLUGS bridge is broken"
        )
        for provider, listing in providers.items():
            assert listing == sorted(listing, key=lambda e: e["slug"]), provider

    def test_no_checkout_degrades_with_the_env_name(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv("ARENABENCH_STELLA_REPO", raising=False)
        monkeypatch.delenv("ARENABENCH_STELLA_ADAPTER", raising=False)
        with pytest.raises(RuntimeError) as caught:
            cat.models_payload()
        assert "ARENABENCH_STELLA_REPO" in str(caught.value)
