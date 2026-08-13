"""The embedding backend's credential rides the same handoff, independently (#2995).

Its own module rather than a block in `test_adapter.py`, which is at its
recorded size ceiling (`scripts/check-file-size.sh`).

The subject: `search`'s semantic rung needs a Voyage (or OpenAI) key, and
`stella-embed` reads it ambiently regardless of which model provider serves
the trial. Modelling it as a per-provider credential — the way the model
route's own secret is modelled — would make it unreachable for every arm that
does not happen to route its model through that same vendor. This is verified
end to end on the Rust side too: `crates/stella-cli/src/credential_handoff.rs`
proves the target is accepted and pairs positionally, and was manually proven
against the real `stella` binary (not just unit-tested) to reach process
environment after consumption.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

import asyncio  # noqa: E402
from types import SimpleNamespace  # noqa: E402

from harbor.models.agent.context import AgentContext  # noqa: E402

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _INSTALL_PATH,
    StellaAgent,
)
from stella_harbor.credential_bundle import (  # noqa: E402
    EMBEDDING_CREDENTIAL_NAMES,
    optional_embedding_credentials,
)

from .test_adapter import _bare_agent, _stream_for, _trajectory_envelope  # noqa: E402


class TestOptionalEmbeddingCredentials:
    def test_unset_resolves_to_nothing(self) -> None:
        assert optional_embedding_credentials({}.get) == {}

    def test_configured_is_forwarded_by_name(self) -> None:
        resolved = optional_embedding_credentials({"VOYAGE_API_KEY": "pa-test"}.get)
        assert resolved == {"VOYAGE_API_KEY": "pa-test"}

    def test_an_empty_string_is_treated_as_unset(self) -> None:
        assert optional_embedding_credentials({"VOYAGE_API_KEY": ""}.get) == {}

    def test_only_the_registered_names_are_recognized(self) -> None:
        assert frozenset({"VOYAGE_API_KEY"}) == EMBEDDING_CREDENTIAL_NAMES
        resolved = optional_embedding_credentials(
            {"VOYAGE_API_KEY": "pa-test", "SOME_OTHER_KEY": "x"}.get
        )
        assert resolved == {"VOYAGE_API_KEY": "pa-test"}


class TestHandoffCarriesBothRoutes:
    def test_the_embedding_credential_rides_the_same_stdin_after_the_provider(
        self, tmp_path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The witness: on `main` this test fails — `VOYAGE_API_KEY` never
        reaches `STELLA_CREDENTIAL_HANDOFF_TARGET`, stdin carries one line, and
        the trial's `search` has nothing to embed with even when the host has
        a valid key.
        """
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.setenv("VOYAGE_API_KEY", "pa-test-voyage-secret")
        source_envelope = _trajectory_envelope()
        durable_stream = _stream_for(source_envelope)
        (tmp_path / "stella-events.jsonl").write_text(durable_stream)

        agent = _bare_agent()
        agent.logs_dir = tmp_path
        agent.model_name = "openrouter/provider/model"
        agent._extra_env = {}
        agent._version = "stella 0.4.47"

        class _Environment:
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ):
                assert command[0] == _INSTALL_PATH
                # Provider credential first, embedding credential appended —
                # positional pairing on the Stella side depends on this order.
                assert env["STELLA_CREDENTIAL_HANDOFF_TARGET"] == (
                    "OPENROUTER_API_KEY,VOYAGE_API_KEY"
                )
                assert stdin == b"openrouter-test-secret\npa-test-voyage-secret\n"
                # Never as a plain container variable — only on the sealed pipe.
                assert "VOYAGE_API_KEY" not in env
                assert "OPENROUTER_API_KEY" not in env
                return SimpleNamespace(stdout="ok", stderr=None, return_code=0)

        context = AgentContext()
        asyncio.run(
            StellaAgent.run.__wrapped__(
                agent, "Fix the task.", _Environment(), context
            )
        )

    def test_an_unconfigured_embedder_leaves_the_provider_only_handoff_unchanged(
        self, tmp_path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Optional and fails open (#2995): no `VOYAGE_API_KEY` on the host,
        no change to the pre-existing single-credential wire shape.
        """
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.delenv("VOYAGE_API_KEY", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        source_envelope = _trajectory_envelope()
        durable_stream = _stream_for(source_envelope)
        (tmp_path / "stella-events.jsonl").write_text(durable_stream)

        agent = _bare_agent()
        agent.logs_dir = tmp_path
        agent.model_name = "openrouter/provider/model"
        agent._extra_env = {}
        agent._version = "stella 0.4.47"

        class _Environment:
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ):
                assert env["STELLA_CREDENTIAL_HANDOFF_TARGET"] == "OPENROUTER_API_KEY"
                assert stdin == b"openrouter-test-secret\n"
                return SimpleNamespace(stdout="ok", stderr=None, return_code=0)

        context = AgentContext()
        asyncio.run(
            StellaAgent.run.__wrapped__(
                agent, "Fix the task.", _Environment(), context
            )
        )
