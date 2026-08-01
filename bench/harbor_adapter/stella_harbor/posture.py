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
from typing import Any

_ENGINE_POSTURE_VERSION = "stella-tb21-engine-posture-v1"
_ASSURANCE_TIERS_VERSION = "stella-tb21-assurance-tiers-v1"

# Host-side selector for the witness/judge author. Unset (the default) is the
# control arm: one inherited model, authored witness structurally off. Set to a
# second `provider/model` on the worker's provider and the same 89 tasks run the
# treatment arm. Never forwarded into the container — the decision reaches
# Stella only as `pipeline_judge_model` inside the hashed posture, so the arm a
# trial ran cannot disagree with the arm its digest records (#1007).
_WITNESS_AUTHOR_ENV = "STELLA_WITNESS_AUTHOR_MODEL"


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
    """
    selected_model = model.strip()
    if not selected_model or "/" not in selected_model:
        raise ValueError("benchmark model must be a non-empty provider/model spec")
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
        "agents": {
            "default": {"effort": "xhigh", "reasoning": "on"},
            "worker": {"effort": "xhigh", "reasoning": "on"},
            "judge": {"effort": "xhigh", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    }
    if witness_author is not None:
        author = _validated_witness_author(selected_model, witness_author)
        # The flat root key, never `agents.judge.model`. Both resolve, but the
        # flat key is what `settings_check` and `stella config` report as the
        # judge's origin, so the disclosed posture and the engine's own account
        # of its wiring name the same field.
        posture["pipeline_judge_model"] = author
        posture["allowed_models"] = [selected_model, author]
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
