// Shapes of what the Python server actually sends. Optional-heavy on
// purpose: the server is the authority and the client should render what
// arrives rather than crash on what didn't.

export interface Dataset {
  key: string;
  title: string;
  description: string;
  harbor_id?: string;
  digest?: string | null;
  task_count?: number | null;
}

export interface Task {
  name: string;
  description?: string;
  difficulty?: string;
  category?: string;
  heavy?: boolean;
  memory_mb?: number | null;
}

export interface AgentInfo {
  slug: string;
  title: string;
  honours: string[];
  has_pipeline?: boolean;
}

export interface Catalog {
  datasets: Dataset[];
  agents: AgentInfo[];
  efforts: string[];
  roles: string[];
}

/** One selectable model from /api/models (#2065). `benchmarked` mirrors the
 *  adapter's _BENCHMARKED_SLUGS — measured ceilings, not inherited ones. */
export interface CatalogModel {
  slug: string;
  benchmarked: boolean;
  output_cap: number | null;
}

/** /api/models: Stella's own catalog grouped by provider — the models that
 *  are actually reachable inside a benchmark container. */
export interface ModelsPayload {
  providers: Record<string, CatalogModel[]>;
}

/** Per-role overrides as the form holds them: strings, '' meaning inherit. */
export interface RoleDraft {
  model: string;
  effort: string;
  reasoning: "" | "on" | "off";
  max_tokens: string;
}

export interface EngineDraft {
  api: string;
  model: string;
  reasoning: boolean;
  effort: string;
  base_url: string;
  budget_usd: string;
  max_tokens: string;
  roles: Record<string, RoleDraft>;
}

export interface Seat {
  id: string;
  name: string;
  agent: string;
  color: string;
  engine: EngineDraft;
  env: string;
}

export interface MatchListRow {
  id: string;
  name: string;
  status: string;
  tasks: number;
  contestants: string[];
}

export interface Totals {
  trials: number;
  judged: number;
  running: number;
  passed: number;
  solve_rate: number;
  clock_time: number;
  /** What the agents said they spent, each on its own table. Not comparable. */
  total_cost: number;
  /** Every seat's tokens through one price table. `null` = model unpriced. */
  priced_cost: number | null;
  tokens_in: number;
  tokens_out: number;
  cache_read: number;
  cache_write: number;
  infrastructure?: number;
  /** Seconds still running after the solution already passed, summed over the
   *  replayed trials. `null` = no trial was replayed — unmeasured, not zero. */
  wasted_time?: number | null;
  /** How many trials the wasted-time sum covers (`arenabench flip` runs). */
  flip_trials?: number;
  [key: string]: number | null | undefined;
}

export interface ContestantSnap {
  id: string;
  name: string;
  agent: string;
  color: string;
  engine_label: string;
  state: string;
  totals: Totals;
  warnings?: string[];
  notes?: string[];
  error?: string | null;
}

export interface Dimension {
  key: string;
  label: string;
  blurb?: string;
  direction: "higher" | "lower" | "neutral";
}

export interface Cell {
  status: string;
  resolved: boolean | null;
  failure?: string | null;
  age_s?: number | null;
  steps: number;
  tools: number;
  tokens_in: number;
  tokens_out: number;
  cache_read: number;
  cache_write: number;
  total_cost: number;
  priced_cost: number | null;
  clock_time: number;
  cap_hits?: number;
  has_video?: boolean;
  /** When the verifier first passed, seconds from the first snapshot.
   *  Only `arenabench flip` can know this; `null`/absent = never replayed. */
  flip_elapsed?: number | null;
  /** Seconds the trial kept running after it was already passing. */
  wasted_elapsed?: number | null;
}

export interface TaskRow {
  task: string;
  cells: Record<string, Cell | null | undefined>;
}

/**
 * One monitor rule firing once, for one arm on one task — the agent monitor
 * protocol's severity semantics: `critical` means the match's numbers are
 * invalid and must not be published; the UI says so rather than tinting a
 * cell.
 */
export interface Detection {
  contestant: string;
  task: string;
  rule: string;
  severity: string;
  evidence: string;
  data?: Record<string, unknown>;
}

export interface Snapshot {
  match: {
    id: string;
    name: string;
    contestants: unknown[];
    record_video?: boolean;
  };
  dataset: { title: string };
  status: string;
  elapsed: number;
  note?: string | null;
  recording_active?: number;
  rows: TaskRow[];
  contestants: ContestantSnap[];
  dimensions: Dimension[];
  leaders?: Record<string, string[]>;
  detections?: Detection[];
}

export interface TranscriptEntry {
  seq: number;
  t: number;
  kind: string;
  title?: string;
  body?: string;
  meta?: Record<string, unknown>;
}

/** A branch the SUT can be built from, as /api/sut/branches returns it. */
export interface SutBranch {
  name: string;
  ref: string;
  commit: string;
  short: string;
  subject?: string;
  committed_at?: number;
  is_default?: boolean;
}

/** How far a staged binary is from the commit a match asked for. */
export interface SutDrift {
  staged: string;
  target: string;
  behind: number;
  ahead: number;
  comparable: boolean;
  identical: boolean;
  summary: string;
}

export interface SutBinary {
  path: string;
  commit: string;
  short: string;
  sha256: string;
  built_at: number;
  known: boolean;
}

/** Which Stella a match pinned to `ref` would run — /api/sut. */
export interface SutStatus {
  ref: string;
  repo: string | null;
  target: string | null;
  staged: SutBinary | null;
  legacy: SutBinary | null;
  drift: SutDrift | null;
  /** True only when a binary built from exactly `target` is staged. */
  ready: boolean;
  problem: string | null;
}

/** One SUT build, from queued to done — /api/sut/build[s]. */
export interface SutBuild {
  id: string;
  ref: string;
  commit: string;
  short: string;
  status: "queued" | "building" | "done" | "failed";
  elapsed: number;
  binary: string;
  sha256: string;
  error: string;
  log_tail: string[];
  cached: boolean;
  done: boolean;
}

/** One quick-start head-to-head as /api/presets returns it: a fully
 * configured match in the same shape a parsed template arrives in, so the
 * wizard applies both through one code path. */
export interface Preset {
  key: string;
  title: string;
  blurb: string;
  match: ParsedMatch;
  required_env?: Record<string, string[]>;
}

/** A parsed arenabench.toml as /api/templates/parse returns it. */
export interface ParsedMatch {
  name?: string;
  dataset?: string;
  tasks?: string[];
  attempts?: number;
  concurrency?: number;
  setup_timeout_multiplier?: number;
  record_video?: boolean;
  contestants?: Array<{
    id?: string;
    name: string;
    agent: string;
    color?: string;
    engine: {
      api: string;
      model: string;
      reasoning: boolean;
      effort: string;
      base_url?: string;
      budget_usd?: number | string | null;
      max_tokens?: number | string | null;
      roles?: Record<
        string,
        {
          model?: string;
          effort?: string;
          reasoning?: boolean | null;
          max_tokens?: number | string | null;
        }
      >;
    };
  }>;
}
