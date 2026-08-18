"""The credential-absence refusal must name the variable holding the secret.

The guard itself is correct and load-bearing: it is what stops a provider key
reaching a task container's ``/proc/<pid>/environ``. What it did not do was say
*which* host variable carried the credential, so every occurrence cost an
operator a manual hunt through the environment — and the shape it actually
catches is an alias under a name nothing recognises, which is precisely the
case a human cannot guess.
"""

from __future__ import annotations

from stella_harbor import (
    _credential_carrying_names,
    _credential_leak_message,
    _is_credential_env_name,
)


class TestTheRefusalNamesTheVariable:
    def test_an_alias_under_an_unrecognised_name_is_named(self) -> None:
        """``VOYAGE_AI_KEY`` beside ``VOYAGE_API_KEY`` is the real-world case.

        It ends ``_AI_KEY``, not ``_API_KEY``, so the credential-name policy
        does not strip it from the Compose host environment and the arena's
        ``_SCRUBBED_PREFIXES`` do not scrub it. The guard fires correctly on a
        variable the operator has to be told the name of.
        """
        secret = "voyage-live-secret-value"
        assert not _is_credential_env_name("VOYAGE_AI_KEY")

        host_env = {
            "PATH": "/usr/bin",
            "VOYAGE_AI_KEY": secret,
            "DOCKER_DEFAULT_PLATFORM": "linux/amd64",
        }
        carriers = _credential_carrying_names(host_env, (secret,))
        assert carriers == ("VOYAGE_AI_KEY",)

        message = _credential_leak_message(carriers)
        assert "VOYAGE_AI_KEY" in message
        # The refusal must not render the material it exists to contain.
        assert secret not in message

    def test_a_clean_environment_names_nothing(self) -> None:
        host_env = {"PATH": "/usr/bin", "DOCKER_DEFAULT_PLATFORM": "linux/amd64"}
        assert _credential_carrying_names(host_env, ("a-secret",)) == ()

    def test_every_carrier_is_named_not_just_the_first(self) -> None:
        """A Bedrock route has several secrets; naming one leaves a second hunt."""
        secret = "shared-secret-value"
        host_env = {"SECOND_ALIAS": secret, "FIRST_ALIAS": secret, "PATH": "/usr/bin"}
        carriers = _credential_carrying_names(host_env, (secret,))
        assert carriers == ("FIRST_ALIAS", "SECOND_ALIAS")
        message = _credential_leak_message(carriers)
        assert "FIRST_ALIAS" in message and "SECOND_ALIAS" in message
