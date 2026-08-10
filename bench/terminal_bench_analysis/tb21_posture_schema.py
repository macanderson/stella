"""The shape a study manifest's recorded engine posture must have.

Split out of `tb21_analysis` when the read-only roles arrived (#2549), for the
reason `stella_harbor.posture` was split out of its own package root: the
file-size gate treats a separable concern as its own module rather than as more
length on an already-oversized one. It is a genuine seam — six frozen sets and
no logic, describing one contract, with no dependency on the analysis around
them.

The contract has two halves, and the split between them is the whole subject:

* **Required.** The seven root keys and four `agents` roles every posture has
  carried since the protocol was frozen. A manifest missing one is malformed;
  a manifest carrying an unlisted key is describing a posture nobody reviewed.
* **Optional.** The keys a *selected arm* adds. Each is absent in the
  historical posture and present only when an operator asked for it — which is
  precisely why they cannot be required: the posture digest identifies every
  registered arm (`bench/READINESS.md` §8.4.4), so requiring a key that
  historical postures do not have would refuse every manifest recorded before
  the arm existed. And they cannot be rejected either, because a set arm is
  real configuration the manifest must be able to state.

That is why the validator this feeds takes an `optional_fields` argument rather
than exact equality alone. "Exact" was the right instrument while the posture
had no arms; it silently became the wrong one as the builder grew selectors,
and the required-root-key set had already fallen behind every one of them —
a treatment-arm manifest naming `pipeline_verifier_model` would have been
refused as "extra" by a schema that claims to describe it.

`test_posture.py::TestManifestSchemaCoversEverySelectableKey` derives both
halves from `stella_harbor.posture` rather than trusting the lists below, so
the next selector fails there instead of on a paid run's manifest.
"""

from __future__ import annotations

#: The `engine_posture` record wrapping one posture: what it is, and its hash.
STUDY_MANIFEST_ENGINE_POSTURE_RECORD_FIELDS = frozenset(
    {"version", "posture", "sha256"}
)

#: The root keys every posture carries, arm or no arm.
STUDY_MANIFEST_ENGINE_POSTURE_FIELDS = frozenset(
    {
        "default_model",
        "allowed_models",
        "auto_mode",
        "effort_auto",
        "reasoning_auto",
        "headless_scope_bypass",
        "agents",
    }
)

#: Root keys a posture MAY carry — one per selectable arm.
#:
#: Absent is the historical posture; present is a declared configuration that
#: moved the digest. Every entry is a key `_benchmark_engine_posture` emits only
#: when its selector was set, and the parity test named in this module's
#: docstring is what keeps that correspondence true.
STUDY_MANIFEST_ENGINE_POSTURE_OPTIONAL_FIELDS = frozenset(
    {
        "pipeline_verifier_model",
        "pipeline_triage_model",
        "pipeline_research_model",
        "pipeline_plan_model",
        "pipeline_max_revisions",
        "pipeline_candidates",
        "pipeline_verifier_evidence_demand",
        "model_timeout_secs",
    }
)

#: The `agents` rows every posture carries.
STUDY_MANIFEST_ENGINE_POSTURE_AGENT_ROLES = frozenset(
    {"default", "worker", "verifier", "triage"}
)

#: The `agents` rows a posture MAY carry (#2549).
#:
#: `research` and `plan` inherit the worker field by field unless an arm turns
#: them down (`over_worker`, `crates/stella-cli/src/agent/engine.rs`), so a
#: posture that does not turn them down has nothing to say about them and says
#: nothing — which is what keeps its digest identical to the arms registered
#: before these roles were configurable at all.
STUDY_MANIFEST_ENGINE_POSTURE_OPTIONAL_AGENT_ROLES = frozenset({"research", "plan"})

#: Per-role field shapes. Every role is the same shape — `effort` and
#: `reasoning`, no `params` — because no role carries an output cap (#2411).
#: The split existed to let the outcome-bearing roles hold `params.max_tokens`
#: while triage kept the engine default; with the cap gone there is nothing for
#: `params` to contain, and a posture that grows one has to be reviewed rather
#: than accepted by a set that still permits the key.
#:
#: The optional rows are listed at the same exact shape as the required ones,
#: which is only possible because the builder emits them whole or not at all
#: (`_role_row` in the adapter's `posture.py`). A partial row would make "exact"
#: unenforceable for exactly the two roles that most need reviewing.
STUDY_MANIFEST_ENGINE_POSTURE_AGENT_FIELDS_BY_ROLE: dict[str, frozenset[str]] = {
    "default": frozenset({"effort", "reasoning"}),
    "worker": frozenset({"effort", "reasoning"}),
    "verifier": frozenset({"effort", "reasoning"}),
    "triage": frozenset({"effort", "reasoning"}),
    "research": frozenset({"effort", "reasoning"}),
    "plan": frozenset({"effort", "reasoning"}),
}
