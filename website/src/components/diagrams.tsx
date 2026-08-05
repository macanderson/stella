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
 * - Glyphs are line art at the same weight, and only earn their place when the
 *   *kind* of a node is load-bearing — a subprocess versus a remote endpoint,
 *   a lock versus a box. A glyph that merely decorates a label is noise, so
 *   most diagrams here carry none.
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

/**
 * A wire between two nodes. `arrow` off for a leg that only joins, not flows;
 * `dashed` for an edge that carries nothing — a failed connect, or a lifeline
 * that is waiting rather than delivering.
 */
function Wire({
  d,
  arrow = true,
  dashed = false,
}: {
  d: string;
  arrow?: boolean;
  dashed?: boolean;
}) {
  return (
    <path
      className={dashed ? "sdg-wire sdg-wire-dashed" : "sdg-wire"}
      d={d}
      markerEnd={arrow ? "url(#sdg-arrow)" : undefined}
    />
  );
}

/**
 * Line-art glyphs, each drawn in a 20×20 box and placed by its top-left corner.
 * The set is deliberately small: a glyph is here to say what *kind* of thing a
 * node is when that distinction is the point of the diagram — a subprocess on
 * this machine versus an endpoint somewhere else, a lock versus a box.
 */
const GLYPHS = {
  /** A local subprocess: stdio, a shell, something Stella launched. */
  terminal: (
    <>
      <rect x="1.5" y="3.5" width="17" height="13" rx="2" />
      <path d="M5.2 8.4 L7.8 10.6 L5.2 12.8" />
      <path d="M10 12.8 H14.4" />
    </>
  ),
  /** A remote endpoint: reached over the network, not launched. */
  globe: (
    <>
      <circle cx="10" cy="10" r="7.5" />
      <path d="M2.5 10 H17.5" />
      <path d="M10 2.5 C13.3 5.4 13.3 14.6 10 17.5 C6.7 14.6 6.7 5.4 10 2.5 Z" />
    </>
  ),
  /** A held claim — a path one worker owns for the duration of its attempt. */
  lock: (
    <>
      <rect x="3.5" y="8.5" width="13" height="9" rx="2" />
      <path d="M6.5 8.5 V6.2 A3.5 3.5 0 0 1 13.5 6.2 V8.5" />
    </>
  ),
  /** A process you run: the engine, a sidecar. */
  server: (
    <>
      <rect x="2.5" y="3" width="15" height="6" rx="1.5" />
      <rect x="2.5" y="11" width="15" height="6" rx="1.5" />
      <circle cx="5.6" cy="6" r="0.9" />
      <circle cx="5.6" cy="14" r="0.9" />
    </>
  ),
  /** Your application — the thing with the users in front of it. */
  app: (
    <>
      <rect x="2.5" y="3.5" width="15" height="13" rx="2" />
      <path d="M2.5 7.4 H17.5" />
      <circle cx="5.4" cy="5.5" r="0.7" />
    </>
  ),
} as const;

function Glyph({
  kind,
  x,
  y,
  accent = false,
}: {
  kind: keyof typeof GLYPHS;
  x: number;
  y: number;
  accent?: boolean;
}) {
  return (
    <g
      className={accent ? "sdg-icon sdg-icon-accent" : "sdg-icon"}
      transform={`translate(${x} ${y})`}
    >
      {GLYPHS[kind]}
    </g>
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
      <Node x={280} y={40} w={160} h={80} label="stella" sub="tools · pipeline · verifier" accent />
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
    ["execute", "step loop"],
    ["witness", "failing test"],
    ["verify", "flip oracle"],
    ["verifier", "cross-family"],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 150"
      role="img"
      aria-label="The staged pipeline: triage, plan, execute, witness, verify, verifier — with a revise loop back into execute."
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
              accent={name === "verify" || name === "verifier"}
            />
            {i < stages.length - 1 && <Wire d={`M${x + 100} 70 H${x + 116}`} />}
          </g>
        );
      })}
      {/* revise: verifier back to execute */}
      <Wire d="M672 96 C672 132 302 132 302 98" />
      <text className="sdg-sub" x="487" y="142" textAnchor="middle">
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
            <text className="sdg-sub sdg-dot-numeral" x={28} y={y + 19} textAnchor="middle">
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
            <text className="sdg-sub sdg-dot-numeral" x={28} y={y + 20} textAnchor="middle">
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

/**
 * MCP: the connect fan-out at session start. Drawn as a topology rather than a
 * list because the two facts a reader needs are both about *edges* — a server
 * is either a subprocess on this machine or an endpoint somewhere else, and a
 * server that never answers loses its own tools without taking the session
 * with it. The dashed leg is that second fact; it is the only edge here that
 * delivers nothing.
 */
export function McpTopologyDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 252"
      role="img"
      aria-label="At session start Stella connects to each configured MCP server — stdio servers as local subprocesses, http servers as remote endpoints — and merges their tools into one namespaced tool set. A server that fails to connect within ten seconds is skipped, and the session continues without its tools."
    >
      <title>How MCP servers reach the agent</title>
      <Defs />
      <Node x={8} y={100} w={152} h={54} label="session start" sub="reads .stella/mcp.toml" />

      {/* stdio — a child process on this machine, with a scrubbed environment */}
      <rect className="sdg-box" x={210} y={38} width={250} height={46} rx={6} />
      <Glyph kind="terminal" x={226} y={51} />
      <text className="sdg-label" x={256} y={59}>
        filesystem
      </text>
      <text className="sdg-sub" x={256} y={73}>
        stdio — subprocess, scrubbed env
      </text>

      {/* http — an endpoint somewhere else, reached with static headers */}
      <rect className="sdg-box" x={210} y={104} width={250} height={46} rx={6} />
      <Glyph kind="globe" x={226} y={117} />
      <text className="sdg-label" x={256} y={125}>
        github
      </text>
      <text className="sdg-sub" x={256} y={139}>
        http — a bearer, replayed
      </text>

      {/* the one that did not answer */}
      <rect className="sdg-box" x={210} y={170} width={250} height={46} rx={6} />
      <Glyph kind="terminal" x={226} y={183} />
      <text className="sdg-label" x={256} y={191}>
        linear
      </text>
      <text className="sdg-sub" x={256} y={205}>
        connect timed out — skipped
      </text>

      <Wire d="M160 127 C186 127 186 61 208 61" />
      <Wire d="M160 127 H208" />
      <Wire d="M160 127 C186 127 186 193 208 193" />

      <Wire d="M460 61 C500 61 500 127 534 127" />
      <Wire d="M460 127 H534" />
      {/* nothing merges from a server that never connected */}
      <Wire d="M460 193 H508" arrow={false} dashed />
      <text className="sdg-sub" x={516} y={197}>
        nothing merges
      </text>

      <Node
        x={536}
        y={100}
        w={176}
        h={54}
        label="one tool set"
        sub="mcp__<server>__<tool>"
        accent
      />
      <text className="sdg-sub" x="360" y="240" textAnchor="middle">
        each connect is isolated, with a ten-second budget — a server that hangs costs you its
        tools, not your session
      </text>
    </svg>
  );
}

/**
 * Hooks: three lifecycle events on one turn, and the asymmetry that decides how
 * you use them — exactly one of the three can stop anything. Drawn as a spine
 * with a single downward exit, because a reader who takes only the shape away
 * has taken away the right thing.
 */
export function HookLifecycleDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 224"
      role="img"
      aria-label="Three hooks fire across a turn: SessionStart, whose stdout becomes context; PreToolUse, whose non-zero exit blocks the tool; and PostToolUse, whose exit status is ignored. Only PreToolUse can stop anything."
    >
      <title>The three hook points on a turn</title>
      <Defs />
      <Wire d="M16 80 H44" />
      <Node x={46} y={54} w={164} h={52} label="SessionStart" sub="stdout becomes context" />
      <Wire d="M210 80 H254" />
      <Node x={256} y={54} w={164} h={52} label="PreToolUse" sub="runs before the tool" accent />
      <Wire d="M420 80 H464" />
      <Node x={466} y={54} w={164} h={52} label="PostToolUse" sub="runs after the tool" />
      <Wire d="M630 80 H702" />

      {/* the only branch that leaves the spine */}
      <Wire d="M338 106 V144" />
      <text className="sdg-sub" x={346} y={130}>
        exit is non-zero
      </text>
      <Node
        x={237}
        y={146}
        w={202}
        h={44}
        label="refused"
        sub="the tool never runs"
      />
      <text className="sdg-sub" x="360" y="210" textAnchor="middle">
        only one of the three can stop anything — and it fails closed: a hook that times out also
        blocks
      </text>
    </svg>
  );
}

/**
 * Determinism: the argument this page makes is structural, so it should be
 * possible to see it. The two panels are drawn to the same scale on purpose —
 * what differs is the edge count, and edges are exactly what the MAST failure
 * classes live in. Nothing is labelled a failure; the tangle is the claim.
 */
export function SingleThreadDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 226"
      role="img"
      aria-label="A swarm is a coordinator and four agents joined by many edges, every one of them a handoff that summarizes. Stella is one ordered loop — plan, act, observe, compact — with a single return edge and one transcript."
    >
      <title>A swarm, and one deterministic loop, at the same scale</title>
      <Defs />

      <text className="sdg-label" x={16} y={28}>
        a swarm — N agents and a coordinator
      </text>
      <rect className="sdg-box" x={16} y={38} width={328} height={152} rx={6} />
      {/* every pair that can exchange a summary, drawn */}
      <Wire d="M180 68 L66 124" arrow={false} />
      <Wire d="M180 68 L128 150" arrow={false} />
      <Wire d="M180 68 L232 150" arrow={false} />
      <Wire d="M180 68 L294 124" arrow={false} />
      <Wire d="M66 124 L128 150" arrow={false} />
      <Wire d="M128 150 L232 150" arrow={false} />
      <Wire d="M232 150 L294 124" arrow={false} />
      <Wire d="M66 124 L232 150" arrow={false} />
      <Wire d="M128 150 L294 124" arrow={false} />
      <Wire d="M66 124 L294 124" arrow={false} />
      {/* seam-coloured, not gold: the accent belongs to the panel on the right */}
      <circle className="sdg-dot-muted" cx={180} cy={68} r={5} />
      <circle className="sdg-dot-muted" cx={66} cy={124} r={5} />
      <circle className="sdg-dot-muted" cx={128} cy={150} r={5} />
      <circle className="sdg-dot-muted" cx={232} cy={150} r={5} />
      <circle className="sdg-dot-muted" cx={294} cy={124} r={5} />
      <text className="sdg-sub" x={180} y={56} textAnchor="middle">
        coordinator
      </text>
      <text className="sdg-sub" x={180} y={178} textAnchor="middle">
        every edge is a handoff, and handoffs summarize
      </text>

      <text className="sdg-label" x={376} y={28}>
        Stella — one deterministic step loop
      </text>
      <rect className="sdg-box-accent" x={376} y={38} width={328} height={152} rx={6} />
      <text className="sdg-sub" x={420} y={74} textAnchor="middle">
        plan
      </text>
      <text className="sdg-sub" x={500} y={74} textAnchor="middle">
        act
      </text>
      <text className="sdg-sub" x={580} y={74} textAnchor="middle">
        observe
      </text>
      <text className="sdg-sub" x={660} y={74} textAnchor="middle">
        compact
      </text>
      <Wire d="M426 88 H494" />
      <Wire d="M506 88 H574" />
      <Wire d="M586 88 H654" />
      <Wire d="M660 94 C660 132 420 132 420 94" />
      <circle className="sdg-dot" cx={420} cy={88} r={5} />
      <circle className="sdg-dot" cx={500} cy={88} r={5} />
      <circle className="sdg-dot" cx={580} cy={88} r={5} />
      <circle className="sdg-dot" cx={660} cy={88} r={5} />
      <text className="sdg-sub" x={540} y={148} textAnchor="middle">
        repeat
      </text>
      <text className="sdg-sub" x={540} y={178} textAnchor="middle">
        no peer to disagree with, no handoff to lose
      </text>

      <text className="sdg-sub" x="360" y="214" textAnchor="middle">
        the swarm failure modes are not mitigated on the right — they are unrepresentable
      </text>
    </svg>
  );
}

/**
 * Event stream: the two rules, as the fork a parser actually walks. The page
 * warns against collapsing them into one try/catch, and a picture is the
 * cheapest way to show that they are two decisions and not one — an unknown
 * `type` leaves early, a known `type` with a bad body must not.
 */
export function EventContractDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 232"
      role="img"
      aria-label="Each line is parsed alone. An unrecognized event type is inert — skip it and keep reading. A recognized type whose body does not fit is a real error and must fail loudly."
    >
      <title>The two rules a conforming client follows</title>
      <Defs />
      <Node x={8} y={100} w={140} h={52} label="one line" sub="parsed on its own" />
      <Wire d="M148 126 H174" />
      <Node x={176} y={100} w={150} h={52} label="type known?" sub="dispatch on the string" />

      <Wire d="M326 114 C346 114 346 44 366 44" />
      <text className="sdg-sub" x={356} y={96}>
        unknown
      </text>
      <Node x={368} y={22} w={208} h={44} label="inert" sub="count it, keep reading" />

      <Wire d="M326 138 C346 138 346 126 366 126" />
      <Node x={368} y={100} w={150} h={52} label="body fits?" sub="against the schema" />

      <Wire d="M518 114 C548 114 548 96 574 96" />
      <text className="sdg-sub" x={528} y={90}>
        yes
      </text>
      <Node x={576} y={74} w={136} h={44} label="handle it" />

      <Wire d="M518 138 C548 138 548 170 574 170" />
      <text className="sdg-sub" x={516} y={188}>
        malformed
      </text>
      <Node x={576} y={148} w={136} h={44} label="fail loudly" sub="this is corruption" accent />

      <text className="sdg-sub" x="360" y="220" textAnchor="middle">
        one try/catch per line collapses these two into the first — a loud failure becomes silent
        data loss
      </text>
    </svg>
  );
}

/**
 * Budget: two ceilings, drawn in the order they fire, because that order is the
 * guide's thesis. The left one costs nothing when it trips, which is why it is
 * the accented node — the reader's instinct is to reach for --budget, and the
 * cheaper stop is the one before it.
 */
export function BudgetGuardDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 220"
      role="img"
      aria-label="A plan meets the scope review first, which stops it before the first edit at zero cost. Steps that get past it are metered between steps and stages, never inside a tool call, so a budget abort never leaves a half-applied edit."
    >
      <title>The two places a run is stopped</title>
      <Defs />
      <Node x={12} y={50} w={110} h={52} label="a plan" sub="nothing written" />
      <Wire d="M122 76 H164" />
      <Node x={166} y={50} w={150} h={52} label="scope review" sub="steps · files · cost" accent />
      <Wire d="M316 76 H378" />
      <Node x={380} y={50} w={140} h={52} label="the steps" sub="where the money goes" />
      <Wire d="M520 76 H564" />
      <Node x={566} y={50} w={138} h={52} label="settled" sub="cost in the receipt" />

      <Wire d="M241 102 V138" />
      <text className="sdg-sub" x={249} y={126}>
        over a threshold
      </text>
      <Node x={153} y={140} w={176} h={44} label="stopped" sub="$0 — nothing edited" />

      <Wire d="M450 102 V138" />
      <text className="sdg-sub" x={458} y={126}>
        over the ceiling
      </text>
      <Node x={350} y={140} w={200} h={44} label="abort" sub="exit 1 — work on disk stays" />

      <text className="sdg-sub" x="360" y="206" textAnchor="middle">
        the meter is read between steps, never inside a tool call — an abort leaves work, not
        wreckage
      </text>
    </svg>
  );
}

/**
 * Cost: five commands, each answering a narrower question than the last. The
 * boxes shrink because that narrowing *is* the guide — the shape says "keep
 * going, the question gets smaller" without a sentence having to.
 */
export function CostChainDiagram() {
  const rungs: [string, string][] = [
    ["stella stats", "which model, and how much"],
    ["stella observe --open", "which run"],
    ["stella inspect", "which call inside it"],
    ["stella inspect 42 --step 3", "what that call was sent"],
    ["… --diff --only system", "what changed since the last one"],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 246"
      role="img"
      aria-label="Five commands, each narrowing the question: stats for which model and how much, observe for which run, inspect for which call, inspect with a step for what it was sent, and diff for what changed since the previous call."
    >
      <title>From a dollar figure down to the exact bytes</title>
      <Defs />
      <Wire d="M28 20 V198" />
      {rungs.map(([cmd, question], i) => {
        const y = 14 + i * 42;
        // Each rung is narrower than the last: the shape carries the narrowing,
        // so no sentence has to say "and now a smaller question".
        const w = 448 - i * 58;
        const last = i === rungs.length - 1;
        return (
          <g key={cmd}>
            <circle className="sdg-dot" cx={28} cy={y + 16} r={8} />
            <text className="sdg-sub sdg-dot-numeral" x={28} y={y + 20} textAnchor="middle">
              {i + 1}
            </text>
            <rect
              className={last ? "sdg-box-accent" : "sdg-box"}
              x={48}
              y={y}
              width={w}
              height={32}
              rx={6}
            />
            <text className="sdg-sub" x={62} y={y + 20}>
              {cmd}
            </text>
            <text className="sdg-sub" x={48 + w + 16} y={y + 20}>
              {question}
            </text>
          </g>
        );
      })}
      <text className="sdg-sub" x="345" y="234" textAnchor="middle">
        every rung reads .stella/private/store.db and never writes it — no network, no API key
      </text>
    </svg>
  );
}

/**
 * Fleets: what makes a shared tree safe. The picture is worth drawing because
 * the outcome people expect — two workers editing one file and sorting it out
 * later — is not what happens. The second dispatch never starts, and it names
 * the worker that beat it.
 */
export function ClaimLockDiagram() {
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 218"
      role="img"
      aria-label="Two tasks declaring the same path meet a shared claim table. The first holds the path for its attempt and runs; the second fails its dispatch by name, in under a second, with the rival identified."
    >
      <title>Claims: one shared tree, one lock table</title>
      <Defs />
      <Node x={16} y={34} w={150} h={48} label="task a" sub="claims error.rs" />
      <Node x={16} y={118} w={150} h={48} label="task b" sub="claims error.rs" />

      <Wire d="M166 58 C208 58 208 88 248 88" />
      <Wire d="M166 142 C208 142 208 112 248 112" />

      <rect className="sdg-box-accent" x={250} y={56} width={200} height={88} rx={6} />
      <Glyph kind="lock" x={340} y={68} accent />
      <text className="sdg-label" x={350} y={110} textAnchor="middle">
        the claim table
      </text>
      <text className="sdg-sub" x={350} y={126} textAnchor="middle">
        one shared tree · exact paths
      </text>

      <Wire d="M450 88 C486 88 486 58 526 58" />
      <Wire d="M450 112 C486 112 486 142 526 142" />
      <Node x={528} y={34} w={176} h={48} label="runs" sub="holds it for the attempt" />
      <Node x={528} y={118} w={176} h={48} label="fails by name" sub="under a second, rival named" />

      <text className="sdg-sub" x="360" y="204" textAnchor="middle">
        the collision surfaces at dispatch, not at integration — and undeclared paths are claimed
        on first write too
      </text>
    </svg>
  );
}

/**
 * The engine's reverse-RPC turn, as a sequence. This replaces an ASCII sketch
 * that said the same thing: the engine never initiates a call, it emits a
 * request and parks. Lifelines are dashed because waiting is what they spend
 * most of a turn doing — the solid horizontals are the only deliveries.
 */
export function EngineSequenceDiagram() {
  const messages: [number, boolean, string][] = [
    [86, true, "POST /v1/turns — prompt, tools, your history"],
    [128, false, "provider_request — it needs a completion"],
    [170, true, "POST …/provider-result — your gateway, your key"],
    [212, false, "tool_request — the model asked for a tool"],
    [254, true, "POST …/tool-result — your sandbox, your RBAC"],
    [296, false, "turn_complete — text, cost, and it is over"],
  ];
  return (
    <svg
      className="sdg"
      viewBox="0 0 720 346"
      role="img"
      aria-label="Your app starts a turn. The engine asks it to run a model call and waits for the result, then asks it to run a tool and waits again, then reports the turn complete. Every outbound call is made by your app; the engine only asks."
    >
      <title>One turn, as a sequence</title>
      <Defs />
      <rect className="sdg-box" x={66} y={12} width={148} height={44} rx={6} />
      <Glyph kind="app" x={96} y={24} />
      <text className="sdg-label" x={126} y={38}>
        your app
      </text>
      <rect className="sdg-box-accent" x={486} y={12} width={148} height={44} rx={6} />
      <Glyph kind="server" x={501} y={24} accent />
      <text className="sdg-label" x={531} y={38}>
        stella-serve
      </text>

      {/* lifelines: dashed, because parked is their default state */}
      <Wire d="M140 56 V310" arrow={false} dashed />
      <Wire d="M560 56 V310" arrow={false} dashed />

      {messages.map(([y, rightward, label]) => (
        <g key={label}>
          <text className="sdg-sub" x={350} y={y - 8} textAnchor="middle">
            {label}
          </text>
          <Wire d={rightward ? `M142 ${y} H558` : `M558 ${y} H142`} />
        </g>
      ))}

      <text className="sdg-sub" x="360" y="334" textAnchor="middle">
        the engine never opens a socket — it emits a request and parks until your app answers
      </text>
    </svg>
  );
}
