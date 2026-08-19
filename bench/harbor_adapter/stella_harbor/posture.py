"""The frozen engine posture a benchmark trial runs under, and its arms.

Split out of the adapter package root when that file passed its size ceiling
(`scripts/check-file-size.sh`): the repository's rule is that separable new
code becomes its own module rather than more length on an already-oversized
file. It is a genuine seam — everything here is a pure function of the selected
model and the chosen arm, with no Harbor, Docker, or credential dependency, so
`make_manifest.py`, `secure_launcher.py`, `tb21_preregistration.py`, and the
adapter all derive their hashes from one implementation rather than from copies
that can disagree.

Two hashed objects live here:

* the **engine posture** — the exact `agent_engine_config` JSON delivered
  through Stella's trusted launcher seam, and
* the **assurance declaration** — which rungs of the verification ladder that
  posture exercises (#1007).

They are separate because the launcher seam
(`config::trusted_engine_config_shape_is_strict`) fails *closed* on any root key
outside `settings::ENGINE_ROOT_FIELDS`, so a descriptive `tiers` field inside
the posture would refuse the run rather than annotate it.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Callable
from typing import Any

#: Signature of an engine-posture builder: ``(model, *, verifier)`` ->
#: ``(posture, canonical_json, sha256)``. `_benchmark_engine_posture` is the
#: frozen claim implementation; `StellaAgent._build_engine_posture` is the seam
#: a non-claim harness may override with another frozen configuration.
PostureBuilder = Callable[..., "tuple[dict[str, Any], str, str]"]

_ENGINE_POSTURE_VERSION = "stella-tb21-engine-posture-v1"
# v2: the declaration derives from the RESOLVED engine posture rather than from
# the host-side witness-author selector, resolves each role through the engine's
# own fallback chain (`agents.<role>.model` > flat key > `default_model`), and
# states an off-reason naming the models both roles resolved to and the keys
# they came from — instead of asserting "every role inherits default_model", a
# sentence that was false whenever any other role carried a pin (#2134). v1
# declarations recorded before this change keep describing what their runs
# computed.
_ASSURANCE_TIERS_VERSION = "stella-tb21-assurance-tiers-v2"

# Host-side selector for the witness/verifier author. Unset (the default) is the
# control arm: one inherited model, authored witness structurally off. Set to a
# second `provider/model` on the worker's provider and the same 89 tasks run the
# treatment arm. Never forwarded into the container — the decision reaches
# Stella only as `pipeline_verifier_model` inside the hashed posture, so the arm a
# trial ran cannot disagree with the arm its digest records (#1007).
_WITNESS_AUTHOR_ENV = "STELLA_WITNESS_AUTHOR_MODEL"

# Host-side selector for the worker's effort tier. The rule that sets it has
# not changed — spend what the comparator spends — but the comparator is no
# longer fixed, so the tier stops being a constant and becomes an arm. Both
# admissible values are leaderboard-meaningful tiers rather than free text, and
# the choice lands in the posture hash, so two runs at different efforts can
# never share a digest.
_WORKER_EFFORT_ENV = "STELLA_WORKER_EFFORT"
_ADMISSIBLE_WORKER_EFFORTS = ("high", "xhigh", "max")

# Host-side selector for the triage author. Triage classifies the request and
# builds a prompt; it never edits the workspace and never decides the outcome,
# so it is the one role where a cheaper, faster, non-reasoning model is a
# design choice rather than a handicap. Unset keeps the historical behaviour
# (triage inherits `default_model`), so every digest recorded before this
# selector existed still describes the posture that produced it.
_TRIAGE_MODEL_ENV = "STELLA_TRIAGE_MODEL"

# Host-side selectors for the two read-only pipeline roles. Research greps a
# tree and reports what it found; plan writes the one work order every later
# stage is judged against. Until #2553 neither could be configured at all — the
# engine had no `research`/`plan` surface, so the roles resolved purely by
# inheritance and there was nothing for a posture to select.
#
# What they inherit is the WORKER, field by field, not the `default` row: see
# `over_worker` in `crates/stella-cli/src/agent/engine.rs`, which merges each
# role's own tuning over the worker's and leaves an unset field on the worker's
# value. So a claim run that pins `agents.worker.effort = xhigh` pins research
# to `xhigh` with it. On match `7d025330abad` that was 76s across 15 research
# calls to emit a few hundred reasoning tokens, against 8.5s for the single
# plan call. #2553 built the knob that turns that down; this is the harness
# reaching it.
#
# Unset omits the row, and here that rule carries more weight than anywhere
# else in this file. The digest identifies every registered arm
# (`bench/READINESS.md` §8.4.4), so emitting two rows unconditionally would
# re-hash every arm already registered in order to describe postures that
# behave exactly as the recorded ones did — spending the comparability that
# digest exists to provide, for nothing. A rename did precisely that once
# (#1394) and ended cross-boundary comparison; this is the same hazard with a
# cheaper answer, because a row nobody asked for is a row that need not exist.
_RESEARCH_EFFORT_ENV = "STELLA_RESEARCH_EFFORT"
_RESEARCH_REASONING_ENV = "STELLA_RESEARCH_REASONING"
_RESEARCH_MODEL_ENV = "STELLA_RESEARCH_MODEL"
_PLAN_EFFORT_ENV = "STELLA_PLAN_EFFORT"
_PLAN_REASONING_ENV = "STELLA_PLAN_REASONING"
_PLAN_MODEL_ENV = "STELLA_PLAN_MODEL"

# All five tiers, where the worker admits three. The worker's list is short
# because its rule is parity — spend what the comparator spends — and a
# head-to-head worker at `low` is not a posture anyone would register. These
# two roles have the opposite rule: the reason to configure them at all is to
# spend *less* than the worker, so the tiers below it are the ones that matter.
# `max` stays admissible because refusing a measurement someone might want is
# not this file's job; the enum exists to catch a typo, not to pick an arm.
_ADMISSIBLE_ROLE_EFFORTS = ("low", "medium", "high", "xhigh", "max")

# The worker's frozen `reasoning` value, named once because two places depend
# on it meaning "what research and plan inherit today": the `worker` row below
# and `_role_row`, which uses it as the value an unselected `reasoning` takes so
# that emitting a row changes nothing an operator did not ask for.
_WORKER_REASONING = "on"

# `STELLA_MAX_REVISIONS` and `STELLA_CANDIDATES` used to select the two
# pipeline knobs deciding how many times a task may be attempted — revision
# turns after a failed verification, and best-of-N candidate executions (#1211
# §6.7, §6.8, wired end-to-end by #2600). Both are gone (#3871): the staged
# pipeline that read them was deleted with `crates/stella-pipeline` (#3865), and
# `agent_engine_config` no longer carries either key.
#
# They are deleted here rather than accepted-and-ignored, and the difference is
# the whole point. `POSTURE_SELECTOR_ENV` below is spliced into the adapter's
# `_HOST_ONLY_STELLA_ENV`, whose ambient check fails **closed** — so an operator
# whose arm still exports one now gets a refused run naming the unregistered
# variable, instead of a posture Stella's trusted-launcher seam rejects at
# launch with an engine-config error that does not name the knob. A benchmark
# knob that silently selects nothing is the failure CLAUDE.md's measure-honestly
# rule exists to prevent, and a run refused early is far cheaper than a result
# that quietly measured the default.

# Host-side selector for the third coupled ceiling. The other two have been
# selectable for generations — the output cap rides `params.max_tokens` in this
# very dict, and the turn budget is a per-trial flag — but `model_timeout` was
# an `EngineConfig` constant, reachable from no configuration at all. That made
# it the one ceiling a benchmark arm could not move without rebuilding the
# binary, which under this protocol is a re-freeze of the registered SUT rather
# than a line in a posture (#1211 §6.2).
#
# It matters because the three are one budget. Raising the output cap alone
# relocates the cliff instead of removing it: a step allowed 128,000 tokens
# against a timeout sized for 64,000 stops on the timeout, and the trial reports
# a capability difference that was really a ceiling nobody scaled. That is not
# hypothetical here — it is the recorded history of this posture, where
# 16384 -> 32000 -> 64000 each moved one ceiling while the others held.
#
# Unset omits the key, so every digest recorded before this selector existed
# still describes the posture that produced it.
_MODEL_TIMEOUT_ENV = "STELLA_MODEL_TIMEOUT"

# `STELLA_VERIFIER_EVIDENCE_DEMAND` selected the corroboration ask (#1295):
# when a model verifier passed and nothing deterministic stood behind it, did
# the pipeline spend one revision demanding the evidence, or record the pass as
# unverified on the spot? Gone for the same reason as the two above (#3871) —
# the staged pipeline that raised the ask was deleted (#3865), and verification
# makes no model call to corroborate. Its measured result is kept on the record
# here because it is the answer, not a gap: the ask fired on nearly every
# Terminal-Bench turn, since those turns had no tracked command and therefore no
# way to satisfy it, so it bought a turn everywhere and evidence nowhere.

# Six hours, the same refusal ceiling `stella-serve` applies to the knob over
# the wire. Sized to sit far above any single generation a model produces
# today while keeping a fat-fingered digit from parking a trial on an
# effectively unbounded await — which on a benchmark spends the whole agent
# timeout and reports it as a task failure.
_MODEL_TIMEOUT_CEILING = 21_600

# There is no output-cap table here any more, and its absence is load-bearing.
#
# It held one number per model, pinned against `catalog.rs` by
# `TestOutputCeilingParity` so the two could not drift. That guarded the right
# hazard with the wrong instrument: the safe value of a benchmark output cap is
# always exactly the model's ceiling, and a table whose every correct entry is
# a copy of another table is a synchronisation problem invented for no gain.
# Sending nothing gets the same number from the authority itself (#2411).
#
# What survives is the check that the authority HAS a number:
# `test_every_bookable_model_has_a_seeded_ceiling`. A booked model missing its
# catalog ceiling falls back to the engine's global 16384, which is the one way
# an uncapped posture can still run capped.

# The models an arm can actually book, by bare slug: worker, the two verifiers,
# and triage. `TestOutputCeilingParity` checks the caps above against
# `catalog.rs` for THESE and no others.
#
# Scoped rather than "every seeded model" because the two tables answer
# different questions. The catalog says what a model can write — every model,
# whether or not a benchmark ever touches it. This file says what an arm asks
# for, which only means anything for a model an arm books. Checking the rest
# would force a mirrored cap for models nobody measured, and each one would
# need a `_MODEL_TIMEOUT_BY_SLUG` entry derived from an observed token rate
# that does not exist — manufacturing precisely the cap-without-timeout
# mismatch the table below exists to prevent.
_BENCHMARKED_SLUGS = frozenset(
    {
        "claude-sonnet-5",
        "claude-fable-5",
        "kimi-k3",
        "claude-haiku-4.5",
    }
)

# There is no sub-ceiling rationale map either, and it is worth recording what
# it taught before it went.
#
# It let a cap sit below the model's ceiling when someone wrote down why, and
# both of its entries were honest. The Sonnet 5 one even had the strongest
# argument available: cap at 64,000 because that is where the comparator's
# steps were measured stopping, and matching the other side is what a
# head-to-head is for.
#
# It is still the wrong shape. "Where the comparator was measured stopping" is
# not a ceiling the comparator was given — Claude Code is handed none — it is
# where a model chose to stop on the tasks someone happened to measure. Turning
# that observation into OUR limit is how a measurement becomes a handicap while
# every comment around it still reads as deliberate. A rationale map cannot
# catch that, because a well-argued entry is exactly what it is designed to
# accept (#2411).

# The timeout is DERIVED from the cap, not chosen independently, which is why
# it lives in the same table. It bounds silence between stream fragments, so it
# has to exceed the time the model needs to produce a full-cap answer at its
# observed speed. The registered derivation: the comparator's rewarded 64,000
# token step took 756s (~85 tokens/second), so 128,000 tokens is ~1,512s, plus
# the same 60s margin every previous ceiling used = 1,572s.
#
# `None` means "leave it out and inherit the engine default", which is what
# every model but Fable does and what every historical run recorded.
#
# Raising the cap without raising this is the mistake this table exists to
# prevent: the step then stops on the timeout instead of the cap, which looks
# identical in the results and is just as much us stopping first. That is not
# hypothetical — it is the recorded history of this posture, where
# 16384 -> 32000 -> 64000 each moved one ceiling while the others held.
_MODEL_TIMEOUT_BY_SLUG = {"claude-fable-5": 1_572}

#: What "no row above" actually runs under: the engine seeds
#: ``model_timeout: Some(Duration::from_secs(816))`` in
#: ``crates/stella-core/src/driver.rs``, and it bounds IDLE SILENCE between
#: stream fragments, never elapsed time — a generation that keeps streaming
#: is never cut by it. Named here because #2021 was filed, implemented, and
#: reverted on the belief that an absent row meant "no ceiling"; the number
#: a reader needs was findable only in the Rust tree. A parity test pins
#: this against the driver's literal.
_ENGINE_DEFAULT_MODEL_TIMEOUT_SECS = 816

# A benchmarked slug with NO `_MODEL_TIMEOUT_BY_SLUG` row inherits the 816s
# engine default — and that inherit must read as a decision, not an omission
# (#2070). Same pattern the removed sub-ceiling map used: every silence is
# written down with its reason, so the next slug added to
# `_BENCHMARKED_SLUGS` fails the suite until someone decides. Deliberately
# NOT a posture row: writing `model_timeout_secs: 816` explicitly was
# implemented, measured as a behavioural no-op, and reverted (#2021's
# refutation), because it changes the registered posture digest in exchange
# for nothing. These sentences cost no digest.
_INHERITED_TIMEOUT_RATIONALE: dict[str, str] = {
    "claude-sonnet-5": (
        "Inherits the 816s idle-silence default. As the head-to-head worker "
        "its 64,000-token cap takes ~756s to fill at the registered "
        "84.66 tok/s, and the silence ceiling only cuts a stream that has "
        "STOPPED producing — no measured Sonnet call has stalled anywhere "
        "near 816s of silence. Writing 816 as a posture row would invalidate "
        "the registered digest (c8536200…) for zero behaviour change."
    ),
    "kimi-k3": (
        "Inherits the 816s idle-silence default. Booked as arm B's VERIFIER: "
        "it emits a verdict, not a solution, so neither the cap nor the "
        "timeout has ever bound — and this model has no observed token rate "
        "to derive a bespoke ceiling from. A manufactured row is exactly the "
        "cap-without-timeout mismatch the table above exists to prevent."
    ),
    "claude-haiku-4.5": (
        "Inherits the 816s idle-silence default. Booked as the TRIAGE "
        "author: a short classification at effort low / reasoning off, "
        "finished in seconds — the ceiling is three orders of magnitude "
        "above anything this role produces."
    ),
}


def _model_slug(model: str) -> str:
    """The bare model name, with any provider or gateway prefix stripped."""
    return model.rsplit("/", 1)[-1].strip().lower()




def default_model_timeout(model: str) -> int | None:
    """The silence ceiling that goes with this model's cap, or ``None``."""
    return _MODEL_TIMEOUT_BY_SLUG.get(_model_slug(model))


def _validated_attempt_count(
    value: str, *, label: str, floor: int, ceiling: int
) -> int:
    """Parse a host-side attempt-count selector, or refuse it with the reason.

    Fails closed on every non-value — empty, non-numeric, out of range — for
    the same reason `_validated_worker_effort` does: silently inheriting the
    default would attribute a run to a configuration nobody chose, and the
    digest would then agree with the typo rather than with reality.
    """
    text = value.strip()
    if not text:
        raise ValueError(
            f"benchmark {label} must not be empty: an empty selector means a "
            f"value was lost on its way here, and inheriting the default would "
            f"score the run under a configuration nobody chose"
        )
    try:
        parsed = int(text)
    except ValueError as exc:
        raise ValueError(
            f"benchmark {label} must be an integer; got `{value}`"
        ) from exc
    if parsed < floor or parsed > ceiling:
        raise ValueError(
            f"benchmark {label} must be between {floor} and {ceiling}; "
            f"got {parsed}"
        )
    return parsed


def _validated_toggle(value: str, *, label: str) -> bool:
    """Parse a host-side on/off selector, or refuse it with the reason.

    Fails closed on anything that is not one of the four accepted spellings,
    for the reason every selector here does: a run scored under a configuration
    nobody chose is worse than a run that refused to start. In particular a
    bare ``"false"`` is *not* silently read as off — the accepted vocabulary is
    the one the settings themselves use (``on``/``off``), plus ``1``/``0`` for
    the shell that has an integer to hand.
    """
    text = value.strip().lower()
    if text in ("on", "1"):
        return True
    if text in ("off", "0"):
        return False
    raise ValueError(f"benchmark {label} must be one of on/off/1/0; got `{value}`")


def resolve_role_effort(value: str | None, *, role: str) -> str | None:
    """Resolve a read-only role's effort tier, or ``None`` to emit no row.

    Unlike the worker's tier this has no frozen default to fall back to, and
    that asymmetry is the design: absent means *omit the row entirely*, which is
    the posture every registered arm was hashed under. An explicitly empty value
    is still refused, for the reason `resolve_worker_effort` refuses one — it
    means an operator meant to select an arm and the value was lost on the way
    here, which is exactly when inheriting silently would attribute the run to a
    tier nobody chose.
    """
    if value is None:
        return None
    return _validated_role_effort(value, role)


def resolve_role_reasoning(value: str | None, *, role: str) -> bool | None:
    """Resolve a read-only role's thinking toggle, or ``None`` to emit no row.

    The tier is only half of what makes research expensive. Turning effort down
    while `reasoning` still rides the worker's `on` leaves the role thinking on
    every call, which on a lookup that reports what it read is most of the cost
    #2553 measured — so the two are selectable separately, and either one alone
    materialises the row.
    """
    if value is None:
        return None
    return _validated_toggle(value, label=f"{role} reasoning")


def resolve_role_model(value: str | None) -> str | None:
    """Resolve a per-role model pin, or ``None`` to inherit.

    Whitespace-only is treated as unset, because unlike the effort tier "no pin"
    is a meaningful and previously-default configuration for every role that has
    one — so an empty value cannot be the lost-arm case that refusal exists to
    catch.
    """
    if value is None:
        return None
    return value.strip() or None


def resolve_model_timeout(value: str | None) -> int | None:
    """Resolve the per-generation silence ceiling in seconds, or ``None``.

    ``0`` is admissible and means *no backstop* — the unbounded await the
    engine's ``Option::None`` spells. It is accepted rather than refused
    because "no ceiling" is a real request that a floor of one could not
    express, and it is distinct from leaving the selector unset, which asks for
    the engine's own default instead.
    """
    if value is None:
        return None
    return _validated_attempt_count(
        value, label="model timeout", floor=0, ceiling=_MODEL_TIMEOUT_CEILING
    )


def resolve_worker_effort(value: str | None) -> str:
    """Resolve the worker's effort tier from its host-side selector.

    ``None`` (unset) reproduces every posture hash recorded before the tier
    became selectable. An explicitly *empty* value is refused rather than
    quietly treated as unset: it means an operator meant to select an arm and
    the value was lost somewhere, which is exactly the case where inheriting
    the frozen default would attribute the run to a tier nobody chose.
    """
    if value is None:
        return "xhigh"
    return _validated_worker_effort(value)


def resolve_triage_model(value: str | None) -> str | None:
    """Resolve the triage author pin, or ``None`` to inherit the worker.

    Same shape as the witness author: the pin has to be asked for, so a tree
    that merely carries this code keeps producing the historical posture.
    """
    return resolve_role_model(value)


#: Every host-side posture selector, in one tuple.
#:
#: The adapter registers these as host-only environment (`_HOST_ONLY_STELLA_ENV`)
#: and its ambient check fails **closed**, so a selector this module reads and
#: that list does not name refuses the run instead of enabling the arm — an
#: unregistered `STELLA_TURN_TIMEOUT` once killed all ten trials of a run. One
#: tuple, unpacked there, is what keeps the two lists from being two lists.
#: The witness author is deliberately absent: it reaches the builder as
#: ``verifier=``, resolved by `StellaAgent._verifier_model`, not through here.
POSTURE_SELECTOR_ENV = (
    _WORKER_EFFORT_ENV,
    _TRIAGE_MODEL_ENV,
    _RESEARCH_EFFORT_ENV,
    _RESEARCH_REASONING_ENV,
    _RESEARCH_MODEL_ENV,
    _PLAN_EFFORT_ENV,
    _PLAN_REASONING_ENV,
    _PLAN_MODEL_ENV,
    _MODEL_TIMEOUT_ENV,
)


def read_posture_selectors(get: Callable[[str], str | None]) -> dict[str, Any]:
    """Resolve every host-side selector into `_benchmark_engine_posture` kwargs.

    ``get`` reads one configured value by name — `StellaAgent._configured_value`
    on the adapter, a plain dict lookup in a test.

    It lives beside the selectors rather than at the call site because the call
    site is reached **twice**: once when trial metadata is collected, and again
    when the exec boundary recomputes the posture and refuses the run unless it
    matches byte for byte. Two spellings of "read the selectors" is two chances
    for those to disagree, and a disagreement there is a refused run at best.
    Keeping the list here also keeps it in one place with `POSTURE_SELECTOR_ENV`,
    which is the half the ambient check reads.
    """
    return {
        "worker_effort": resolve_worker_effort(get(_WORKER_EFFORT_ENV)),
        "triage_model": resolve_triage_model(get(_TRIAGE_MODEL_ENV)),
        "research_effort": resolve_role_effort(
            get(_RESEARCH_EFFORT_ENV), role="research"
        ),
        "research_reasoning": resolve_role_reasoning(
            get(_RESEARCH_REASONING_ENV), role="research"
        ),
        "research_model": resolve_role_model(get(_RESEARCH_MODEL_ENV)),
        "plan_effort": resolve_role_effort(get(_PLAN_EFFORT_ENV), role="plan"),
        "plan_reasoning": resolve_role_reasoning(
            get(_PLAN_REASONING_ENV), role="plan"
        ),
        "plan_model": resolve_role_model(get(_PLAN_MODEL_ENV)),
        "model_timeout_secs": resolve_model_timeout(get(_MODEL_TIMEOUT_ENV)),
    }


def _validated_role_model(model: str, candidate: str, role: str) -> str:
    """Return a validated per-role model pin, or refuse it with the reason.

    The provider rule is the same one `_validated_verifier` enforces and
    it is enforced separately rather than shared, because the two roles fail
    differently: an unreachable verifier silently degrades the *claim* (the run
    proceeds with the worker as its own author), while an unreachable triage
    model fails the *call*. Both are refusals here, before anything is spent.
    """
    pin = candidate.strip()
    if not pin or "/" not in pin:
        raise ValueError(
            f"benchmark {role} model must be a non-empty provider/model spec"
        )
    worker_provider = model.split("/", 1)[0].strip().lower()
    pin_provider = pin.split("/", 1)[0].strip().lower()
    if worker_provider != pin_provider:
        raise ValueError(
            f"benchmark {role} model must share the worker's provider "
            f"(`{worker_provider}`): a trial carries exactly one provider "
            f"credential over the anonymous FD, so a {role} model on "
            f"`{pin_provider}` would authenticate against nothing"
        )
    return pin


def _validated_worker_effort(worker_effort: str) -> str:
    """Return the worker effort tier, or refuse an unrecognised one.

    Fails closed rather than falling back to a default: a typo that silently
    became `xhigh` would produce a number attributed to an effort the run never
    used, and the digest would agree with the typo rather than with reality.
    """
    effort = worker_effort.strip().lower()
    if effort not in _ADMISSIBLE_WORKER_EFFORTS:
        raise ValueError(
            f"benchmark worker effort must be one of "
            f"{', '.join(_ADMISSIBLE_WORKER_EFFORTS)}; got `{worker_effort}`"
        )
    return effort


def _validated_role_effort(value: str, role: str) -> str:
    """Return a read-only role's effort tier, or refuse an unrecognised one.

    Same fail-closed rule as the worker's tier over a wider vocabulary, and the
    refusal matters more here rather than less: the tiers this admits and the
    worker's do not overlap on the low end, so a typo that fell back to the
    worker would produce a run measuring the exact configuration the arm was
    selected to move away from.
    """
    effort = value.strip().lower()
    if effort not in _ADMISSIBLE_ROLE_EFFORTS:
        raise ValueError(
            f"benchmark {role} effort must be one of "
            f"{', '.join(_ADMISSIBLE_ROLE_EFFORTS)}; got `{value}`"
        )
    return effort


def _role_row(
    *, role: str, effort: str | None, reasoning: bool | None, worker_effort: str
) -> dict[str, str] | None:
    """The ``agents.<role>`` row for a read-only role, or ``None`` to omit it.

    Emitted whole or not at all, and the unselected half takes the value the
    role inherits *today* — the worker's tier, and the worker's ``reasoning:
    on``. Two properties follow, and both are the point:

    * the row's mere presence is a behavioural no-op, so a one-knob arm moves
      one variable rather than two; and
    * every row this writes has the same two fields as every other role's,
      which is what lets the manifest schema keep validating an exact shape
      instead of accepting whatever a partial row happened to carry.

    It validates rather than trusting its caller, for the reason
    ``_validated_worker_effort`` is called inside the builder and not beside the
    selector: the builder is the one function *both* the collection path and the
    exec-boundary recompute call, so a check anywhere else is a check one of
    them can skip.
    """
    if effort is None and reasoning is None:
        return None
    if reasoning is not None and not isinstance(reasoning, bool):
        raise ValueError(
            f"benchmark {role} reasoning must be a bool or None; got {reasoning!r}"
        )
    thinking = _WORKER_REASONING == "on" if reasoning is None else reasoning
    return {
        "effort": (
            worker_effort if effort is None else _validated_role_effort(effort, role)
        ),
        "reasoning": "on" if thinking else "off",
    }


def _pin_role_model(
    posture: dict[str, Any], *, model: str, pin: str, role: str
) -> None:
    """Pin one role's model in its flat key and widen the model vocabulary.

    The flat root key, never ``agents.<role>.model``. Both resolve — the engine
    reads ``agents.<role>.model`` first and falls through to the flat key
    (``AgentEngineConfig::model_for``) — but the flat key is what
    ``settings_check`` and ``stella config`` report as the role's origin, so the
    disclosed posture and the engine's own account of its wiring name the same
    field.

    ``allowed_models`` has to widen with it, and that half is load-bearing: the
    vocabulary is a whitelist, so a pin outside it is refused at resolve time
    and the role drops back to whatever it inherits — billing one model while
    the digest claims another.
    """
    validated = _validated_role_model(model, pin, role)
    posture[_FLAT_ROLE_MODEL_KEY[role]] = validated
    allowed = list(posture["allowed_models"])
    if validated not in allowed:
        allowed.append(validated)
    posture["allowed_models"] = allowed


def _validated_verifier(model: str, verifier: str) -> str:
    """Return the pinned witness/verifier author, or refuse it with the reason.

    Three fail-closed conditions, because a treatment arm that quietly
    degrades into the control arm is precisely the failure #1007 is about:
    the number would be produced by a posture the manifest does not describe.

    The provider check is the non-obvious one. A trial receives exactly one
    provider credential, over the anonymous FD (`_selected_provider_credential`
    resolves it from the *worker* model's provider), so an author on a second
    provider has nothing to authenticate with and would fail every call. Pin
    both roles inside one provider — the protocol's roster is entirely
    ``openrouter/…`` for this reason.
    """
    author = verifier.strip()
    if not author or "/" not in author:
        raise ValueError(
            "benchmark witness author must be a non-empty provider/model spec"
        )
    if author == model:
        raise ValueError(
            "benchmark witness author must differ from the worker model: Stella "
            "requires an author independent of the worker, so pinning the "
            "worker's own model leaves the witness tier off while changing the "
            "posture hash — the worst of both arms"
        )
    worker_provider = model.split("/", 1)[0].strip().lower()
    author_provider = author.split("/", 1)[0].strip().lower()
    if worker_provider != author_provider:
        raise ValueError(
            "benchmark witness author must share the worker's provider "
            f"(`{worker_provider}`): a trial carries exactly one provider "
            f"credential over the anonymous FD, so an author on "
            f"`{author_provider}` would authenticate against nothing"
        )
    return author


def _benchmark_engine_posture(
    model: str,
    *,
    verifier: str | None = None,
    worker_effort: str = "xhigh",
    triage_model: str | None = None,
    research_effort: str | None = None,
    research_reasoning: bool | None = None,
    research_model: str | None = None,
    plan_effort: str | None = None,
    plan_reasoning: bool | None = None,
    plan_model: str | None = None,
    model_timeout_secs: int | None = None,
) -> tuple[dict[str, Any], str, str]:
    """Return a canonical Terminal-Bench engine posture and its hash.

    Request posture is explicit per role so ordinary auto-mode defaults cannot
    drift across Stella versions. The normalized JSON is the exact value
    delivered through the trusted launcher override consumed by the CLI.

    Two arms, and which one a run used is a property of the hash rather than of
    the logs (#1007):

    * ``verifier=None`` — the **control arm**. Model routing is expressed
      only by ``default_model``; every role inherits it and no role has a
      provider/model override. Stella will not let the worker write the test
      that verifies it, so with one model for every role the independent author
      never exists: the run reports ``WitnessUnavailable`` and proceeds unproven
      (#973). Every Terminal-Bench number published before #1007 is this arm.
    * ``verifier="provider/slug"`` — the **treatment arm**. A second model
      is pinned for the verifier role via ``pipeline_verifier_model``, which is what
      the authored-witness tier resolves its author from, and ``allowed_models``
      widens to name both. Still fully pinned, still one hash, still no
      auto-selection — the reproducibility argument is untouched; the posture
      simply now says which of two frozen configurations it is.

    Both arms are frozen and disclosed. Choosing between them is a measurement
    decision that changes ``digest``, and therefore the registered SUT; the
    point of having both is that the choice is made in the manifest instead of
    being discovered by grepping a trajectory for a warning line.

    ``max_revisions``, ``candidates`` and ``verifier_evidence_demand`` were
    three more arguments of the same shape (#1211 §6.7, §6.8; #1295). They are
    gone (#3871): the staged pipeline that read all three was deleted with
    ``crates/stella-pipeline`` (#3865), so the keys they emitted are no longer
    in the engine's vocabulary and are refused by the trusted-launcher seam
    rather than ignored. Passing one is now a ``TypeError`` here — at the call
    site, naming the argument — rather than a run that starts and measures the
    default.

    ``model_timeout_secs`` is the same shape again, for the ceiling that used to
    be the exception (#1211 §6.2). It is the last per-generation ceiling this
    posture still sets: the output cap and the turn budget are both gone
    (#2411), leaving the model's catalog maximum and the task's own wall clock.
    It is a key like any other — selecting it changes the digest, leaving it
    unset reproduces every historical one.

    The six ``research_*``/``plan_*`` arguments are the same rule applied to the
    two read-only roles (#2549). All six default to ``None``, which emits
    neither an ``agents`` row nor a flat key, so a tree that merely carries this
    code still returns byte-identical JSON — and therefore an identical digest —
    for every arm registered before these roles were configurable. Selecting any
    of them is a posture change that says so in the digest, which is the only
    way the registered numbers keep describing the postures that produced them.
    """
    selected_model = model.strip()
    if not selected_model or "/" not in selected_model:
        raise ValueError("benchmark model must be a non-empty provider/model spec")
    selected_effort = _validated_worker_effort(worker_effort)
    posture: dict[str, Any] = {
        "default_model": selected_model,
        "allowed_models": [selected_model],
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        # A task container is disposable and the budget cap is the real guard,
        # so scope review has nothing to protect here and nobody to ask. Left
        # off, any plan over the thresholds (more than 5 steps) ends the run.
        "headless_scope_bypass": "on",
        # `xhigh` for every role the outcome depends on, and the rule that
        # picks it has not changed — only the comparator has. The rule is:
        # spend what the other side spends, because the leaderboard carries
        # `high`, `xhigh` and `max` as distinct values, so a mismatch is less
        # compute applied to one side rather than a naming variation. Against
        # "Claude Code using GLM-5.1 at **max effort**" that rule said `max`,
        # and every Terminal-Bench number published before then was produced
        # at `high` — under that handicap.
        #
        # The Sonnet-5 comparator is Claude Code on the first-party Anthropic
        # API, which runs `xhigh` by default, so the same rule now says
        # `xhigh`. Reading `max` as "more is safer" would invert it: `max`
        # here would hand Stella compute the comparator never gets, and
        # Anthropic documents `xhigh` — not `max` — as the setting for coding
        # and agentic work, with `max` prone to overthinking. Parity and the
        # model's own guidance agree.
        #
        # Changing this constant changes the posture digest, and therefore the
        # registered SUT. That is the intended way to make the change — a run
        # states which frozen posture it used in its manifest — but it does
        # mean digests recorded against the `max` posture (bench/READINESS.md)
        # describe the earlier arm, not this one.
        #
        # `triage` deliberately stays low/off, and that is parity rather than an
        # exception to it: it emits a three-line classification and never edits
        # the workspace, so raising it would change what Stella *is* rather than
        # what it was allowed to spend.
        # No role carries `params.max_tokens`, and its ABSENCE is the setting.
        #
        # The engine seeds `max_output_tokens` from the model's catalog entry
        # and nothing but an explicit cap can lower it — "the DEFAULT is always
        # the model's maximum" (`stella-cli/src/agent/engine.rs`,
        # `tuned_engine_config`). So omitting the key asks for the model's own
        # ceiling on every role, on every model, without this file having to
        # know what that number is. Writing one here could only ever match the
        # catalog or sit under it, and the history below is four years of
        # rediscovering that the second one is a handicap:
        #
        #   16384 -> 32000 -> 64000, each raise chasing the same failure —
        #   a step ending with `output_tokens` exactly at the cap and
        #   `tool_calls=0`, the model cut off mid-reasoning with no visible
        #   answer. On the gate, four trials died at exactly 32000; Claude
        #   Code passed all four on the same model, same API, same effort,
        #   spending 45,001, 64,000, 64,000 and 25,965 output tokens. Two
        #   landed on precisely its ceiling and still emitted the tool call.
        #
        # Each fix moved one ceiling while the others held, because the cap,
        # `model_timeout` and the turn budget are one budget and the rule
        # governing all three is the same: never be the side that stops first.
        # #2411 finished the thought — a per-trial dollar budget is that same
        # ceiling wearing a different unit, and ArenaBench now refuses one.
        #
        # `model_timeout` does not become the new binding ceiling. It bounds
        # IDLE SILENCE between stream fragments, never elapsed time, so a
        # generation that keeps streaming is never cut by it however long it
        # runs (see `_ENGINE_DEFAULT_MODEL_TIMEOUT_SECS`).
        #
        # This hands compute to nobody: Claude Code does not cap itself, so
        # every number below the model's ceiling was a Stella-side handicap.
        "agents": {
            "default": {
                "effort": "xhigh",
                "reasoning": "on",
            },
            # Only the worker's tier moves with the arm, and `default` stays at
            # `xhigh` rather than tracking it so the arm moves one variable.
            #
            # It does NOT stand in for the roles with no entry below, and that
            # correction is the whole of #2549. `AgentEngineAgents::get` is a
            # straight per-role lookup with no fallback, and `over_worker`
            # (`crates/stella-cli/src/agent/engine.rs`) merges research and plan
            # over the **worker** field by field. So the roles this row was
            # believed to govern actually follow `worker` — and the arm's tier
            # did silently retune them, which is exactly the undeclared second
            # variable the old reading here claimed to prevent.
            "worker": {
                "effort": selected_effort,
                "reasoning": _WORKER_REASONING,
            },
            "verifier": {
                "effort": "xhigh",
                "reasoning": "on",
            },
            "triage": {"effort": "low", "reasoning": "off"},
        },
    }
    if verifier is not None:
        author = _validated_verifier(selected_model, verifier)
        # The flat root key, never `agents.verifier.model`. Both resolve — the
        # engine reads `agents.<role>.model` first and falls through to the
        # flat key (`AgentEngineConfig::model_for`), and
        # `the_flat_pipeline_verifier_model_alone_resolves_role_verifier_to_the_verifier`
        # pins that the flat key alone reaches `Role::Verifier` — but the flat key
        # is what `settings_check` and `stella config` report as the verifier's
        # origin, so the disclosed posture and the engine's own account of its
        # wiring name the same field.
        #
        # Naming the author is not the same as reaching it. A trial runs with
        # the catalog frozen (`STELLA_CATALOG_AUTO_REFRESH=0`), so the author
        # must be a slug Stella's OFFLINE seed catalog carries for that
        # provider; an unlisted one fails slug validation, the verifier pin is
        # dropped, and the verifier falls back to the worker. That used to make
        # the treatment arm run as the control arm under a treatment-arm digest
        # (#1147). It now refuses the run instead: a posture delivered through
        # the trusted launcher seam that names a verifier other than the worker
        # arms `PipelineConfig::require_independent_witness`, and a run that
        # cannot resolve an independent author fails before it spends anything.
        # A witness arm whose author is outside the seed therefore produces no
        # number at all, which is the intended outcome — the alternative is a
        # number this digest misdescribes.
        posture["pipeline_verifier_model"] = author
        posture["allowed_models"] = [selected_model, author]
    # Triage, research and plan pin identically — flat key plus a widened
    # vocabulary — so they share `_pin_role_model` rather than three copies of
    # it. The verifier above deliberately does not: it *replaces*
    # `allowed_models` rather than appending to it, because the treatment arm's
    # vocabulary is exactly the two models it names.
    for role, pin in (
        ("triage", triage_model),
        ("research", research_model),
        ("plan", plan_model),
    ):
        if pin is not None:
            _pin_role_model(posture, model=selected_model, pin=pin, role=role)
    # The read-only roles' tuning rows (#2549). Absent unless asked for: this is
    # the one place in this function where a row that merely *exists* would
    # re-hash arms registered years of runs ago, so "unset omits" is not a
    # stylistic echo of the keys above it but the reason the digest still means
    # what `bench/READINESS.md` §8.4.4 says it means.
    for role, effort, reasoning in (
        ("research", research_effort, research_reasoning),
        ("plan", plan_effort, plan_reasoning),
    ):
        row = _role_row(
            role=role,
            effort=effort,
            reasoning=reasoning,
            worker_effort=selected_effort,
        )
        if row is not None:
            posture["agents"][role] = row
    # `pipeline_max_revisions`, `pipeline_candidates` and
    # `pipeline_verifier_evidence_demand` were emitted here until #3871.
    #
    # Removing them cannot move a digest that omitted them, and no artifact
    # tracked in this repository carries any of the three (checked across
    # `bench/` and `arenabench/`, ignored and hidden files included). A match
    # registered outside the tree under `~/.arenabench/` is not covered by that
    # check — if one selected a knob, its recorded digest describes a posture
    # this code can no longer emit, which is the honest reading: the arm it
    # named is gone, not renamed.
    #
    # Omit-when-unset, and here it carries an extra weight: this key
    # is what makes a timeout change a *posture* change rather than a rebuild.
    # A run that selects it says so in its digest; a run that does not is
    # byte-identical to every run recorded before the key existed, which is the
    # only way the registered numbers keep describing the postures that
    # produced them.
    effective_timeout = (
        model_timeout_secs
        if model_timeout_secs is not None
        else default_model_timeout(selected_model)
    )
    if effective_timeout is not None:
        posture["model_timeout_secs"] = effective_timeout
    normalized = json.dumps(
        posture,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return posture, normalized, digest


#: The flat root key that pins each role's model, by role.
_FLAT_ROLE_MODEL_KEY = {
    "worker": "pipeline_worker_model",
    "verifier": "pipeline_verifier_model",
    "triage": "pipeline_triage_model",
    "research": "pipeline_research_model",
    "plan": "pipeline_plan_model",
}

#: The roles whose UNPINNED default is the worker's model, not ``default_model``.
#:
#: Every other role has a standing identity — unpinned, triage and the verifier
#: still have a model of their own, and ``default_model`` is it. These two are
#: documented as "run whatever the worker runs"
#: (``own_model_spec_for``, ``crates/stella-cli/src/engine_config.rs``), so
#: falling through to ``default_model`` for them would split them onto a
#: different model the moment anything re-points the worker without touching
#: settings. The two coincide in the frozen posture below, where the worker
#: carries no pin of its own — which is exactly why a resolver that got this
#: wrong would keep agreeing with reality until the first arm that pinned one.
_WORKER_INHERITING_ROLES = frozenset({"research", "plan"})


def _posture_model_value(value: Any, key: str) -> str | None:
    """Read one role-model value from a resolved posture, or refuse it.

    ``None`` (the key is absent) is a real answer — the role inherits — but a
    present value that is not a non-empty string is a malformed posture, and a
    declaration derived from a guess about it would be exactly the
    false-metadata failure #2134 is about. Fail closed with the reason.
    """
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip():
        raise ValueError(
            f"engine posture key `{key}` must be a non-empty string when "
            f"present; got {value!r}"
        )
    return value.strip()


def _posture_agent_model(posture: dict[str, Any], role: str) -> str | None:
    """The ``agents.<role>.model`` pin, or ``None`` when the role sets none."""
    agents = posture.get("agents")
    if agents is None:
        return None
    if not isinstance(agents, dict):
        raise ValueError(
            f"engine posture key `agents` must be a mapping when present; got "
            f"{agents!r}"
        )
    entry = agents.get(role)
    if entry is None:
        return None
    if not isinstance(entry, dict):
        raise ValueError(
            f"engine posture key `agents.{role}` must be a mapping when "
            f"present; got {entry!r}"
        )
    return _posture_model_value(entry.get("model"), f"agents.{role}.model")


def resolve_posture_role_model(
    posture: dict[str, Any], role: str, *, default_model: str
) -> tuple[str, str]:
    """Resolve one role's model from a posture, and name the key it came from.

    Mirrors ``AgentEngineConfig::model_for`` (``crates/stella-cli/src/settings.rs``)
    key for key: ``agents.<role>.model`` outranks the flat ``pipeline_<role>_model``,
    which outranks ``default_model`` — except for the two roles that inherit the
    worker instead on that last rung (``_WORKER_INHERITING_ROLES``, which is the
    engine's own split between ``model_spec_for`` and ``own_model_spec_for``, not
    an approximation of it). Mirroring it *exactly* is the whole point —
    a declaration that resolves roles by a different rule than the engine does is
    free to disagree with the run it describes, which is the shape of #2134 and of
    #1147 before it.

    Two of those rungs bit in the same match. ``cc00894779ff`` read only the
    host-side selector and so missed the flat key an ArenaBench roles config
    writes; a reader that took the flat key alone would still miss
    ``agents.<role>.model`` above it, and would still call a verifier that
    inherits ``default_model`` "the worker's model" when the *worker* is the
    pinned one. The returned origin key is what lets the off-reason name the
    setting an operator would actually edit, in the same vocabulary
    ``settings_check::flat_source_label`` reports.
    """
    flat_key = _FLAT_ROLE_MODEL_KEY.get(role)
    if flat_key is None:
        raise ValueError(f"unknown engine role `{role}`")
    agent_key = f"agents.{role}.model"
    for key, value in (
        (agent_key, _posture_agent_model(posture, role)),
        (flat_key, _posture_model_value(posture.get(flat_key), flat_key)),
    ):
        if value is not None:
            return value, key
    # Unpinned, and the last rung differs by role (see
    # `_WORKER_INHERITING_ROLES`). Recursing resolves the worker through the
    # same two rungs above, so "research inherits the worker" reports the key
    # that actually decided the worker's model rather than asserting
    # `default_model` and being right only by coincidence.
    if role in _WORKER_INHERITING_ROLES:
        return resolve_posture_role_model(
            posture, "worker", default_model=default_model
        )
    return default_model, "default_model"


def assurance_tiers_from_posture(
    posture: dict[str, Any],
) -> tuple[dict[str, Any], str, str]:
    """Declare which verification tiers a *resolved* engine posture exercises.

    The input is the exact ``agent_engine_config`` dict delivered to Stella —
    the output of ``_benchmark_engine_posture`` or of a harness's overridden
    builder (``StellaAgent._build_engine_posture`` is a documented seam) — so
    the declaration can never disagree with the configuration the engine
    actually ran. It used to be recomputed from the host-side witness-author
    selector alone, which is a second, narrower channel: an ArenaBench roles
    config reaches the posture without touching that selector, and match
    ``cc00894779ff`` recorded ``verifier_model: null`` / ``arm: witness-off``
    for a run whose posture named kimi-k3 as the verifier and whose trials
    demonstrably authored a witness (#2134).

    The witness arm's predicate is exactly one sentence, and it is the engine's
    own: the verifier role resolves to a model different from the worker's
    (``Pipeline::witness_author_independence``). Both sides are resolved here
    through :func:`resolve_posture_role_model`, so "the verifier inherits" is
    never *assumed* to mean "the verifier equals the worker" — a posture that
    pins only the worker leaves the verifier on ``default_model``, which is an
    independent author and used to be declared as the opposite.

    When the predicate fails, the recorded off-reason states the models both
    roles resolved to and the posture keys they came from — never a claim about
    roles this function did not examine.
    """
    if not isinstance(posture, dict):
        raise ValueError(
            "assurance tiers need the resolved engine posture as a dict; got "
            f"{type(posture).__name__}"
        )
    default_model = posture.get("default_model")
    if not isinstance(default_model, str) or "/" not in default_model.strip():
        raise ValueError(
            "engine posture `default_model` must be a non-empty provider/model "
            f"spec; got {default_model!r}"
        )
    baseline = default_model.strip()
    worker, worker_origin = resolve_posture_role_model(
        posture, "worker", default_model=baseline
    )
    verifier, verifier_origin = resolve_posture_role_model(
        posture, "verifier", default_model=baseline
    )
    author = verifier if verifier != worker else None
    off_reason = (
        None
        if author
        else (
            "no author independent of the worker: the engine posture resolves "
            f"the verifier role to `{verifier}` (from `{verifier_origin}`) and "
            f"the worker role to the same model (from `{worker_origin}`), so "
            "the witness tier cannot be authored on any task in this run"
        )
    )
    declaration: dict[str, Any] = {
        "version": _ASSURANCE_TIERS_VERSION,
        "arm": "witness-on" if author else "witness-off",
        "worker_model": worker,
        "verifier_model": author,
        "tiers": {
            # Flip oracle and recorded test results need no second model, so
            # this rung is on in both arms.
            "deterministic_verify": "on",
            "authored_witness": "on" if author else "off",
            # The verifier rung runs either way; in the control arm it resolves to
            # the worker's own model, which is a materially weaker claim and is
            # named as such rather than reported as a plain "on".
            "model_verdict": (
                "on-independent-of-worker" if author else "on-same-model-as-worker"
            ),
        },
        "authored_witness_off_reason": off_reason,
    }
    normalized = json.dumps(
        declaration,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return declaration, normalized, digest


def _benchmark_assurance_tiers(
    model: str,
    *,
    verifier: str | None = None,
) -> tuple[dict[str, Any], str, str]:
    """Return which verification tiers this posture exercises, and its hash.

    A sibling of the posture rather than a field inside it, and that is forced
    rather than stylistic: the trusted launcher seam
    (``config::trusted_engine_config_shape_is_strict``) fails **closed** on any
    root key outside ``ENGINE_ROOT_FIELDS``, so a ``tiers`` key inside the
    posture would refuse the run outright. Hashing it separately keeps the
    declaration frozen and disclosed on the same terms as the posture.

    This exists because "the witness did not run" used to be a log line inside
    the event stream, discoverable only by reading trajectories — which is what
    turns a stated caveat into a misread number. A scored run now either
    exercises a tier or declares it off in metadata a manifest can read (#1007).

    The ``(model, verifier)`` selector shape is kept for the callers that hold
    only the host-side inputs (``make_manifest.py``, the preregistration
    tooling); it validates them with the same refusals as the posture builder,
    then delegates to :func:`assurance_tiers_from_posture` so the two paths
    cannot disagree about what a declaration says. The adapter itself derives
    from the built posture instead — see ``StellaAgent.run`` (#2134).
    """
    selected_model = model.strip()
    if not selected_model or "/" not in selected_model:
        raise ValueError("benchmark model must be a non-empty provider/model spec")
    author = (
        _validated_verifier(selected_model, verifier)
        if verifier is not None
        else None
    )
    posture: dict[str, Any] = {"default_model": selected_model}
    if author is not None:
        posture["pipeline_verifier_model"] = author
    return assurance_tiers_from_posture(posture)


def fold_witness_observations(events: list[dict[str, Any]]) -> dict[str, Any]:
    """Summarize what a trial's proof stream observed about the witness tier.

    The declaration above says which rungs the posture *enables*; this says
    which ones the run actually *reached*. Both are needed and they are
    different claims — a posture can enable the authored witness on a task whose
    warrant decided no test was warranted at all.

    Reads `{"type":"proof","step":{"kind":…}}` events, Stella's own account of
    its verification ladder (`stella_protocol::ProofStep`), plus the
    `{"type":"verdict"}` event that closes the ladder. Before #1007 this
    lived only in the human-readable warning beside it, so "was the witness
    authored" was a question you answered by grepping trajectories.

    `witness_authored` is deliberately tri-state. `False` means the ladder said
    it could not author one; `None` means the stream never reached the question
    (an interrupted trial, or triage waiving assurance). Collapsing those into
    one boolean is how "not measured" starts reading as "measured and absent".

    The verdict fields are the other half of the A/B question (#1284). A trial
    carries two independent opinions about the same work — Stella's own
    (`verdict`) and the benchmark's external grader (`reward`, from the
    verifier, which the agent never sees) — and the interesting number is how
    often they disagree, in which direction. That comparison was only ever
    computable from the multi-gigabyte job tree, which is never committed, so
    it evaporated when the tree was deleted. Folding the verdict here puts it in
    trial metadata, and `score_dev_baseline.py` carries it into the committed
    `trials.jsonl` beside the reward it must be compared against.

    `self_verdict_deterministic` is what keeps that comparison honest: a verdict
    the flip oracle decided and a verdict a model opined are not the same claim,
    and the model-verifier-only rate is the one #1284 quotes.
    """
    proof_kinds: dict[str, int] = {}
    unavailable_reasons: list[str] = []
    warranted = 0
    assurance_planned: bool | None = None
    verdict_count = 0
    verdict_passed: bool | None = None
    verdict_deterministic: bool | None = None

    for event in events:
        if event.get("type") == "verdict":
            passed = event.get("passed")
            if not isinstance(passed, bool):
                continue  # a verdict that states no outcome is not one
            verdict_count += 1
            # Last verdict wins: a turn can revise a candidate and verifier it
            # again, and the claim the trial *ends* on is the one the external
            # reward is a comment on.
            verdict_passed = passed
            evidence = event.get("evidence")
            deterministic = (
                evidence.get("deterministic") if isinstance(evidence, dict) else None
            )
            verdict_deterministic = (
                deterministic if isinstance(deterministic, bool) else None
            )
            continue
        if event.get("type") != "proof":
            continue
        step = event.get("step")
        if not isinstance(step, dict):
            continue
        kind = step.get("kind")
        if isinstance(kind, str) and kind:
            proof_kinds[kind] = proof_kinds.get(kind, 0) + 1
        if kind == "witness_unavailable":
            reason_text = step.get("reason")
            if (
                isinstance(reason_text, str)
                and reason_text
                and reason_text not in unavailable_reasons
                and len(unavailable_reasons) < 8
            ):
                unavailable_reasons.append(reason_text)
        elif kind == "warrant":
            if step.get("required") is True:
                warranted += 1
        elif kind == "assurance":
            planned = step.get("witness")
            if isinstance(planned, bool):
                assurance_planned = planned

    authored_count = proof_kinds.get("witness_authored", 0)
    unavailable_count = proof_kinds.get("witness_unavailable", 0)
    if authored_count:
        authored: bool | None = True
        state = "authored"
    elif unavailable_count:
        authored = False
        state = "unavailable"
    else:
        authored = None
        state = "not_reported"

    return {
        "proof_step_counts": proof_kinds,
        "witness_authored": authored,
        "witness_authored_state": state,
        "witness_authored_count": authored_count,
        "witness_unavailable_count": unavailable_count,
        "witness_unavailable_reasons": unavailable_reasons,
        "witness_warranted_count": warranted,
        "assurance_witness_planned": assurance_planned,
        # Same tri-state discipline as `witness_authored`: a trial that was
        # killed before the ladder closed made no claim, and "made no claim"
        # must not be scored as "claimed failure" in an agreement rate.
        "self_verdict_passed": verdict_passed,
        "self_verdict_state": (
            "not_reported"
            if verdict_passed is None
            else ("passed" if verdict_passed else "failed")
        ),
        "self_verdict_deterministic": verdict_deterministic,
        "self_verdict_count": verdict_count,
        "verification_unavailable_count": proof_kinds.get(
            "verification_unavailable", 0
        ),
    }
