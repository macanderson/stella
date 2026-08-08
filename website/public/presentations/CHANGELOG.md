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
