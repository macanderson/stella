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

#: Signature of an engine-posture builder: ``(model, *, witness_author)`` ->
#: ``(posture, canonical_json, sha256)``. `_benchmark_engine_posture` is the
#: frozen claim implementation; `StellaAgent._build_engine_posture` is the seam
#: a non-claim harness may override with another frozen configuration.
PostureBuilder = Callable[..., "tuple[dict[str, Any], str, str]"]

_ENGINE_POSTURE_VERSION = "stella-tb21-engine-posture-v1"
_ASSURANCE_TIERS_VERSION = "stella-tb21-assurance-tiers-v1"

# Host-side selector for the witness/judge author. Unset (the default) is the
# control arm: one inherited model, authored witness structurally off. Set to a
# second `provider/model` on the worker's provider and the same 89 tasks run the
# treatment arm. Never forwarded into the container — the decision reaches
# Stella only as `pipeline_judge_model` inside the hashed posture, so the arm a
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

# The output cap, per model, and the silence ceiling that has to absorb it.
#
# These were one shared number (64000) until the Fable ceiling set was
# approved (#1211 §6.2). One number was only ever right by coincidence: it is
# the *model's* ceiling, and models differ. Fable 5 answers up to 128,000
# output tokens; capping its trials at 64,000 stopped it at half the height
# the comparator is allowed to fill, and the score then reported that as a
# capability difference rather than as our own ceiling.
#
# Keyed by the bare model slug, so a model reached directly
# (`anthropic/claude-fable-5`) and the same model reached through a gateway
# (`openrouter/anthropic/claude-fable-5`) get the same ceilings. Booking route
# is not a model property.
#
# `TestOutputCeilingParity` pins the caps here against
# `stella-model/src/catalog.rs`, which is the authority. Change the catalog and
# this must follow, which is the point: the two numbers used to be able to
# drift apart silently, both still looking deliberate.
_DEFAULT_OUTPUT_CAP = 64_000
_OUTPUT_CAP_BY_SLUG = {"claude-fable-5": 128_000}

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


def _model_slug(model: str) -> str:
    """The bare model name, with any provider or gateway prefix stripped."""
    return model.rsplit("/", 1)[-1].strip().lower()


def default_output_cap(model: str) -> int:
    """The output-token cap this model's benchmark posture uses."""
    return _OUTPUT_CAP_BY_SLUG.get(_model_slug(model), _DEFAULT_OUTPUT_CAP)


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

    The provider rule is the same one `_validated_witness_author` enforces and
    it is enforced separately rather than shared, because the two roles fail
    differently: an unreachable judge silently degrades the *claim* (the run
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


def _validated_witness_author(model: str, witness_author: str) -> str:
    """Return the pinned witness/judge author, or refuse it with the reason.

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
    author = witness_author.strip()
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
    witness_author: str | None = None,
    worker_effort: str = "xhigh",
    triage_model: str | None = None,
    max_revisions: int | None = None,
    candidates: int | None = None,
    model_timeout_secs: int | None = None,
) -> tuple[dict[str, Any], str, str]:
    """Return a canonical Terminal-Bench engine posture and its hash.

    Request posture is explicit per role so ordinary auto-mode defaults cannot
    drift across Stella versions. The normalized JSON is the exact value
    delivered through the trusted launcher override consumed by the CLI.

    Two arms, and which one a run used is a property of the hash rather than of
    the logs (#1007):

    * ``witness_author=None`` — the **control arm**. Model routing is expressed
      only by ``default_model``; every role inherits it and no role has a
      provider/model override. Stella will not let the worker write the test
      that verifies it, so with one model for every role the independent author
      never exists: the run reports ``WitnessUnavailable`` and proceeds unproven
      (#973). Every Terminal-Bench number published before #1007 is this arm.
    * ``witness_author="provider/slug"`` — the **treatment arm**. A second model
      is pinned for the judge role via ``pipeline_judge_model``, which is what
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
    be the exception (#1211 §6.2). The output cap is set per role below and the
    turn budget is a per-trial flag, but the per-generation silence ceiling was
    an engine constant — so a Fable-class arm, which needs it scaled with the
    output cap or it merely stops on the timeout instead, could not be
    expressed as a posture at all. It is now a key like any other: selecting it
    changes the digest, leaving it unset reproduces every historical one.
    """
    selected_model = model.strip()
    if not selected_model or "/" not in selected_model:
        raise ValueError("benchmark model must be a non-empty provider/model spec")
    selected_effort = _validated_worker_effort(worker_effort)
    # The model's own ceilings, unless the operator selected otherwise below.
    output_cap = default_output_cap(selected_model)
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
        # `params.max_tokens` raises the engine's 16384 output cap, and at
        # xhigh it is load-bearing rather than a tuning knob. Measured, not
        # assumed: in the first xhigh smoke, 2 of 3 Stella trials died with
        #
        #   output_tokens=16384, tool_calls=0
        #   "reached its output-token limit before producing any visible
        #    response — its budget was likely spent on reasoning"
        #
        # The engine default carries a comment saying 16k was itself a raise
        # from 8k for exactly this failure on glm-5.2, and names per-model caps
        # as the real fix. Effort and the output cap are coupled: raising the
        # tier without raising the cap buys reasoning that cannot fit an answer
        # beside it, and the step ends with no tool call at all.
        #
        # 64000, which is the model's own ceiling and therefore the
        # comparator's. The previous value was 32000, held there because
        # `model_timeout` was 600s and a 64000-token step would not fit inside
        # it — one self-imposed ceiling justifying another. Both moved
        # together; `model_timeout` is now 816s, and neither binds first.
        #
        # The 32000 value was falsified directly. On the gate, four trials
        # ended on a step that emitted exactly 32000 output tokens with zero
        # tool calls. Claude Code passed all four of those tasks (reward 1.0)
        # on the same model, same API and same effort, and its winning steps
        # on them spent 45,001, 64,000, 64,000 and 25,965 output tokens. Two
        # landed on precisely 64,000 — its ceiling — and still had room to
        # emit the tool call. Sonnet fills whatever budget it is given in
        # either agent; the only variable is who stops it first.
        #
        # That is also why 16384 -> 32000 did not fix truncation and 64000
        # "merely traded truncation for a model_timeout": each attempt moved
        # one ceiling while the others held. The output cap, `model_timeout`
        # and the turn budget are one budget, and the rule that sets all three
        # is the same — never be the side that stops first.
        #
        # This is not compute handed to one side. Claude Code does not cap
        # itself, so every number below the model's ceiling was a Stella-side
        # handicap and removing it restores parity rather than breaking it.
        # `triage` keeps the engine default: it runs at low effort and emits a
        # three-line classification, so the cap was never near binding for it.
        "agents": {
            "default": {
                "effort": "xhigh",
                "reasoning": "on",
                "params": {"max_tokens": output_cap},
            },
            # Only the worker's tier moves with the arm. `default` stays at
            # `xhigh` deliberately: it governs roles with no explicit entry
            # below, and letting it track the worker would silently retune
            # those roles too — a second, undeclared variable inside a digest
            # that claims to describe one.
            "worker": {
                "effort": selected_effort,
                "reasoning": "on",
                "params": {"max_tokens": output_cap},
            },
            "judge": {
                "effort": "xhigh",
                "reasoning": "on",
                "params": {"max_tokens": output_cap},
            },
            "triage": {"effort": "low", "reasoning": "off"},
        },
    }
    if witness_author is not None:
        author = _validated_witness_author(selected_model, witness_author)
        # The flat root key, never `agents.judge.model`. Both resolve — the
        # engine reads `agents.<role>.model` first and falls through to the
        # flat key (`AgentEngineConfig::model_for`), and
        # `the_flat_pipeline_judge_model_alone_resolves_role_judge_to_the_witness_author`
        # pins that the flat key alone reaches `Role::Judge` — but the flat key
        # is what `settings_check` and `stella config` report as the judge's
        # origin, so the disclosed posture and the engine's own account of its
        # wiring name the same field.
        #
        # Naming the author is not the same as reaching it. A trial runs with
        # the catalog frozen (`STELLA_CATALOG_AUTO_REFRESH=0`), so the author
        # must be a slug Stella's OFFLINE seed catalog carries for that
        # provider; an unlisted one fails slug validation, the judge pin is
        # dropped, and the judge falls back to the worker. That used to make
        # the treatment arm run as the control arm under a treatment-arm digest
        # (#1147). It now refuses the run instead: a posture delivered through
        # the trusted launcher seam that names a judge other than the worker
        # arms `PipelineConfig::require_independent_witness`, and a run that
        # cannot resolve an independent author fails before it spends anything.
        # A witness arm whose author is outside the seed therefore produces no
        # number at all, which is the intended outcome — the alternative is a
        # number this digest misdescribes.
        posture["pipeline_judge_model"] = author
        posture["allowed_models"] = [selected_model, author]
    if triage_model is not None:
        # Same flat-key reasoning as the judge pin above: `settings_check` and
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


def _benchmark_assurance_tiers(
    model: str,
    *,
    witness_author: str | None = None,
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
    """
    selected_model = model.strip()
    if not selected_model or "/" not in selected_model:
        raise ValueError("benchmark model must be a non-empty provider/model spec")
    author = (
        _validated_witness_author(selected_model, witness_author)
        if witness_author is not None
        else None
    )
    declaration: dict[str, Any] = {
        "version": _ASSURANCE_TIERS_VERSION,
        "arm": "witness-on" if author else "witness-off",
        "worker_model": selected_model,
        "witness_author_model": author,
        "tiers": {
            # Flip oracle and recorded test results need no second model, so
            # this rung is on in both arms.
            "deterministic_verify": "on",
            "authored_witness": "on" if author else "off",
            # The judge rung runs either way; in the control arm it resolves to
            # the worker's own model, which is a materially weaker claim and is
            # named as such rather than reported as a plain "on".
            "model_judge": (
                "on-independent-of-worker" if author else "on-same-model-as-worker"
            ),
        },
        "authored_witness_off_reason": (
            None
            if author
            else (
                "no author independent of the worker: every role inherits "
                "default_model, so the witness tier cannot be authored on any "
                "task in this run"
            )
        ),
    }
    normalized = json.dumps(
        declaration,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return declaration, normalized, digest


def fold_witness_observations(events: list[dict[str, Any]]) -> dict[str, Any]:
    """Summarize what a trial's proof stream observed about the witness tier.

    The declaration above says which rungs the posture *enables*; this says
    which ones the run actually *reached*. Both are needed and they are
    different claims — a posture can enable the authored witness on a task whose
    warrant decided no test was warranted at all.

    Reads `{"type":"proof","step":{"kind":…}}` events, Stella's own account of
    its verification ladder (`stella_protocol::ProofStep`). Before #1007 this
    lived only in the human-readable warning beside it, so "was the witness
    authored" was a question you answered by grepping trajectories.

    `witness_authored` is deliberately tri-state. `False` means the ladder said
    it could not author one; `None` means the stream never reached the question
    (an interrupted trial, or triage waiving assurance). Collapsing those into
    one boolean is how "not measured" starts reading as "measured and absent".
    """
    proof_kinds: dict[str, int] = {}
    unavailable_reasons: list[str] = []
    warranted = 0
    assurance_planned: bool | None = None

    for event in events:
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
    }
