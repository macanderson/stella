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
          stroke="var(--stella-seam)"
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
