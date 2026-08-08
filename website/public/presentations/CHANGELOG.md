# Investor deck changelog — 2026-08-08 rebuild

Every number that changed, from what to what, and why. Scope: `investor-deck.html`.
Companion: [`BENCHMARK_METHODOLOGY.md`](./BENCHMARK_METHODOLOGY.md).

## Why the benchmark numbers had to change

The previous deck carried two mutually contradictory sets of benchmark
figures (proof slide vs. ask slide), and a repo-wide search found that **none
of the eight numbers exists in any run artifact** — no trials, no score file,
no manifest. None is even an attainable integer fraction of the 89-task
suite. The deck also claimed a "result data package … on GitHub" that does
not exist; its only link was the repo root.

The repository's one real, preregistered, artifact-backed Terminal-Bench 2.1
run is `bench/evidence/tb21-hh10-20260731/` (2026-07-31). All benchmark
figures now come from it, plus the published tbench.ai leaderboard for
context. Details: `BENCHMARK_METHODOLOGY.md`.

## Numbers changed

| Where | Was | Now | Why |
|---|---|---|---|
| Proof slide, stella solve rate | 91.32% | **65.2% (58/89)** | 91.32% exists in no artifact and is not an attainable fraction of 89 tasks. 58/89 is the measured, preregistered result. |
| Proof slide, comparator solve rate | 80.3% "Claude Code · Fable 5" | **49.4% (44/89) Claude Code, same model (glm-5.2)** — plus the published **83.8% ± 1.2%** Claude Code + Fable 5 leaderboard row shown separately as context | 80.3% was below the public record (83.8%) and unsourced — it read as a handicapped baseline. The measured comparison is same-model; the Fable 5 figure now comes from tbench.ai directly, per founder instruction. |
| Proof slide, stella cost | $3.12/task | **$1.35 per solved task** | $3.12 exists in no artifact. $1.35 = $78.25 / 58 from the run's published results page (the score-table extraction gives $1.31; the deck quotes the higher). |
| Proof slide, comparator cost | $24.12/task | **removed** | Unsourced — and the run's own README rules arm-to-arm cost comparison invalid (different providers, different token accounting, comparator cost reported on only 71/89 trials). No Claude Code cost appears anywhere in the deck now. |
| Ask slide, resolution rate | 88.55% vs 81.03% | **removed** (tiles now: +15.7 pts, $1.35, 89/89, 0 bytes) | Second, contradictory, equally unsourced set. Deleted entirely; one number per metric deck-wide. |
| Ask slide, cost | $12.31 vs $59.32 | **removed** | Same. |
| "#1 on Terminal-Bench 2.1" (proof slide, ask slide, meta description) | asserted | **removed everywhere** | stella holds no public leaderboard row. Replaced with the claim that survives contact: +15.7 points over Claude Code, model held constant, preregistered and reproducible — with the audited public row named as a use of funds. |
| "~90% discount" / "about 90% less cost" (cover, payoff slide, meta) | asserted | **removed** | Was derived from the fabricated cost pairs ($3.12 vs $24.12 is 87%; $12.31 vs $59.32 is 79% — neither is 90%). No valid paired frontier-cost measurement exists. Replaced with measured $1.35/solved task plus the sourced ~1/27th open-vs-frontier API price ratio (DeepSeek-R1 vs o1, Jan 2025). |
| "Better results than frontier" (cover) | asserted | **removed** | Not measured. The measured claim is harness superiority at matched model; the frontier gap (65.2% vs 83.8%) is acknowledged on the proof slide and closing it is a funded milestone. |
| "stella, trained on your data, beats Claude Code" (proof slide) | asserted | **removed** | No Oxagen-trained model exists yet; both arms ran stock weights. The private-model flywheel is now presented as the funded roadmap (product slide: "steps 1–3 ship today; step 4 is what $2M builds"). |
| Veracode stat (pain slide) | quoted: "45% of AI-written code hides a known security hole" | **paraphrased:** AI-generated code introduced OWASP Top 10 vulnerabilities in 45% of test tasks (80 tasks, 100+ models) | The quoted sentence appears nowhere in the report. Verified against the 2025 GenAI Code Security Report (Jul 2025) and paraphrased without quotation marks. |
| Market slide, $12.5B "on model APIs alone" | asserted | **replaced with $4B AI coding category** | Could not verify $12.5B against the Menlo report's public materials. The $4B coding-category figure is confirmed in Menlo's Dec 2025 release and is the more relevant wedge number. $37B total and 3.2× (from $11.5B) verified and kept. |
| The ask | $2M SAFE at **$12.5M** post-money cap | $2M SAFE at **$18M** post-money cap — **11.1% dilution stated on-slide** | Founder-directed repricing. Dilution math: 2 / 18 = 11.1%. |
| GPAs (team slide) | GPA 3.92 / GPA 3.89 | **removed** | Padding on a $125M-exit founder's bio; lowers perceived seniority. |

## Structural changes

- 21 slides → **15 core + 3 diligence appendices** (A1 methodology, A2 witness
  failure modes, A3 licensing/IP). Pain 01/02/03 + "the deal on the table"
  compressed into one problem slide; invention + oracle flip merged; "the
  shift" merged into the payoff slide; vision merged into the close. Proof
  moved from slide 12 to slide 5.
- **New slide — "the moat, examined":** names the air-gap/flywheel objection
  and answers it (the substrate compounds; content-free telemetry enforced in
  CI; the pipeline is the switching cost).
- **Metering answered on the business-model slide:** subscription per team +
  usage metered on verified traces entering the training pipeline, counted
  on-prem; billed on the count, content never seen.
- **Competition table expanded** from 2 columns (Cursor/Claude Code) to four
  categories: IDE/CLI agents, frontier labs, open harnesses (SWE-agent,
  OpenHands, Aider), fine-tune platforms (Together, Fireworks, Predibase) —
  each with "why we win" and "what stops the copy".
- **Witness-protocol limitations named** (appendix A2): shallow flips,
  regression blindness, oracle coverage, reward hacking — with the current
  guards and the honest gaps.
- **Harness parity and contamination disclosure on the proof slide itself**,
  not only in a footnote.
- Proof slide links to `BENCHMARK_METHODOLOGY.md`; appendix A1 summarizes it
  for the room.

## Flags — claims that need founder verification before this deck ships

1. ~~Design-partner status~~ **Resolved by founder, 2026-08-08**: both
   partnerships are verbal; both are repeat buyers the founder has sold
   software to before. Cary has transacted north of $1M/year across three of
   the founder's companies, and its R&D department can waive competitive bid.
   The slide now states "verbal" plainly with that context; the confirm chip
   is removed.
2. **Degree titles** (team slide): "BS, Machine Learning & Algorithms —
   University of Tennessee, Knoxville" and "MS, Machine Learning — Georgia
   Institute of Technology" could not be verified against a registrar. Confirm
   the exact conferred wording; anything that looks retrofitted will be
   googled. (GPAs already removed.)
3. **Base-weight license matrix** (appendix A3): carries a
   `[verification pending]` chip. Confirm the glm-5.2 license terms (and any
   other base weights offered) permit commercial fine-tuning, redistribution
   of derivatives to customers, and government deployment.
4. **Fonteva figures** (team slide): $125M / $6M raised / ~60% retained are
   founder-attested; kept as-is, not independently verified here.
5. **Public leaderboard figures**: 83.8% ± 1.2% / 83.1% / 80.4% supplied from
   tbench.ai as of Aug 2026 — re-check the leaderboard the week the deck is
   shown; those rows move.
6. **Proposed (not founder-ratified) targets** on the ask slide, marked here
   rather than on-slide: 3–5 paid pilots, both design partnerships converted
   to paid, $1M ARR run-rate entering the seed, 18-month plan for four hires +
   training infrastructure. Ratify or edit before sending.
7. **Oracle-coverage percentage** (appendix A2): deliberately unnumbered — no
   measured figure exists. The slide says so and commits to per-pilot
   measurement. Do not add a number without a measurement behind it.

---

# Revision 2 — 2026-08-08, the v2.1 core message

Source: `Oxagen-Stella-Investor-Deck-v2_1.pptx` (founder-authored, 9 slides).
Its **narrative** was merged into this deck; its **benchmark arithmetic was
not**, for the reasons in the rebuild section above.

## Rejected from the PPTX

| PPTX claim | Disposition |
|---|---|
| "Latest 2-run average: **97.81%** of tasks solved. The current public leaderboard leader sits near 89%." (slide 6) | **Not imported.** 97.81% appears in no run artifact, and 89% contradicts the tbench.ai row already cited here (83.8% ± 1.2%). The proof slide keeps the measured 58/89 = 65.2%. |
| "We sell **90% cost reductions**" as a headline assertion (cover) | **Imported only as a labelled target.** The PPTX's own slide 7 calls it *"Target economics"*, which is a different claim than a measurement — so it ships on the "we don't sell tokens" slide with an on-slide `target` chip, beside the measured $1.35/solved task, and the footnote states plainly that no valid paired frontier-cost measurement exists yet. |
| "The frontier labs deserve it" (slide 2) | Reframed to blame the design decision, not the vendors: "earned by shipping agents with no definition of done." stella is BYOK on those same providers. |

## Imported from the PPTX

- **New slide — "the origin" (03):** the founder's thesis test, the discovery
  that the agent was claiming work it had not done, the oracle flip as the fix,
  and pre-labeled traces as the unplanned side effect. The deck previously
  asserted the witness protocol as a design; it now tells how it was found.
- **New slide — "the slope" (07): "stella builds stella."** The three
  self-improvement mechanisms, each verified present in the tree before being
  claimed: skill auto-promotion (`stella-core/src/skills.rs`,
  `stella-cli/src/memory/learning.rs`), the tool foundry with its capability
  witness and adoption ledger (`stella-cli/src/tool_foundry.rs`,
  `stella-tools/src/foundry_witness.rs`, store schema v20), and the A/B recall
  control (`stella-cli/src/agent/skill_usage.rs`). Framed as the PPTX frames
  it — "the point is the slope, not the number" — with no number attached.
- **New slide — "we don't sell tokens" (11):** the incentive inversion, which
  is the PPTX's core message and had no home in the previous deck. Replaces
  "the payoff" and absorbs its measured economics tiles.
- **New slide — "software first, then every vertical with a checkable
  outcome" (12):** the three-rung ladder (software → engine builders and
  optimizers → robotics), each rung named by the oracle it already has. The
  closing claim that the engine ports unchanged is backed by architecture
  invariant #1, which `scripts/check-invariants.sh` enforces in `make gate` —
  a market claim with a CI gate behind it. Previously one sentence on the
  close slide.
- **Problem slide (02):** broken cost attribution added as the setup, answered
  on slide 11 by per-execution cost/token/outcome recording. Plus the PPTX's
  sharpest line — nobody trusts the code AI ships, everybody LGTMs it anyway —
  and the "AI slop" framing.
- **Cover:** "stella is the engine. Oxagen is the control plane." and the
  positioning line "We don't sell tokens. We sell verified work — and the
  models that produce it."
- **Product slide (09):** the two-sided motion — open engine drives bottom-up
  adoption, commercial control plane is what the enterprise buys.
- **Ask slide (17):** leaderboard submission requires four verified runs, named
  as milestone one.

Deck length: 18 slides → **21** (18 core + 3 diligence appendices).

## Layout defect found and fixed (pre-existing)

`.slide` combined `display:flex; align-items:center` with `body{overflow:hidden}`,
so any slide taller than the viewport had its content centred **off both
edges** — heading and conclusion clipped while the middle still looked
designed. Measured at 1440×900, five slides were losing content this way,
including the proof slide, whose `<h2>` and overline were both off-screen.

Two changes, no content removed:

- `.frame` now uses `margin: auto` instead of the container's
  `align-items: center`. An auto cross-axis margin centres while free space
  exists and collapses to zero when it does not, so an over-tall slide starts
  at the top and scrolls rather than clipping. `.slide` gains `overflow-y:auto`
  (and `overflow:visible` under `@media print`).
- Vertical padding moved from `clamp(24px,6vw,96px)` to `clamp(18px,3vh,44px)`
  — it is charged against the slide's usable height, so scaling it to viewport
  *width* cost ~170px of vertical room on a 900px screen.

Every slide was then re-measured over CDP. Slides 2, 6, 15 and 20 still exceed
900px and scroll a little (17px, 195px, 51px, 57px); all four keep their
heading, lead and argument above the fold, and only footnote tails fall below
it. Slides 6, 15 and 20 are pre-existing density from revision 1 and their
content was deliberately left intact — slide 6's overflow is the benchmark
methodology disclosure.

## Flag added for founder verification

8. **The ~90% cost-reduction target** ("we don't sell tokens" slide): shipped
   with an on-slide `target` chip and a footnote saying no paired measurement
   exists. Either ratify it as the stated target or replace it with a measured
   figure once a like-for-like frontier-cost comparison is run. Do not let the
   chip be dropped while the number stays.
