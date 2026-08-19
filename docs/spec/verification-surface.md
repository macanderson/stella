---
id: verification-surface
title: "The deck's verification surface in the plugin era"
status: implemented
---

# The deck's verification surface in the plugin era

*Decides [#3790](https://github.com/macanderson/stella/issues/3790). Context:
the PROOF rail was removed in #3791 ahead of `stella-pipeline`'s extraction
into an installable verification plugin (`doc:pipeline-as-plugins`, #3511).*

## Decision 1 — before a plugin is installed, the deck shows nothing

The deck has **no** verification surface when no verification plugin is
installed. Not a panel, not a placeholder, not an "unproven" standing.

The alternative that was on the table — rendering an explicit *unproven*
state — is a claim the host cannot make honestly. "Unproven" asserts that
verification was expected and did not happen. With no plugin installed,
nothing in the turn promised verification: the raw loop is the default, the
pipeline is opt-in, and a turn that never asked to be verified is not
"unproven", it is simply a turn. A standing that fires on every plugin-less
run teaches the reader to ignore it — the same failure hue-fatigue the
oracle's pre-flip red was designed to avoid (D6) — and it would be the
PROOF rail's permanently-empty box (#3791) wearing a worse costume: not
empty, but *lying*.

What the deck keeps showing is what the stream actually carries:

- `AgentEvent::Verdict` still folds the session model and renders the
  one-line textline verdict (`✓ verify (deterministic): …`). If a turn
  emits a verdict, the deck says so. If none arrives, nothing is drawn —
  the absence of a panel **is** the honest statement.
- `AgentEvent::Proof` steps remain in the traces tab and the offline
  transcript export, which read the recorded stream.

## Decision 2 — when a panel returns, it renders from protocol events only

A verification panel returns **with the plugin wire contract** (#3511), not
before. When it does, it is bound by four constraints, stated here so the
future PR is reviewable against them:

1. **Protocol events only.** The panel's data source is the `AgentEvent`
   stream — the same contract every other deck surface holds. No
   plugin-private channel, no polling the plugin process.
2. **Two visual languages.** Self-reported plugin evidence ("the plugin
   says its checks passed") and host-run verification ("stella itself ran
   the command and saw the flip") must be distinguishable at a glance —
   different glyph *and* different hue, per the deck's glyph-over-hue rule.
   A plugin's self-report is a claim; host-run evidence is a fact; the
   deck's one job is never to let the first impersonate the second.
3. **The finish-invariant holds.** A turn that ends without a verdict
   closes the panel in a terminal state — never left reading as in-flight
   on a dead turn (the #2007 class of bug).
4. **Snapshot coverage.** The panel ships with deck snapshots covering at
   minimum four states: no plugin (nothing rendered), plugin self-report
   pending, host-run pass, host-run fail. AGENTS.md § "Testing approach" is
   normative on where those frames live and how they are regenerated and
   read; this list only says which states the panel owes one.

## Decision 3 — `ORACLE_PRE_FLIP` is gone

The theme token's only consumer was the witness panel, removed in #3791.
The brand kit at `docs/brand/` never carried an oracle token, so nothing
mirrored it; the contrast-table argument for keeping it was circular (the
tables existed to check a token nothing rendered). Removed with its
`palette::ORACLE_RED` / `ORACLE_RED_INK` values, fallback entries, and
paper remap. If Decision 2's panel wants a pre-flip red, it reintroduces
one deliberately, with a consumer attached — a token earns its place by
being painted, not by waiting.

## Decision 4 — the `Proof`/`Verdict` consumer rows say `RecordedOnly`

Both rows were `Unclassified` debt under #2703. Their consumers are in fact
known: the traces tab (`stella-tui/src/deck/classify.rs`), the textline
verdict and session-model fold (`stella-tui/src/model.rs`,
`textline.rs`), the offline transcript export
(`stella-cli/src/export/transcript.rs`), and a debug-level diag record
(`stella-cli/src/diag_bridge.rs`). None is a *selecting* surface in
`Surface`'s sense — the TUI renders every variant by construction and is
deliberately not listable — so the honest posture is `RecordedOnly`, citing
this issue. The open question the rows point at is a producer question:
whether a verification plugin re-emits `Verdict` (assumed — it is the
natural event) and whether `ProofStep` survives the extraction at all.
Settled with the wire contract in #3511.

## What this does not decide

- The plugin wire contract itself (event shapes, self-report vs host-run
  encoding) — that is #3511's.
- Whether the observatory's journal whitelist grows a `verdict` entry —
  a #2707 question, independent of the deck.
