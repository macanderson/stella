/**
 * Inline-SVG diagrams for the docs.
 *
 * These used to animate: dashes travelling along every wire on a 7-second loop,
 * boxes pulsing their opacity, a checkmark drawing and erasing itself. All of
 * it is gone. A diagram is a thing you look at while reading the paragraph
 * beside it, and a picture that keeps moving pulls the eye off the sentence —
 * the motion never encoded anything the arrows do not already say.
 *
 * What is left, and the rules it follows:
 * - Theme-aware: every colour is a Fumadocs or brand token, so light and dark
 *   both read. The accent (blue) marks the one or two nodes each diagram is
 *   actually about; everything else is a seam.
 * - One stroke weight, one radius, two type sizes. Boxes are outlines, not
 *   fills, so a diagram reads as line art rather than as a stack of panels.
 * - Accessible: every diagram carries `role="img"` plus a `<title>` and an
 *   `aria-label` that states the same thing the picture does, so the content
 *   survives with images off.
 * - Server-safe: pure SVG, no client JS, and no per-diagram `<style>` tag —
 *   the `sdg-` (stella diagram) rules live once in src/app/global.css.
 */

/** Arrowhead shared by every wire. Defined per-SVG because ids are document-scoped. */
function Defs() {
  return (
    <defs>
      <marker
        id="sdg-arrow"
        viewBox="0 0 8 8"
        refX="7"
        refY="4"
        markerWidth="6"
        markerHeight="6"
        orient="auto-start-reverse"
      >
        <path
          d="M0.5 0.8 L7.2 4 L0.5 7.2"
          fill="none"
          stroke="var(--stella-rule)"
          strokeWidth="1.4"
          strokeLinecap="round"
        />
      </marker>
    </defs>
  );
}

/**
 * A labelled box. Nearly every diagram below is boxes-and-wires, and hand-
 * writing the rect/label/sub triple three times per diagram is where typos in
 * `textAnchor` and off-by-one `y` offsets come from.
 */
function Node({
  x,
  y,
  w = 150,
  h = 52,
  label,
  sub,
  accent = false,
}: {
  x: number;
  y: number;
  w?: number;
  h?: number;
  /** Omit for a box whose only content is its sub-label (a caption box). */
  label?: string;
  sub?: string;
  accent?: boolean;
}) {
  const cx = x + w / 2;
  // With a sub-label the pair is centered as a block; alone, the label centers
  // on the box itself.
  const labelY = sub ? y + h / 2 - 2 : y + h / 2 + 4;
  const subY = label ? y + h / 2 + 15 : y + h / 2 + 4;
  return (
    <g>
      <rect
        className={accent ? "sdg-box-accent" : "sdg-box"}
        x={x}
        y={y}
        width={w}
        height={h}
        rx={6}
      />
      {label && (
        <text className="sdg-label" x={cx} y={labelY} textAnchor="middle">
          {label}
        </text>
      )}
      {sub && (
        <text className="sdg-sub" x={cx} y={subY} textAnchor="middle">
          {sub}
        </text>
      )}
    </g>
  );
}

/** A wire between two nodes. `arrow` off for a leg that only joins, not flows. */
function Wire({ d, arrow = true }: { d: string; arrow?: boolean }) {
  return (
    <path className="sdg-wire" d={d} markerEnd={arrow ? "url(#sdg-arrow)" : undefined} />
  );
}

/** Landing page: you → stella → your provider, telemetry staying local. */
export function HeroFlowDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 190"
      role="img"
      aria-label="Your prompt flows through stella to the provider you chose; telemetry stays on your machine."
    >
      <title>How Stella fits together</title>
      <Defs />
      <Node x={20} y={52} w={120} h={56} label="you" sub="a prompt, a goal" />
      <Node x={280} y={40} w={160} h={80} label="stella" sub="tools · pipeline · judge" accent />
      <Node x={580} y={52} w={120} h={56} label="provider" sub="your key, direct" />
      <Wire d="M140 80 H278" />
      <Wire d="M440 80 H578" />
      <Wire d="M360 120 V148" arrow={false} />
      <Node x={285} y={150} w={150} h={32} sub=".stella/ — telemetry stays here" />
    </svg>
  );
}

/** Inference pipeline: the staged flow with the revise return edge. */
export function PipelineFlowDiagram() {
  const stages: [string, string][] = [
    ["triage", "route it"],
    ["plan", "split context"],
    ["witness", "failing test"],
    ["execute", "step loop"],
    ["verify", "flip oracle"],
    ["judge", "cross-family"],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 150"
      role="img"
      aria-label="The staged pipeline: triage, plan, witness, execute, verify, judge — with a revise loop back into execute."
    >
      <title>The staged inference pipeline</title>
      <Defs />
      {stages.map(([name, sub], i) => {
        const x = 16 + i * 118;
        return (
          <g key={name}>
            <Node
              x={x}
              y={44}
              w={100}
              h={52}
              label={name}
              sub={sub}
              accent={name === "verify" || name === "judge"}
            />
            {i < stages.length - 1 && <Wire d={`M${x + 100} 70 H${x + 116}`} />}
          </g>
        );
      })}
      {/* revise: judge back to execute */}
      <Wire d="M672 96 C672 132 420 132 420 98" />
      <text className="sdg-sub" x="546" y="142" textAnchor="middle">
        revise — bounded, with evidence
      </text>
    </svg>
  );
}

/** Context engine: the recall → work → cite/reflect loop around the stores. */
export function RecallLoopDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 200"
      role="img"
      aria-label="Memories and the code graph feed the recall block, the model works, citations and reflections feed back into the stores."
    >
      <title>The context loop</title>
      <Defs />
      <Node x={24} y={40} w={168} h={46} label="memories · rules" sub=".stella/memories · rules" />
      <Node x={24} y={112} w={168} h={46} label="code graph" sub="tree-sitter index" />
      <Node x={280} y={76} w={160} h={48} label="recall block" sub="5 frames · ~1,200 tokens" accent />
      <Node x={528} y={76} w={168} h={48} label="the model turn" sub="cached prefix + recall" />
      <Wire d="M192 63 C240 63 240 88 278 92" />
      <Wire d="M192 135 C240 135 240 112 278 108" />
      <Wire d="M440 100 H526" />
      {/* feedback: cite_memory / reflections back to the stores */}
      <Wire d="M612 124 C612 182 108 182 108 160" />
      <text className="sdg-sub" x="360" y="176" textAnchor="middle">
        cite_memory · reflections · episodes — memory that earns its place
      </text>
    </svg>
  );
}

/** Fleet: one base commit fanning out to worktree lanes, converging on review. */
export function FleetFanoutDiagram() {
  const lanes = [
    { y: 40, label: "fleet/t1" },
    { y: 90, label: "fleet/t2" },
    { y: 140, label: "fleet/t3" },
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 180"
      role="img"
      aria-label="A pinned base commit fans out to isolated worktree branches; finished branches converge on your review."
    >
      <title>Fleet fan-out over git worktrees</title>
      <Defs />
      <circle className="sdg-dot" cx="60" cy="90" r="6" />
      <text className="sdg-label" x="60" y="66" textAnchor="middle">
        base
      </text>
      <text className="sdg-sub" x="60" y="118" textAnchor="middle">
        pinned SHA
      </text>
      {lanes.map(({ y, label }) => (
        <g key={label}>
          <Wire d={`M68 90 C130 90 130 ${y} 190 ${y} H238`} />
          <Node x={240} y={y - 15} w={180} h={30} sub={`${label} — its own worktree`} />
          <Wire d={`M420 ${y} H520 C580 ${y} 580 90 636 90`} />
        </g>
      ))}
      <circle className="sdg-dot" cx="648" cy="90" r="6" />
      <text className="sdg-label" x="648" y="66" textAnchor="middle">
        review
      </text>
      <text className="sdg-sub" x="648" y="118" textAnchor="middle">
        merge on your terms
      </text>
    </svg>
  );
}

/** Getting started: the four commands between an empty shell and a shipped change. */
export function QuickstartDiagram() {
  const steps: [string, string][] = [
    ["install", "one curl | sh"],
    ["authenticate", "export one key"],
    ["init", "learn the repo"],
    ["run", "ship a change"],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 132"
      role="img"
      aria-label="Four steps: install, authenticate, init, run."
    >
      <title>From empty shell to first change</title>
      <Defs />
      {steps.map(([label, sub], i) => {
        const x = 16 + i * 174;
        return (
          <g key={label}>
            <Node x={x} y={32} w={150} h={56} label={label} sub={sub} accent={i === 3} />
            {i < steps.length - 1 && <Wire d={`M${x + 150} 60 H${x + 172}`} />}
          </g>
        );
      })}
      <text className="sdg-sub" x="360" y="116" textAnchor="middle">
        about two minutes, end to end — no account, no proxy
      </text>
    </svg>
  );
}

/**
 * Credentials: the five sources, tried top to bottom, first hit wins. Drawn as
 * a ladder rather than a list because the *fall-through* is the point — the
 * rungs are ordered, and any one of them can end the search.
 */
export function CredentialChainDiagram() {
  const rungs = [
    "--api-key flag (needs an explicit --model)",
    "the provider's env var, plus its aliases",
    "settings.json — providers.<id>.api_key",
    "~/.stella/credentials.toml",
    "interactive prompt — saved, so it asks once",
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 228"
      role="img"
      aria-label="Five credential sources tried in order — flag, environment variable, settings.json, credentials.toml, interactive prompt — and the first one that resolves is used."
    >
      <title>The credential chain</title>
      <Defs />
      {/* the fall-through spine: tried in order, top to bottom */}
      <Wire d="M28 24 V196" />
      {rungs.map((text, i) => {
        const y = 16 + i * 40;
        return (
          <g key={text}>
            <circle className="sdg-dot" cx={28} cy={y + 15} r={8} />
            <text
              className="sdg-sub"
              x={28}
              y={y + 19}
              textAnchor="middle"
              fill="var(--color-fd-background)"
              fontWeight={600}
            >
              {i + 1}
            </text>
            <rect className="sdg-box" x={48} y={y} width={312} height={30} rx={6} />
            <text className="sdg-sub" x={62} y={y + 19}>
              {text}
            </text>
            {/* every rung can exit to "resolved" — that is what first-hit-wins means */}
            <Wire d={`M360 ${y + 15} C410 ${y + 15} 410 122 456 122`} arrow={i === 0} />
          </g>
        );
      })}
      <Node x={458} y={96} w={230} h={52} label="key resolved" sub="first hit wins — the rest are skipped" accent />
      <text className="sdg-sub" x={204} y={214} textAnchor="middle">
        nothing below the first hit is ever read
      </text>
    </svg>
  );
}

/**
 * Settings scopes: three files merged per key, with the org layer acting as a
 * ceiling rather than another peer. The asymmetry is the whole diagram — a
 * cascade where one layer can only ever subtract.
 */
export function SettingsCascadeDiagram() {
  const layers: [string, string, boolean][] = [
    ["org-managed", "a ceiling — off stays off", true],
    ["project — .stella/settings.json", "beats user, for keys it is trusted with", false],
    ["user — ~/.stella/settings.json", "your defaults, always applied", false],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 216"
      role="img"
      aria-label="Org-managed, project, and user settings merge per key into the effective settings; the org layer is a ceiling that lower scopes can narrow but never re-open."
    >
      <title>How settings scopes merge</title>
      <Defs />
      {layers.map(([label, sub, accent], i) => {
        const y = 20 + i * 64;
        return (
          <g key={label}>
            <Node x={24} y={y} w={330} h={52} label={label} sub={sub} accent={accent} />
            <Wire d={`M354 ${y + 26} C420 ${y + 26} 420 110 464 110`} arrow={i === 1} />
          </g>
        );
      })}
      <Node x={466} y={84} w={224} h={52} label="effective settings" sub="merged per key" accent />
      <text className="sdg-sub" x="360" y="204" textAnchor="middle">
        most specific wins — except that an org &ldquo;off&rdquo; can be narrowed further, never
        re-opened
      </text>
    </svg>
  );
}

/**
 * The permission gate a tool call passes through. Both exits are drawn: the
 * refusal is a real, documented outcome, not an error path to hide.
 */
export function PermissionGateDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 200"
      role="img"
      aria-label="A tool call passes through the PreToolUse hook and the permission rules; allowed calls execute and fire PostToolUse, denied calls come back to the model as a refusal."
    >
      <title>How a tool call is gated</title>
      <Defs />
      <Node x={12} y={74} w={128} h={52} label="tool call" sub="the model asks" />
      <Node x={168} y={74} w={150} h={52} label="PreToolUse" sub="your hook may veto" />
      <Node x={346} y={74} w={150} h={52} label="permission" sub="rules + settings" accent />
      <Wire d="M140 100 H166" />
      <Wire d="M318 100 H344" />
      <Wire d="M496 92 C536 92 536 46 560 46" />
      <Node x={562} y={20} w={142} h={52} label="execute" sub="then PostToolUse" accent />
      <text className="sdg-sub" x={528} y={62}>
        allow
      </text>
      <Wire d="M496 108 C536 108 536 154 560 154" />
      <Node x={562} y={128} w={142} h={52} label="refused" sub="returned to the model" />
      <text className="sdg-sub" x={528} y={150}>
        deny
      </text>
      <text className="sdg-sub" x="248" y="188" textAnchor="middle">
        enforced at the tool boundary — never by prompt discipline
      </text>
    </svg>
  );
}

/**
 * Embedding guide: the ownership split. The reference page draws the *sequence*
 * (ask, answer, ask, answer); this draws the *boundary*, which is the thing a
 * reader is actually deciding about — what of theirs has to cross it. Nothing
 * does. The caption names the absence, the way the telemetry diagram does.
 */
export function EngineOwnershipDiagram() {
  const yours = [
    "provider keys · gateway · routing",
    "tools · sandbox · RBAC",
    "history · billing · your data",
  ];
  const engine: [string, boolean][] = [
    ["step loop · tool dispatch", true],
    ["compaction · token budget", false],
    ["retry class · loop detect · caps", false],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 232"
      role="img"
      aria-label="Your app keeps keys, tools, and data; the engine keeps the loop, compaction, and retries. Only requests and results cross between them."
    >
      <title>What the engine owns, and what never leaves your app</title>
      <Defs />
      <text className="sdg-label" x={16} y={30}>
        your app
      </text>
      <rect className="sdg-box" x={16} y={40} width={300} height={144} rx={6} />
      {yours.map((text, i) => (
        <g key={text}>
          <rect className="sdg-box" x={32} y={52 + i * 42} width={268} height={34} rx={6} />
          <text className="sdg-sub" x={46} y={73 + i * 42}>
            {text}
          </text>
        </g>
      ))}
      <text className="sdg-label" x={404} y={30}>
        stella-serve
      </text>
      <rect className="sdg-box" x={404} y={40} width={300} height={144} rx={6} />
      {engine.map(([text, accent], i) => (
        <g key={text}>
          <rect
            className={accent ? "sdg-box-accent" : "sdg-box"}
            x={420}
            y={52 + i * 42}
            width={268}
            height={34}
            rx={6}
          />
          <text className="sdg-sub" x={434} y={73 + i * 42}>
            {text}
          </text>
        </g>
      ))}
      {/* the engine asks; your app answers. Nothing else crosses. */}
      <Wire d="M402 86 H318" />
      <text className="sdg-sub" x={360} y={78} textAnchor="middle">
        asks
      </text>
      <Wire d="M318 140 H402" />
      <text className="sdg-sub" x={360} y={132} textAnchor="middle">
        answers
      </text>
      <text className="sdg-sub" x="360" y="212" textAnchor="middle">
        no HTTP client, no TLS stack, no provider adapter — it cannot leak a key it never holds
      </text>
    </svg>
  );
}

/**
 * Embedding guide, second half: because your app *is* the model, one host runs
 * against a gateway or against a scripted reply, and the loop cannot tell. That
 * is the whole argument for testing an agent loop in CI, so it gets a picture.
 */
export function EngineTestHarnessDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 196"
      role="img"
      aria-label="One host drives either your real gateway or a scripted reply function; both exercise the identical agent loop, but only one of them spends money."
    >
      <title>The same host, with and without a model</title>
      <Defs />
      <Node x={16} y={62} w={162} h={56} label="one host" sub="the code you just wrote" />
      <Wire d="M178 78 C226 78 226 44 264 44" />
      <Node x={266} y={18} w={186} h={52} label="your gateway" sub="real models · real spend" />
      <Wire d="M178 102 C226 102 226 142 264 142" />
      <Node x={266} y={116} w={186} h={52} label="scripted replies" sub="a function returning JSON" />
      <Wire d="M452 44 C500 44 500 82 522 90" />
      <Wire d="M452 142 C500 142 500 104 522 98" />
      <Node x={524} y={68} w={180} h={52} label="the identical loop" sub="same frames · same steps" accent />
      <text className="sdg-sub" x="360" y="186" textAnchor="middle">
        one of those two paths costs nothing and finishes in milliseconds — that is the one CI runs
      </text>
    </svg>
  );
}

/**
 * CI guide: two engines, one task set, and — the asymmetry that is the whole
 * point — two different kinds of exit. Loop correctness is deterministic and
 * therefore allowed to block; a quality delta is a model-quality measurement
 * and is only ever allowed to inform.
 */
export function EngineGateDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 244"
      role="img"
      aria-label="One committed task set runs through both Stella and Claude Code; the comparison produces a blocking loop-correctness verdict and an advisory quality delta."
    >
      <title>Two engines, one task set, two kinds of exit</title>
      <Defs />
      <Node x={8} y={86} w={126} h={52} label="one task set" sub="in your repo" />
      <Wire d="M134 112 C156 112 156 54 174 54" />
      <Wire d="M134 112 C156 112 156 170 174 170" />
      <Node x={176} y={28} w={168} h={52} label="stella" sub="--output-format json" />
      <Node x={176} y={144} w={168} h={52} label="claude code" sub="-p --output-format json" />
      <Wire d="M344 54 C366 54 366 112 388 112" />
      <Wire d="M344 170 C366 170 366 112 388 112" />
      <Node x={390} y={86} w={124} h={52} label="compare" sub="two receipts" />
      <Wire d="M514 100 C534 100 534 46 554 46" />
      <Wire d="M514 124 C534 124 534 178 554 178" />
      <Node x={556} y={20} w={156} h={52} label="loop correctness" sub="exit 1 — blocks" accent />
      <Node x={556} y={152} w={156} h={52} label="quality delta" sub="a comment, not a gate" />
      <text className="sdg-sub" x="360" y="230" textAnchor="middle">
        the deterministic half is allowed to block; the model-quality half is only allowed to inform
      </text>
    </svg>
  );
}

/**
 * CI guide: loop-bench's verdict ladder, in its real precedence order. Drawn as
 * rungs because the *order* is load-bearing — reward outranks everything, so a
 * solved task is never called silent no matter what its event stream lost.
 */
export function LoopVerdictDiagram() {
  const rungs: [string, string, boolean][] = [
    ["reward is 1.0 — the task was solved", "solved", false],
    ["zero tool calls, and no terminal event", "SILENT-DEATH", true],
    ["zero tool calls, but it said why", "ZERO-WORK", true],
    ["tool calls happened; the verifier said no", "ran (unsolved)", false],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 224"
      role="img"
      aria-label="Four verdicts checked in order: solved, silent death, zero work, ran but unsolved. The middle two — a turn that did nothing — are the ones that fail the gate."
    >
      <title>The loop-correctness verdict ladder</title>
      <Defs />
      {/* checked top to bottom; the first match wins */}
      <Wire d="M28 24 V170" />
      {rungs.map(([test, verdict, fails], i) => {
        const y = 16 + i * 44;
        return (
          <g key={verdict}>
            <circle className="sdg-dot" cx={28} cy={y + 16} r={8} />
            <text
              className="sdg-sub"
              x={28}
              y={y + 20}
              textAnchor="middle"
              fill="var(--color-fd-background)"
              fontWeight={600}
            >
              {i + 1}
            </text>
            <rect className="sdg-box" x={48} y={y} width={296} height={32} rx={6} />
            <text className="sdg-sub" x={62} y={y + 20}>
              {test}
            </text>
            <Wire d={`M344 ${y + 16} H392`} />
            <Node x={394} y={y} w={150} h={32} label={verdict} accent={fails} />
          </g>
        );
      })}
      {/* the bracket spans exactly the two verdicts that exit non-zero */}
      <Wire d="M556 60 H570 V136 H556" arrow={false} />
      <text className="sdg-label" x={582} y={94}>
        exit 1
      </text>
      <text className="sdg-sub" x={582} y={110}>
        the gate trips
      </text>
      <text className="sdg-sub" x="296" y="212" textAnchor="middle">
        loop health, not pass rate — a task nobody solves still passes, provided the loop ran
      </text>
    </svg>
  );
}

/**
 * Telemetry: where a run's numbers go. The point of the picture is the absent
 * arrow — there is no edge leaving the machine, so there is nothing to opt out
 * of.
 */
export function TelemetryFlowDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 196"
      role="img"
      aria-label="Each session turn writes a receipt into .stella/ on your disk, which stella stats and the observatory read back. No arrow leaves the machine."
    >
      <title>Where telemetry goes</title>
      <Defs />
      <Node x={12} y={70} w={150} h={54} label="a session turn" sub="tokens · tools · files" />
      <Node x={196} y={70} w={150} h={54} label="a receipt" sub="one row, per turn" accent />
      <Node x={380} y={70} w={158} h={54} label=".stella/" sub="on your disk, as JSON" />
      <Wire d="M162 97 H194" />
      <Wire d="M346 97 H378" />
      <Wire d="M538 84 C566 84 566 48 590 48" />
      <Wire d="M538 110 C566 110 566 146 590 146" />
      <Node x={592} y={22} w={116} h={52} label="stella stats" sub="the numbers" />
      <Node x={592} y={120} w={116} h={52} label="observatory" sub="the dashboard" />
      <text className="sdg-sub" x="275" y="184" textAnchor="middle">
        no arrow leaves this picture — there is no endpoint, so there is nothing to opt out of
      </text>
    </svg>
  );
}
