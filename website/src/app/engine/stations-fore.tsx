import Link from "next/link";
import {
  BrandFace,
  EventFeed,
  FactCard,
  Lead,
  Station,
  StationHead,
  StationHeadH1,
} from "./station";

/**
 * The fore of the engine: intake (the hatch), the turn loop (the heart), and
 * the tool bay. Aft stations — vera's chamber, the governance plane, the
 * evolution deck, the exit — live in stations-aft.tsx; the split keeps each
 * file a readable size rather than one god page.
 *
 * Copy discipline: every mechanical claim here restates a documented one —
 * /docs/inference-pipeline for the loop and stages, /docs/agent-tools for the
 * bay. The tour is allowed to be theatrical about *presentation*, never about
 * *facts*.
 */

/* ── 00 · intake ─────────────────────────────────────────────────────────── */

export function IntakeStation() {
  return (
    <Station id="intake">
      <StationHeadH1
        id="intake"
        code="00"
        name="intake"
        title={
          <>
            you are standing inside
            <br />a coding agent.
          </>
        }
      />
      <Lead>
        Most sites tell you what their agent does. This one walks you through
        the machine while it works. <BrandFace>stella</BrandFace> is a
        terminal coding agent — fast, BYOK, model-agnostic — and it refuses to
        call work done until the work is proven. Walk the engine and watch a
        goal become verified work.
      </Lead>

      {/* The goal enters through the hatch: the one gold prompt on deck. */}
      <div className="eng-goal" role="img" aria-label="A goal entering the engine">
        <p className="eng-goal-line">
          <span className="eng-goal-prompt">$ </span>
          stella run &quot;make the flaky scheduler test deterministic&quot;
        </p>
        <p className="eng-goal-status">
          <span className="eng-goal-dot" /> goal received — engine turning
        </p>
      </div>

      <ul className="eng-intake-facts">
        <li>runs on the API keys you already have</li>
        <li>speaks ten providers&apos; native protocols — nothing emulated</li>
        <li>nothing proxied through a hosted service</li>
        <li>telemetry never leaves your disk</li>
      </ul>

      <div className="eng-intake-ctas">
        <a href="#loop" className="eng-cta">
          begin the tour ↓
        </a>
        <a href="#exit" className="eng-cta-quiet">
          skip to the exit — install
        </a>
      </div>
    </Station>
  );
}

/* ── 01 · the turn loop ──────────────────────────────────────────────────── */

const LOOP_FEED = [
  '{"type":"step_manifest","turn":4,"step":2,"planned_tools":3}',
  '{"type":"tool_start","tool":"task_list","call_seq":0}',
  '{"type":"tool_start","tool":"get_state","call_seq":0}',
  '{"type":"tool_result","tool":"task_list","status":"ok"}',
  '{"type":"tool_result","tool":"get_state","status":"ok"}',
  '{"type":"step_receipt","turn":4,"step":2,"outcome":"continue"}',
  '{"type":"step_usage","role":"worker","tokens_in":18204,"cached":17651}',
] as const;

/** The staged pipeline, in canonical stage_rank order. Dashed = conditional. */
const PIPELINE_STAGES = [
  { name: "triage", conditional: false },
  { name: "recall", conditional: false },
  { name: "research", conditional: true },
  { name: "plan", conditional: true },
  { name: "scope", conditional: true },
  { name: "execute", conditional: false },
  { name: "witness", conditional: true },
  { name: "verify", conditional: false },
  { name: "verdict", conditional: false },
] as const;

export function TurnLoopStation() {
  return (
    <Station id="loop">
      <StationHead
        id="loop"
        code="01"
        name="the turn loop"
        title={
          <>
            one loop. no coordinator, no hidden swarm —{" "}
            <span className="eng-title-accent">and no I/O in the core.</span>
          </>
        }
      />
      <Lead>
        At the heart of the engine is a single-threaded step loop: the model
        proposes tool calls, the tools execute — independent reads fan out in
        parallel — the results feed back, and the loop repeats until the work
        is done. That is the entire trick. Everything else on this tour is
        structure built on top of it.
      </Lead>

      {/* The loop, drawn: three stations on a ring, guards on the return
          edge, the decision core in the middle, a comet running the track. */}
      <div className="eng-ring-scene">
        <div
          className="eng-ring"
          role="img"
          aria-label="The turn loop: the model proposes, tools execute, results feed back — with compaction, budget, and loop-detection guards on the return edge, around a pure decision core"
        >
          <div className="eng-ring-track" aria-hidden="true" />
          <div className="eng-ring-orbit" aria-hidden="true">
            <span className="eng-ring-comet" />
          </div>
          <p className="eng-ring-node" data-pos="top">
            model proposes
          </p>
          <p className="eng-ring-node" data-pos="right">
            tools execute
          </p>
          <p className="eng-ring-node" data-pos="bottom">
            results feed back
          </p>
          <p className="eng-ring-gate" data-pos="left">
            <span className="eng-ring-gate-label">guards</span>
            compact · budget · loop-detect
          </p>
          <p className="eng-ring-core">
            pure decision core
            <span>no network · no filesystem · no clock</span>
          </p>
        </div>

        <div className="eng-ring-copy">
          <FactCard title="decisions are pure functions">
            Compaction, eviction, budget arithmetic, loop detection, retry,
            skill selection — every decision the engine makes is a synchronous
            function over owned data. No I/O can happen inside the core, so
            the loop is property-tested, replayable, and deterministic enough
            to prove things about.
          </FactCard>
          <FactCard title="guards fire at safe boundaries">
            The budget guard is consulted between model calls and never
            interrupts a tool in flight — a run stops at a seam, not
            mid-motion. When the conversation gets noisy, the engine compacts
            it and keeps going.
          </FactCard>
        </div>
      </div>

      {/* The staged pipeline the loop rides under `stella run`. */}
      <div className="eng-pipeline">
        <p className="eng-pipeline-label">
          on <BrandFace>stella run</BrandFace>, the loop rides a staged
          pipeline —
        </p>
        <ol className="eng-pipeline-strip">
          {PIPELINE_STAGES.map((stage) => (
            <li
              key={stage.name}
              className="eng-pipeline-stage"
              data-conditional={stage.conditional || undefined}
            >
              {stage.name}
            </li>
          ))}
        </ol>
        <p className="eng-pipeline-note">
          dashed stages are conditional: a task that does not need them skips
          them — they are never performed for show.{" "}
          <Link href="/docs/inference-pipeline">the full pipeline, staged</Link>
        </p>
      </div>

      <EventFeed lines={LOOP_FEED} />
    </Station>
  );
}

/* ── 02 · the tool bay ───────────────────────────────────────────────────── */

/**
 * A walk of the racks — the real built-in tool names and access levels from
 * /docs/agent-tools: the whole 12-tool surface. `read` racks are plain; a
 * `mutating` tool carries the amber edge, the same convention the docs'
 * tool cards use.
 */
const TOOL_RACKS: readonly {
  rack: string;
  tools: readonly { name: string; access: "read" | "mutating" }[];
}[] = [
  {
    rack: "task board",
    tools: [
      { name: "task_create", access: "mutating" },
      { name: "task_list", access: "read" },
      { name: "task_start", access: "mutating" },
      { name: "task_complete", access: "mutating" },
      { name: "task_cancel", access: "mutating" },
    ],
  },
  {
    rack: "sub-agents",
    tools: [
      { name: "task", access: "mutating" },
      { name: "task_assign", access: "mutating" },
    ],
  },
  {
    rack: "scratch state",
    tools: [
      { name: "save_state", access: "mutating" },
      { name: "get_state", access: "read" },
      { name: "list_state", access: "read" },
      { name: "delete_state", access: "mutating" },
    ],
  },
  {
    rack: "environment",
    tools: [{ name: "get_environment", access: "read" }],
  },
] as const;

export function ToolBayStation() {
  return (
    <Station id="tools">
      <StationHead
        id="tools"
        code="02"
        name="the tool bay"
        title={
          <>
            every capability is a tool.{" "}
            <span className="eng-title-accent">
              every tool does exactly one thing.
            </span>
          </>
        }
      />
      <Lead>
        A parameter may <em>scope</em> an operation — a path, a key, an offset.
        It may never <em>select</em> one. A tool that can both edit and delete
        is two tools wearing one schema, so <BrandFace>stella</BrandFace>{" "}
        splits it: the model learns each verb properly, and policy can withhold
        the destructive verb without withholding the benign one.
      </Lead>

      <div className="eng-bay">
        {TOOL_RACKS.map((rack) => (
          <div key={rack.rack} className="eng-bay-rack">
            <h3 className="eng-bay-rack-name">{rack.rack}</h3>
            <ul className="eng-bay-tools">
              {rack.tools.map((tool) => (
                <li
                  key={tool.name}
                  className="eng-bay-tool"
                  data-access={tool.access}
                >
                  {tool.name}
                </li>
              ))}
            </ul>
          </div>
        ))}
        <div className="eng-bay-rack">
          <h3 className="eng-bay-rack-name">docked via MCP</h3>
          <p className="eng-bay-mcp">
            your external tool servers merge into the same registry, behind the
            same per-tool permissions — a guest tool follows house rules.
          </p>
        </div>
      </div>

      <p className="eng-bay-legend">
        <span className="eng-bay-key" data-access="read">
          read
        </span>
        <span className="eng-bay-key" data-access="mutating">
          mutating
        </span>{" "}
        — every tool declares which it is, honestly: the engine&apos;s
        concurrency contract depends on it. This is the whole built-in
        registry; everything else docks via MCP or a custom manifest.{" "}
        <Link href="/docs/agent-tools">The full toolbelt and its permissions</Link>
        .
      </p>
    </Station>
  );
}
