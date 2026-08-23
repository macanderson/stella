import Link from "next/link";
import { InstallBlock } from "@/components/install-block";
import { REPO_URL } from "@/lib/site";
import {
  BrandFace,
  EventFeed,
  FactCard,
  Lead,
  Station,
  StationHead,
} from "./station";

/**
 * The aft of the engine: vera's verification chamber, the Oxagen governance
 * plane, the evolution deck, and the exit. The fore stations (intake, the
 * turn loop, the tool bay) are in stations-fore.tsx.
 *
 * **The honesty rule this file is written under.** A marketing page is still
 * a claim surface, and this repository's standard is that a claim ships with
 * the evidence that establishes it. So every mechanism named here restates a
 * documented one, and anything on the evolution deck that is a *design*
 * rather than a shipped capability carries a status chip that says so —
 * `shipped`, `partial`, or `in design`, sourced from
 * /docs/self-improvement § "What is actually built today". Selling a
 * roadmap as a feature is the one move that would make the rest of this
 * page untrustworthy too.
 */

/* ── 03 · vera ───────────────────────────────────────────────────────────── */

const VERA_FEED = [
  '{"type":"witness_authored","by":"witness-author","tamper_watch":true}',
  '{"type":"probe","command":"cargo test -p scheduler","phase":"pre","result":"FAIL"}',
  '{"type":"probe","command":"cargo test -p scheduler","phase":"post","result":"PASS"}',
  '{"type":"flip_oracle","state":"flipped","locked_command":"cargo test -p scheduler"}',
  '{"type":"diff_budget","changed_lines":38,"limit":400,"within":true}',
  '{"type":"verdict","rung":"SubmitFast","model_calls_spent":0}',
] as const;

/** The ladder, in the order it is walked. Every rung is terminal. */
const LADDER = [
  {
    rung: "1",
    name: "revise",
    tone: "fail",
    body: "Touched tests are red. That is already a deterministic failure — the evidence goes straight back into a revision turn. Nothing is spent confirming a failure nobody doubts.",
  },
  {
    rung: "2",
    name: "nothing attempted",
    tone: "fail",
    body: "The turn dispatched no call capable of changing the workspace, and nothing that could look saw a change. The one place an absence is read as evidence.",
  },
  {
    rung: "3",
    name: "unverifiable",
    tone: "abstain",
    body: "Every channel was blind — no flip, no test result, an unreadable tree. vera abstains. Nothing is claimed about the work, in particular not that it failed.",
  },
  {
    rung: "4",
    name: "submit fast",
    tone: "pass",
    body: "Flip achieved, touched tests green, diff within budget, no fresh diagnostics. The full deterministic pass — the only rung that means proven.",
  },
  {
    rung: "5",
    name: "unverified",
    tone: "abstain",
    body: "The probes looked, and what came back did not amount to a proof. vera's honest I do not know — stated, not dressed up as a pass.",
  },
] as const;

export function VeraStation() {
  return (
    <Station id="vera" tone="chamber">
      <StationHead
        id="vera"
        code="03"
        name="the verification chamber"
        badge="deterministic"
        title={
          <>
            meet <span className="eng-title-accent">vera</span> — she does not
            have an opinion about your code.
          </>
        }
      />
      <Lead>
        Every other agent ends a task when the model says it is finished. That
        is the whole problem: the thing that did the work is also the thing
        grading it. <BrandFace>vera</BrandFace> is the deterministic verifier
        at the back of the engine, and she never asks a model anything. She
        runs commands and reads what happened.
      </Lead>

      {/* The flip oracle: the central mechanism, drawn as two runs of one
          command with the change dropped in between. */}
      <div className="eng-flip">
        <p className="eng-flip-label">the flip oracle</p>
        <div className="eng-flip-track">
          <div className="eng-flip-run" data-state="fail">
            <span className="eng-flip-state">FAIL</span>
            <span className="eng-flip-when">before the change</span>
          </div>
          <div className="eng-flip-change" aria-hidden="true">
            <span className="eng-flip-arrow" />
            <span className="eng-flip-change-label">the diff</span>
          </div>
          <div className="eng-flip-run" data-state="pass">
            <span className="eng-flip-state">PASS</span>
            <span className="eng-flip-when">after the change</span>
          </div>
        </div>
        <p className="eng-flip-note">
          One command, locked by name, run on both sides. A suite that was
          already green proves nothing — only the fail→pass flip proves the
          change did the work. The oracle is a state machine: it locks onto
          the first command it sees <em>fail</em>, and only a later pass of{" "}
          <em>that same command</em>{" "}
          moves it to flipped. The
          &quot;it passed, ship it&quot; false positive is structurally
          excluded.
        </p>
      </div>

      <div className="eng-vera-facts">
        <FactCard title="the witness is written by someone else">
          With no test command configured, an independent model — never the
          worker — authors a minimal <em>failing</em> test, working from a
          pristine snapshot of the pre-change tree, blind to the diff, so it
          cannot write a test that merely restates the patch. It gets exactly
          three tools. Every other tool name returns an error.
        </FactCard>
        <FactCard title="tampering forfeits the credit">
          The witness file&apos;s full filesystem identity is pinned where it
          was written and re-pinned inside the candidate. A worker that edits
          the witness does not get credit for passing it.
        </FactCard>
        <FactCard title="the witness does not stay">
          It encodes a moment — &quot;this code doesn&apos;t do X yet&quot; —
          not an invariant, so it lives and dies with the candidate workspace.
          You never inherit an already-satisfied test nobody reviewed.
        </FactCard>
        <FactCard title="verification buys no model call">
          The ladder is arithmetic over measurements. Every rung is terminal,
          no reviewer is consulted, and the verdict is emitted from the answer
          directly.
        </FactCard>
      </div>

      {/* The ladder. */}
      <div className="eng-ladder">
        <p className="eng-ladder-label">
          five outcomes. every one of them terminal —
        </p>
        <ol className="eng-ladder-rungs">
          {LADDER.map((rung) => (
            <li key={rung.name} className="eng-ladder-rung" data-tone={rung.tone}>
              <p className="eng-ladder-rung-head">
                <span className="eng-ladder-num">{rung.rung}</span>
                <span className="eng-ladder-name">{rung.name}</span>
              </p>
              <p className="eng-ladder-body">{rung.body}</p>
            </li>
          ))}
        </ol>
      </div>

      {/* The receipt: why the model verdict was removed. A number that costs
          us something is the one worth printing. */}
      <blockquote className="eng-receipt">
        <p>
          The last rung used to ask a second model for <em>PASS</em> or{" "}
          <em>FAIL</em>. Over an 89-task Terminal-Bench run, that verdict
          agreed with the benchmark&apos;s grader <strong>46%</strong> of the
          time — a coin flip — and 17 of its false passes cost 5 tasks
          outright. So it was removed structurally, not defaulted off.
        </p>
        <footer>
          — why <BrandFace>vera</BrandFace> has no opinion,{" "}
          <Link href="/docs/plugins">the deterministic ladder</Link>
        </footer>
      </blockquote>

      <EventFeed lines={VERA_FEED} />
    </Station>
  );
}

/* ── 04 · governance ─────────────────────────────────────────────────────── */

export function GovernanceStation() {
  return (
    <Station id="governance">
      <StationHead
        id="governance"
        code="04"
        name="the governance plane"
        title={
          <>
            <span className="eng-title-accent">oxagen</span> is the plane above
            the engine — the rules that travel with the repository.
          </>
        }
      />
      <Lead>
        An agent that behaves differently on every laptop is not governed, it
        is merely configured. The governance plane makes steering policy a{" "}
        <em>reviewable artifact</em>: records that live in the repository,
        travel with a clone, and are validated in CI on every pull request —
        so a teammate&apos;s session obeys the same rules yours does, because
        it read the same file.
      </Lead>

      <div className="eng-gov">
        <div className="eng-gov-layer" data-layer="records">
          <p className="eng-gov-path">.stella/rules/*.toml</p>
          <h3 className="eng-gov-name">context records</h3>
          <p className="eng-gov-body">
            One record per file, tracked in git — the only part of{" "}
            <BrandFace>.stella/</BrandFace>{" "}
            that is. A record only steers a
            teammate&apos;s session if it travels with the repository, so it
            is committed like code and reviewed like code.
          </p>
        </div>
        <div className="eng-gov-layer" data-layer="mode">
          <p className="eng-gov-path">.stella/governance.toml</p>
          <h3 className="eng-gov-name">the governance mode</h3>
          <p className="eng-gov-body">
            How much authority a record may hold. This repository runs{" "}
            <BrandFace>regulated</BrandFace> — enforcement is a grant, not a
            default, and a record earns it through promotion rather than by
            existing.
          </p>
        </div>
        <div className="eng-gov-layer" data-layer="ledger">
          <p className="eng-gov-path">.stella/promotions.jsonl</p>
          <h3 className="eng-gov-name">the hash-chained ledger</h3>
          <p className="eng-gov-body">
            Every enforcement grant, retirement, and supersession, chained so
            the history cannot be quietly rewritten.{" "}
            <BrandFace>stella context validate</BrandFace> re-verifies the
            records and the chain in CI on every pull request.
          </p>
        </div>
      </div>

      {/* Egress. The strongest claim on the site, so it is stated as an
          enforcement mechanism rather than a promise. */}
      <div className="eng-egress">
        <h3 className="eng-egress-title">zero telemetry egress by default</h3>
        <p className="eng-egress-body">
          Community <BrandFace>stella</BrandFace> sends telemetry nowhere. Your
          prompts, paths, tool payloads, reasoning, errors, git state,
          memories, and rules are <strong>never exportable</strong> — not
          off by default, not exportable. There is no update check and no
          anonymous analytics. Model-provider traffic is the normal network
          exception you chose when you brought your own key.
        </p>
        <p className="eng-egress-body">
          The one addition is an explicitly enrolled Oxagen Enterprise managed
          mode: a signed org-managed document may authorize a minimal
          content-free operational rollup to one exact allowlisted HTTPS sink.
        </p>
        <p className="eng-egress-enforced">
          <span className="eng-egress-enforced-label">enforced, not assumed</span>
          A reviewed allowlist of exportable columns plus a sentinel harness
          every egress encoder registers with. Adding a field fails the build
          until the allowlist is edited in the same change — so a human has to
          answer &quot;is this content?&quot; before it can ship. A leak here
          is a privacy incident, not a bug.
        </p>
      </div>

      <p className="eng-gov-more">
        <Link href="/docs/telemetry">what is metered, and where it stays</Link>
      </p>
    </Station>
  );
}

/* ── 05 · evolution ──────────────────────────────────────────────────────── */

/**
 * The paid tier's three engines, each with an honest availability status.
 *
 * `status` maps to /docs/self-improvement § "What is actually built today":
 * the Tool Foundry ships in slices (#830) and the dataset export ships
 * (#872), skill mining ships but still promotes on recurrence rather than
 * measured lift (#833), and weight-space adapters (#836) are not implemented.
 * The chips exist so a reader can tell a shipped capability from a designed
 * one without reading the issue tracker — the alternative is a page that is
 * wrong the day someone checks.
 */
const EVOLUTION = [
  {
    key: "skills",
    name: "skill mining",
    status: "partial",
    statusLabel: "ships · lift gate in design",
    claim: "your team's recurring wins become reusable procedures",
    body: "Every turn is reflected on and mined. A lesson that keeps recurring is promoted into a Skill — a parameterized procedure, not a recalled fact — and injected as volatile context when it is selected. Mining ships today; what is still in design is the evaluation run that promotes on measured lift instead of on recurrence.",
    issue: "833",
  },
  {
    key: "foundry",
    name: "the tool foundry",
    status: "shipped",
    statusLabel: "ships in slices",
    claim: "the engine builds the tool it keeps wishing it had",
    body: "When stella reconstructs the same shell incantation for the third time, the capability-gap detector notices. The foundry authors a typed tool, proves it with a witness test, and files it for adoption. A proposed tool is inert until it is both adopted and enabled — new capability lands disabled, behind the same authority gate as any other.",
    issue: "830",
  },
  {
    key: "weights",
    name: "your own weighted model",
    status: "design",
    statusLabel: "dataset ships · adapters in design",
    claim: "frontier-quality work on a model your team's traces taught",
    body: "Accepted, verified turns export as one JSONL record each, every string through the secret redactor, owner-only, with a manifest describing the filter that selected them. The frontier item is what comes next: train and evaluate open-weight adapters on that corpus offline, and — gated by eval and human sign-off — promote one that measurably improves behavior. The economics are the point: inference on a model you own is inference you are not renting.",
    issue: "836",
  },
] as const;

export function EvolutionStation() {
  return (
    <Station id="evolution">
      <StationHead
        id="evolution"
        code="05"
        name="the evolution deck"
        badge="paid tier"
        title={
          <>
            the engine that{" "}
            <span className="eng-title-accent">rebuilds itself</span>{" "}
            — from your team&apos;s traces, not from a vendor&apos;s.
          </>
        }
      />
      <Lead>
        Most of what gets called agent self-improvement is recall: a note file
        the agent writes and reads back. Useful — and not the same thing as
        getting <em>better</em>. Each engine on this deck changes what{" "}
        <BrandFace>stella</BrandFace> can <em>do</em>, and each one is gated on
        a measured lift against data it did not train on.
      </Lead>

      <div className="eng-evo">
        {EVOLUTION.map((engine) => (
          <article key={engine.key} className="eng-evo-card">
            <header className="eng-evo-head">
              <h3 className="eng-evo-name">{engine.name}</h3>
              <span className="eng-evo-status" data-status={engine.status}>
                {engine.statusLabel}
              </span>
            </header>
            <p className="eng-evo-claim">{engine.claim}</p>
            <p className="eng-evo-body">{engine.body}</p>
            <p className="eng-evo-issue">
              <a href={`${REPO_URL}/issues/${engine.issue}`}>
                design and status — #{engine.issue}
              </a>
            </p>
          </article>
        ))}
      </div>

      {/* The guardrails are the offer, not the footnote. */}
      <div className="eng-evo-rails">
        <p className="eng-evo-rails-label">
          the conditions under which any of it is allowed to ship —
        </p>
        <ul className="eng-evo-rails-list">
          <li>
            <strong>promotion requires measured lift on held-out data.</strong>{" "}
            Never frequency, never plausibility, never recency.
          </li>
          <li>
            <strong>human sign-off on anything that sticks.</strong>{" "}
            Self-authored pull requests open as drafts and never auto-merge.
            An adapter is never promoted to default without a person approving
            it.
          </li>
          <li>
            <strong>reversibility.</strong> The prior state is kept, so every
            promotion can be rolled back — a pinned adapter version, a prior
            config overlay, a Skill that stopped helping and was retired.
          </li>
          <li>
            <strong>provenance and redaction.</strong> Training data carries
            its lineage and is scrubbed before it is stored. Never train on
            secrets or PII.
          </li>
        </ul>
      </div>

      <p className="eng-evo-honest">
        <span className="eng-evo-honest-mark" aria-hidden="true">
          ✦
        </span>
        <span>
          <strong>What this deck is, today.</strong> The chips above are
          accurate as of this writing: some of this ships, some of it is a
          design under active discussion in the open issue tracker, and the
          details will change. We would rather you read a status chip than
          discover the gap after you paid.{" "}
          <Link href="/docs/self-improvement">
            the full track, component by component
          </Link>
        </span>
      </p>
    </Station>
  );
}

/* ── 06 · exit ───────────────────────────────────────────────────────────── */

const INSTALL = "curl -fsSL https://stella.oxagen.sh/install.sh | sh";

export function ExitStation() {
  return (
    <Station id="exit">
      <StationHead
        id="exit"
        code="06"
        name="the exit"
        title={
          <>
            you have seen the engine.{" "}
            <span className="eng-title-accent">now run it.</span>
          </>
        }
      />
      <Lead>
        One binary, one API key you already have. No account, no sign-up,
        nothing proxied. The engine you just walked through is the one that
        installs.
      </Lead>

      <div className="eng-exit-install">
        <InstallBlock command={INSTALL} />
      </div>

      <div className="eng-exit-doors">
        <Link href="/docs/getting-started/installation" className="eng-cta">
          install and authenticate
        </Link>
        <Link href="/docs" className="eng-cta-quiet">
          read the docs
        </Link>
        <a href={REPO_URL} className="eng-cta-quiet">
          source on github — agpl 3.0
        </a>
      </div>

      <p className="eng-exit-coda">
        <BrandFace>stella</BrandFace> is open source and built in the open by{" "}
        <a href="https://github.com/macanderson">@macanderson</a>. Every
        mechanism on this tour is documented in full, including the parts that
        are still a design —{" "}
        <Link href="/docs/plugins">start with plugins</Link>.
      </p>
    </Station>
  );
}
