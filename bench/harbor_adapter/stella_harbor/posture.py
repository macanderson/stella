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

# Host-side selectors for the two pipeline knobs that decide how many times a
# task may be attempted: revision turns after a *failed* verification, and
# best-of-N candidate executions. Both were fully implemented in the pipeline
# and reachable from nothing — outside tests, `max_revisions` was written once
# (to 2) and `candidates` once (to `None`), so best-of-N had never run in
# production at all. `agent_engine_config` now carries both, which is what lets
# a benchmark arm select them (#1211 §6.7, §6.8).
#
# Unset is the historical posture in both cases, and that is the whole point:
# a tree that merely carries this code reproduces every digest recorded before
# the selectors existed. Choosing a value adds a root key, changes the digest,
# and therefore declares itself in the manifest.
_MAX_REVISIONS_ENV = "STELLA_MAX_REVISIONS"
_CANDIDATES_ENV = "STELLA_CANDIDATES"

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

# Host-side selector for the corroboration ask (#1295): when a model verifier
# passes and nothing deterministic stands behind it, does the pipeline spend
# one revision demanding the evidence, or record the pass as unverified on the
# spot?
#
# A selector rather than a rebuild because the question is empirical and the
# answer is a measurement, not a preference. It was measured once and reverted:
# the ask fired on nearly every Terminal-Bench turn, because those turns had no
# tracked command and therefore no way to satisfy it, so it bought a turn
# everywhere and evidence nowhere. The pipeline now refuses to raise the ask
# without a command that could answer it, which is the change this selector
# exists to put on the record — two arms, one binary, one posture key apart.
#
# Unset omits the key, so every digest recorded before this selector existed
# still describes the posture that produced it.
_VERIFIER_EVIDENCE_DEMAND_ENV = "STELLA_VERIFIER_EVIDENCE_DEMAND"

# Refusal ceilings, not clamps. Unlike the effort tier there is no enum to
# validate an integer against, so a bound is the only check available beyond
# "parses as a number" — and the failure it catches is a fat-fingered extra
# digit, which on these two knobs is a runaway bill rather than a wrong answer.
# Both sit comfortably above the values #1211 actually proposes (3-4 revisions,
# 2 candidates), so hitting one means a typo rather than an ambitious arm.
_MAX_REVISIONS_CEILING = 10
_CANDIDATES_CEILING = 5

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


def resolve_max_revisions(value: str | None) -> int | None:
    """Resolve the per-candidate revision budget, or ``None`` to inherit.

    ``0`` is admissible and meaningful — one shot, no retry — so the floor is
    zero rather than one. Revisions are only ever spent on a verification that
    already *failed*, so raising this costs nothing on tasks that pass first
    time; it is the tail it changes.
    """
    if value is None:
        return None
    return _validated_attempt_count(
        value, label="max revisions", floor=0, ceiling=_MAX_REVISIONS_CEILING
    )


def resolve_candidates(value: str | None) -> int | None:
    """Resolve the best-of-N candidate count, or ``None`` for single-shot.

    Floored at 1, not 0: the pipeline floors a zero to one anyway
    (``PipelineConfig::candidate_count``), so accepting one here would let two
    different selector values produce the same run under two different digests
    — the exact ambiguity these selectors exist to remove.

    Note the cost shape differs from revisions: candidates are paid
    *unconditionally*, so ``2`` doubles execution cost across every task,
    including the ones a single shot would have solved.
    """
    if value is None:
        return None
    return _validated_attempt_count(
        value, label="candidates", floor=1, ceiling=_CANDIDATES_CEILING
    )


def resolve_verifier_evidence_demand(value: str | None) -> bool | None:
    """Resolve the corroboration-ask arm, or ``None`` to inherit the default.

    Fails closed on anything that is not one of the four accepted spellings,
    for the reason every selector here does: a run scored under a configuration
    nobody chose is worse than a run that refused to start. In particular a
    bare ``"false"`` is *not* silently read as off — the accepted vocabulary is
    the one the setting itself uses (``on``/``off``), plus ``1``/``0`` for the
    shell that has an integer to hand.
    """
    if value is None:
        return None
    text = value.strip().lower()
    if text in ("on", "1"):
        return True
    if text in ("off", "0"):
        return False
    raise ValueError(
        "benchmark verifier evidence demand must be one of on/off/1/0; "
        f"got `{value}`"
    )


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
    Whitespace-only is treated as unset here because, unlike the effort tier,
    "no pin" is a meaningful and previously-default configuration.
    """
    if value is None:
        return None
    return value.strip() or None


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
    max_revisions: int | None = None,
    candidates: int | None = None,
    verifier_evidence_demand: bool | None = None,
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

    ``max_revisions`` and ``candidates`` follow the same rule and exist for the
    same reason (#1211 §6.7, §6.8). Both are ``None`` by default, which omits
    the key rather than writing the engine's default into it — so this function
    still returns byte-identical JSON, and therefore an identical digest, for
    every posture recorded before the knobs were reachable at all.

    ``model_timeout_secs`` is the same shape again, for the ceiling that used to
    be the exception (#1211 §6.2). It is the last per-generation ceiling this
    posture still sets: the output cap and the turn budget are both gone
    (#2411), leaving the model's catalog maximum and the task's own wall clock.
    It is a key like any other — selecting it changes the digest, leaving it
    unset reproduces every historical one.
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
            # Only the worker's tier moves with the arm. `default` stays at
            # `xhigh` deliberately: it governs roles with no explicit entry
            # below, and letting it track the worker would silently retune
            # those roles too — a second, undeclared variable inside a digest
            # that claims to describe one.
            "worker": {
                "effort": selected_effort,
                "reasoning": "on",
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
    if triage_model is not None:
        # Same flat-key reasoning as the verifier pin above: `settings_check` and
        # `stella config` report the flat key as the role's origin, so the
        # disclosed posture and the engine's own account of its wiring name the
        # same field. `allowed_models` has to widen with it — the vocabulary is
        # a whitelist, and a triage pin outside it is refused at resolve time,
        # which would drop triage back onto the worker and bill the expensive
        # model for the cheap role while the digest claimed otherwise.
        triage = _validated_role_model(selected_model, triage_model, "triage")
        posture["pipeline_triage_model"] = triage
        allowed = list(posture["allowed_models"])
        if triage not in allowed:
            allowed.append(triage)
        posture["allowed_models"] = allowed
    # The attempt-count knobs are omitted entirely when unselected rather than
    # written at their default value, and that is load-bearing: the digest is
    # taken over this dict, so emitting `"pipeline_max_revisions": 2` would
    # change every hash in the tree to describe a posture identical to the one
    # they already described. Absent means "the engine's default", which is
    # exactly what the historical runs had.
    if max_revisions is not None:
        posture["pipeline_max_revisions"] = max_revisions
    if candidates is not None:
        posture["pipeline_candidates"] = candidates
    # Same omit-when-unset rule, spelled in the setting's own `on`/`off`
    # vocabulary rather than as a JSON bool: the CLI parses this key as a
    # `Toggle`, and a `true` here would be a refused run rather than a
    # selected arm.
    if verifier_evidence_demand is not None:
        posture["pipeline_verifier_evidence_demand"] = (
            "on" if verifier_evidence_demand else "off"
        )
    # Same omit-when-unset rule, and here it carries an extra weight: this key
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
}


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
    which outranks ``default_model``. Mirroring it *exactly* is the whole point —
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
