"""Host-only credential bundle shared by the secure launcher and adapter.

The benchmark provider key must not exist in Harbor's environment: Docker
Compose performs variable interpolation while building the task environment,
before the installed-agent adapter gets a chance to scrub anything.  The
launcher therefore places provider credentials in one unlinked, owner-only,
seekable file descriptor and execs Harbor with credential-shaped environment
variables removed.  Adapter workers use ``pread`` so concurrent trials never
share or advance a file offset.

A bundle carries a *set*, not a single secret.  Every provider but one
authenticates with exactly one value, and the selection rule used to demand
exactly one — which is why Bedrock, whose SigV4 signing needs an access key id
*and* a secret access key (plus a session token for temporary credentials),
could not be benchmarked at all (#1301).  Each provider now declares what its
route requires, so "one secret" is a property of most providers rather than a
property of the handoff.

Two things stay deliberately out of the credential half:

``routing``
    ``AWS_REGION`` selects an endpoint host.  It is required, it has no other
    home, and it is not a secret: a run manifest discloses it like any other
    routing decision.  It travels in its own half of the bundle so nothing has
    to decide, per call site, whether a given name should be scrubbed from
    evidence — the credential half is scrubbed, the routing half is disclosed.

ambient AWS credential sources
    Profile files, SSO caches, IMDS and the ECS/EKS container-role endpoints,
    and web-identity token files are all rejected.  Supporting them would mean
    the benchmark container resolves whatever AWS identity the host happens to
    be carrying, which is exactly the ambient authority a one-descriptor
    handoff exists to eliminate.  They remain on the scrub deny-list below
    precisely so they cannot leak in by another route.  Explicit credentials
    only.
"""

from __future__ import annotations

import json
import os
import stat
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any, BinaryIO

HOST_CREDENTIAL_BUNDLE_FD_ENV = "STELLA_HOST_CREDENTIAL_BUNDLE_FD"
# v2 adds the ``routing`` half.  A version bump rather than an optional key:
# the reader validates an exact top-level shape, so a launcher and an adapter
# that disagree about whether routing exists must fail loudly at the handoff
# instead of dropping a region and running in the wrong AWS partition.
HOST_CREDENTIAL_BUNDLE_SCHEMA = "stella-host-credential-bundle-v2"
HOST_CREDENTIAL_SOURCE = "anonymous-seekable-fd-v1"
ENV_CREDENTIAL_SOURCE = "environment-fallback"
MAX_CREDENTIAL_BUNDLE_BYTES = 256 * 1024

PROVIDER_CREDENTIAL_NAMES = frozenset(
    {
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "ZAI_API_KEY",
        "OPENROUTER_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "VERTEX_ACCESS_TOKEN",
        "LOCAL_API_KEY",
        # Bedrock's explicit-credential set.  Note AWS_REGION is not here — it
        # is routing, and lives in PROVIDER_ROUTING_NAMES.
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    }
)

# Non-secret values a route cannot be built without.  Carried in the bundle
# because they have no other home — the launcher scrubs the whole AWS family
# out of Harbor's environment — but kept apart from credentials so evidence
# scrubbing never has to guess which names are secret.
PROVIDER_ROUTING_NAMES = frozenset({"AWS_REGION"})

HOST_ONLY_CONTROL_CREDENTIAL_NAMES = frozenset({"OPENROUTER_MANAGEMENT_API_KEY"})


@dataclass(frozen=True)
class ProviderCredentialSpec:
    """What one model provider's route needs from the host environment.

    ``required_any`` is a tuple of *groups*: exactly one name from each group
    must be configured.  A single-name group is a plain requirement; a
    multi-name group is an alias set (``GEMINI_API_KEY``/``GOOGLE_API_KEY``),
    where "exactly one" is what keeps two divergent copies of the same key
    from silently picking a winner.
    """

    required_any: tuple[tuple[str, ...], ...]
    optional: tuple[str, ...] = ()
    routing_any: tuple[tuple[str, ...], ...] = ()
    #: Declaration order for the handoff wire.  The container pairs values with
    #: names positionally, so this order is part of the contract, not cosmetic.
    order: tuple[str, ...] = ()

    def ordered_names(self) -> tuple[str, ...]:
        """Every credential name this spec can select, in wire order."""
        if self.order:
            return self.order
        return tuple(name for group in self.required_any for name in group)


def _single(*names: str) -> ProviderCredentialSpec:
    """A provider that authenticates with exactly one secret."""
    return ProviderCredentialSpec(required_any=(names,))


PROVIDER_CREDENTIAL_SPECS: dict[str, ProviderCredentialSpec] = {
    "anthropic": _single("ANTHROPIC_API_KEY"),
    "openai": _single("OPENAI_API_KEY"),
    "xai": _single("XAI_API_KEY"),
    "deepseek": _single("DEEPSEEK_API_KEY"),
    "zai": _single("ZAI_API_KEY"),
    "openrouter": _single("OPENROUTER_API_KEY"),
    "gemini": _single("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    "google": _single("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    "vertex": _single("VERTEX_ACCESS_TOKEN"),
    # Explicit credentials only — see the module docstring for the AWS
    # credential sources deliberately excluded, and why.
    "bedrock": ProviderCredentialSpec(
        required_any=(("AWS_ACCESS_KEY_ID",), ("AWS_SECRET_ACCESS_KEY",)),
        optional=("AWS_SESSION_TOKEN",),
        # Required, not defaulted.  Stella's own chain falls back to
        # ``us-east-1``, which is right for a developer at a terminal and wrong
        # for a benchmark: a run whose region was never stated cannot be
        # reproduced, and the default would silently decide which AWS region
        # the published numbers came from.
        routing_any=(("AWS_REGION", "AWS_DEFAULT_REGION"),),
        order=("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"),
    ),
}

_AWS_CREDENTIAL_NAMES = frozenset(
    {
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_SECURITY_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AZURE_FEDERATED_TOKEN_FILE",
    }
)

# Harbor is a Python process which in turn starts Docker and task-side shells.
# Passing the caller's complete environment would let ambient interpreter,
# dynamic-loader, Git, Compose, or shell startup variables execute mutable code
# after the provider credential FD becomes inheritable.  Keep the host handoff
# deliberately small instead of trying to enumerate every dangerous spelling.
_HARBOR_ENV_ALLOWLIST = frozenset(
    {
        "HOME",
        "LOGNAME",
        "SHELL",
        "TEMP",
        "TERM",
        "TMP",
        "TMPDIR",
        "USER",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "DOCKER_CERT_PATH",
        "DOCKER_CONFIG",
        "DOCKER_CONTEXT",
        "DOCKER_HOST",
        "DOCKER_TLS_VERIFY",
        "STELLA_BINARY",
        "STELLA_BUDGET",
        "STELLA_DISABLE_REFLECTION",
        "STELLA_SOURCE_COMMIT",
    }
)


def is_credential_env_name(name: str) -> bool:
    """Mirror Stella's subprocess deny-list at the Harbor launch boundary."""
    upper = name.upper()
    return (
        upper in {"API_KEY", "TOKEN", "PASSWORD", "SECRET"}
        or upper.endswith(("_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"))
        or upper in _AWS_CREDENTIAL_NAMES
    )


def provider_credentials_from_environment(
    environ: Mapping[str, str],
) -> dict[str, str]:
    """Collect only supported provider credentials for the typed bundle."""
    credentials: dict[str, str] = {}
    for name in sorted(PROVIDER_CREDENTIAL_NAMES):
        value = environ.get(name)
        if value:
            credentials[name] = value
    return credentials


def credential_values_from_environment(environ: Mapping[str, str]) -> tuple[str, ...]:
    """Collect every provider or host-only control secret for byte scrubbing."""
    names = PROVIDER_CREDENTIAL_NAMES | HOST_ONLY_CONTROL_CREDENTIAL_NAMES
    return tuple(value for name in sorted(names) if (value := environ.get(name)))


@dataclass(frozen=True)
class SelectedProviderCredentials:
    """One route's resolved handoff: secrets, and the routing values beside them.

    ``credentials`` is ordered by the provider spec's declaration order,
    because the container pairs the descriptor's lines with the declared target
    names positionally.  ``routing`` is normalized to its canonical name — a
    region read from ``AWS_DEFAULT_REGION`` is reported as ``AWS_REGION``, so
    everything downstream has exactly one spelling to record and to disclose.
    """

    credentials: dict[str, str]
    routing: dict[str, str]

    def secret_values(self) -> tuple[str, ...]:
        """Every value that must never appear in evidence or a container Config."""
        return tuple(self.credentials.values())


def _select_one_of(
    environ: Mapping[str, str], group: tuple[str, ...], *, label: str
) -> tuple[str, str]:
    """Resolve exactly one configured name from an alias group."""
    present = [
        (name, value)
        for name in group
        if (value := environ.get(name)) is not None and value != ""
    ]
    if len(present) != 1:
        raise RuntimeError(
            f"selected model provider must have exactly one configured {label} from "
            + "/".join(group)
        )
    return present[0]


def provider_credentials_for_model(
    environ: Mapping[str, str], model: str
) -> SelectedProviderCredentials:
    """Select the credential set and routing values for a ``provider/model`` route.

    Fail-closed at every step: an unsupported provider, a missing member of a
    required group, or two spellings of the same alias all refuse the launch.
    A partially-resolved Bedrock route would otherwise reach the container as
    an access key id with no secret, sign nothing, and arrive as a provider
    403 several minutes into a trial.
    """
    provider, separator, model_id = model.partition("/")
    provider = provider.strip().lower()
    if not separator or not provider or not model_id.strip():
        raise RuntimeError("Harbor --model must be a literal provider/model route")
    spec = PROVIDER_CREDENTIAL_SPECS.get(provider)
    if spec is None:
        raise RuntimeError(
            "secure launcher does not support the selected model provider"
        )

    selected: dict[str, str] = {}
    for group in spec.required_any:
        name, value = _select_one_of(environ, group, label="credential")
        selected[name] = value
    for name in spec.optional:
        value = environ.get(name)
        if value:
            selected[name] = value

    routing: dict[str, str] = {}
    for group in spec.routing_any:
        _, value = _select_one_of(environ, group, label="routing value")
        # Canonical spelling is the group's first name, so a region sourced
        # from AWS_DEFAULT_REGION is still recorded and forwarded as AWS_REGION.
        routing[group[0]] = value

    ordered = {
        name: selected[name] for name in spec.ordered_names() if name in selected
    }
    # Every selected name must be placeable on the wire; a spec whose `order`
    # omits one of its own names would silently drop a secret.
    if set(ordered) != set(selected):
        raise RuntimeError(
            "provider credential spec declared a credential it cannot order on the wire"
        )
    return SelectedProviderCredentials(credentials=ordered, routing=routing)


@dataclass(frozen=True)
class RouteCredentialResolution:
    """What a route resolved to, and where it came from.

    ``handoff_target`` is the exact ``STELLA_CREDENTIAL_HANDOFF_TARGET`` value —
    the credential names, comma-separated, in the order their values are
    written to the pipe. ``None`` on the environment-fallback path, which makes
    no claim and sends no bundle.
    """

    selected: SelectedProviderCredentials
    source: str
    handoff_target: str | None


def select_route_credentials(
    model: str,
    *,
    lookup: Callable[[str], str | None],
    bundled: SelectedProviderCredentials | None,
    ambient_values: Sequence[str] = (),
) -> RouteCredentialResolution:
    """Resolve one ``provider/model`` route's credentials and routing values.

    Two paths, deliberately different in strictness:

    * **Bundle** — a secure launcher handed over an anonymous descriptor. The
      descriptor is the sole source of truth, so this refuses anything it did
      not expect (a credential the provider does not use, a required member
      missing, a routing value missing) and refuses any bundled secret that
      also appears somewhere ambient under a name of the copier's choosing.
    * **Environment fallback** — no bundle, no claim. First configured name in
      an alias group wins, exactly as it did before routes could need more than
      one secret: an operator with both ``GEMINI_API_KEY`` and
      ``GOOGLE_API_KEY`` exported has always been able to run here, and
      tightening it would break working local runs to protect a guarantee this
      path does not make.

    ``lookup`` resolves one variable name the way its caller resolves every
    other — for the adapter, Harbor's per-run overrides ahead of the ambient
    environment.
    """
    provider = model.split("/", 1)[0].strip().lower()
    spec = PROVIDER_CREDENTIAL_SPECS.get(provider)
    if spec is None:
        if provider != "local":
            raise RuntimeError(
                f"no secure provider-credential mapping for model provider `{provider}`"
            )
        # Config's local provider accepts an arbitrary nonempty key, but it
        # still travels through the same FD-only seam.
        spec = _single("LOCAL_API_KEY")
    candidates = spec.ordered_names()

    if bundled is not None:
        selected = {
            name: value
            for name in candidates
            if (value := bundled.credentials.get(name)) is not None
        }
        unexpected = sorted(set(bundled.credentials) - set(candidates))
        if unexpected:
            raise RuntimeError(
                "host credential bundle carries credentials the selected provider "
                "does not use: " + ", ".join(unexpected)
            )
        for group in spec.required_any:
            if sum(1 for name in group if name in selected) != 1:
                raise RuntimeError(
                    "host credential bundle must carry exactly one "
                    + "/".join(group)
                    + f" for provider `{provider}`"
                )
        for value in selected.values():
            if any(value and value in ambient for ambient in ambient_values):
                raise RuntimeError(
                    "selected provider credential is duplicated in Harbor's "
                    "ambient configuration; refusing claim-eligible execution"
                )
        routing = dict(bundled.routing)
        # The launcher canonicalizes each routing group to its first name, so
        # the bundle is checked for that spelling only.
        missing_routing = [
            group[0] for group in spec.routing_any if not routing.get(group[0])
        ]
        if missing_routing:
            raise RuntimeError(
                f"host credential bundle has no {', '.join(missing_routing)} for "
                f"provider `{provider}`"
            )
        ordered = {name: selected[name] for name in candidates if name in selected}
        return RouteCredentialResolution(
            selected=SelectedProviderCredentials(credentials=ordered, routing=routing),
            source=HOST_CREDENTIAL_SOURCE,
            handoff_target=",".join(ordered),
        )

    selected = {}
    for group in spec.required_any:
        for name in group:
            value = lookup(name)
            if value is None:
                continue
            if not value:
                raise RuntimeError(f"selected provider credential {name} is empty")
            selected[name] = value
            break
        else:
            if provider == "local":
                continue
            raise RuntimeError(
                f"selected provider `{provider}` requires one of: " + ", ".join(group)
            )
    for name in spec.optional:
        value = lookup(name)
        if value:
            selected[name] = value
    if not selected and provider == "local":
        selected = {"LOCAL_API_KEY": "local"}
    routing = {}
    for group in spec.routing_any:
        for name in group:
            value = lookup(name)
            if value:
                # Canonical spelling, so a region read from AWS_DEFAULT_REGION
                # is still recorded and forwarded as AWS_REGION.
                routing[group[0]] = value
                break
        else:
            raise RuntimeError(
                f"selected provider `{provider}` requires one of: " + ", ".join(group)
            )
    ordered = {name: selected[name] for name in candidates if name in selected}
    return RouteCredentialResolution(
        selected=SelectedProviderCredentials(credentials=ordered, routing=routing),
        source=ENV_CREDENTIAL_SOURCE,
        handoff_target=None,
    )


def sanitized_harbor_environment(
    environ: Mapping[str, str], bundle_fd: int
) -> dict[str, str]:
    """Allowlist launch variables and expose only the unlinked bundle FD number.

    Scrubbing only credential-shaped names is insufficient: a wrapper or CI
    system can duplicate a provider key under an arbitrary name, and runtime
    variables such as ``PYTHONPATH`` or ``DYLD_INSERT_LIBRARIES`` can execute
    caller-controlled code. Locale variables are the sole prefix-based family;
    every other surviving name is explicit. Values containing a bundled secret
    are removed too, without ever including the secret in diagnostics.
    """
    credential_values = credential_values_from_environment(environ)
    sanitized = {
        str(name): str(value)
        for name, value in environ.items()
        if (str(name) in _HARBOR_ENV_ALLOWLIST or str(name).startswith("LC_"))
        and not is_credential_env_name(str(name))
        and str(name) != HOST_CREDENTIAL_BUNDLE_FD_ENV
        and not any(secret in str(value) for secret in credential_values)
    }
    sanitized[HOST_CREDENTIAL_BUNDLE_FD_ENV] = str(bundle_fd)
    return sanitized


def create_anonymous_credential_bundle(
    credentials: Mapping[str, str],
    routing: Mapping[str, str] | None = None,
) -> BinaryIO:
    """Create and populate an unlinked mode-0600 seekable credential FD."""
    unknown = sorted(set(credentials) - PROVIDER_CREDENTIAL_NAMES)
    if unknown:
        raise RuntimeError(
            "credential bundle contains unsupported provider credential names: "
            + ", ".join(unknown)
        )
    if not credentials:
        raise RuntimeError("no supported provider credential is configured")
    routing = dict(routing or {})
    unknown_routing = sorted(set(routing) - PROVIDER_ROUTING_NAMES)
    if unknown_routing:
        raise RuntimeError(
            "credential bundle contains unsupported routing names: "
            + ", ".join(unknown_routing)
        )
    normalized: dict[str, str] = {}
    for name, value in credentials.items():
        if not isinstance(value, str) or not value:
            raise RuntimeError(f"provider credential {name} is empty or non-string")
        normalized[name] = value
    normalized_routing: dict[str, str] = {}
    for name, value in routing.items():
        if not isinstance(value, str) or not value:
            raise RuntimeError(f"provider routing value {name} is empty or non-string")
        normalized_routing[name] = value

    payload = json.dumps(
        {
            "schema_version": HOST_CREDENTIAL_BUNDLE_SCHEMA,
            "credentials": normalized,
            "routing": normalized_routing,
        },
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    if len(payload) > MAX_CREDENTIAL_BUNDLE_BYTES:
        raise RuntimeError("credential bundle exceeds the 256 KiB safety limit")

    # Ownership intentionally transfers to the caller and must survive exec.
    handle = tempfile.TemporaryFile(mode="w+b")  # noqa: SIM115
    try:
        fd = handle.fileno()
        os.fchmod(fd, 0o600)
        handle.write(payload)
        handle.flush()
        handle.seek(0)
        os.set_inheritable(fd, True)
        return handle
    except BaseException:
        handle.close()
        raise


def _object_without_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise RuntimeError("credential bundle contains duplicate JSON keys")
        value[key] = item
    return value


def read_anonymous_credential_bundle(fd_text: str) -> SelectedProviderCredentials:
    """Validate and pread a launcher-owned credential bundle without seeking."""
    try:
        fd = int(fd_text, 10)
    except (TypeError, ValueError) as exc:
        raise RuntimeError(
            f"{HOST_CREDENTIAL_BUNDLE_FD_ENV} must be a non-negative descriptor"
        ) from exc
    if fd < 0:
        raise RuntimeError(
            f"{HOST_CREDENTIAL_BUNDLE_FD_ENV} must be a non-negative descriptor"
        )

    try:
        info = os.fstat(fd)
    except OSError as exc:
        raise RuntimeError("host credential bundle descriptor is not open") from exc
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 0:
        raise RuntimeError(
            "host credential bundle must be an unlinked regular temporary file"
        )
    if stat.S_IMODE(info.st_mode) & 0o077:
        raise RuntimeError("host credential bundle must be owner-only mode 0600")
    if not os.get_inheritable(fd):
        raise RuntimeError("host credential bundle descriptor is not inheritable")

    try:
        payload = os.pread(fd, MAX_CREDENTIAL_BUNDLE_BYTES + 1, 0)
    except OSError as exc:
        raise RuntimeError("host credential bundle descriptor is not seekable") from exc
    if len(payload) > MAX_CREDENTIAL_BUNDLE_BYTES:
        raise RuntimeError("credential bundle exceeds the 256 KiB safety limit")
    try:
        decoded = json.loads(
            payload.decode("utf-8"), object_pairs_hook=_object_without_duplicate_keys
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RuntimeError(
            "host credential bundle is not valid canonical JSON"
        ) from exc
    if not isinstance(decoded, dict) or set(decoded) != {
        "schema_version",
        "credentials",
        "routing",
    }:
        raise RuntimeError("host credential bundle has an invalid top-level shape")
    if decoded["schema_version"] != HOST_CREDENTIAL_BUNDLE_SCHEMA:
        raise RuntimeError("host credential bundle has an unsupported schema version")
    credentials = decoded["credentials"]
    if not isinstance(credentials, dict) or not credentials:
        raise RuntimeError("host credential bundle has no provider credentials")
    unknown = sorted(set(credentials) - PROVIDER_CREDENTIAL_NAMES)
    if unknown:
        raise RuntimeError(
            "host credential bundle contains unsupported provider credential names"
        )
    for name, value in credentials.items():
        if not isinstance(name, str) or not isinstance(value, str) or not value:
            raise RuntimeError("host credential bundle contains an invalid credential")
    routing = decoded["routing"]
    # Present-but-empty is the normal case for every provider but Bedrock; the
    # key itself is mandatory so a v1-shaped bundle cannot be read as a v2 one
    # with its region silently missing.
    if not isinstance(routing, dict):
        raise RuntimeError("host credential bundle has an invalid routing section")
    unknown_routing = sorted(set(routing) - PROVIDER_ROUTING_NAMES)
    if unknown_routing:
        raise RuntimeError("host credential bundle contains unsupported routing names")
    for name, value in routing.items():
        if not isinstance(name, str) or not isinstance(value, str) or not value:
            raise RuntimeError(
                "host credential bundle contains an invalid routing value"
            )
    return SelectedProviderCredentials(credentials=credentials, routing=routing)


def read_bundle_from_environment(
    environ: Mapping[str, str],
) -> SelectedProviderCredentials | None:
    """Read the host bundle named by ``environ`` or return None for fallback use."""
    fd_text = environ.get(HOST_CREDENTIAL_BUNDLE_FD_ENV)
    if fd_text is None:
        return None
    return read_anonymous_credential_bundle(fd_text)
