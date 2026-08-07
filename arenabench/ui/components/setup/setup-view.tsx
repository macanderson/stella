"use client";

import * as React from "react";
import { api, postJson } from "@/lib/api";
import type {
  Catalog,
  Dataset,
  MatchListRow,
  ParsedMatch,
  Preset,
  Seat,
  Snapshot,
  Task,
} from "@/lib/types";
import { cn, PALETTE } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/input";
import { LabeledCheckbox } from "@/components/ui/checkbox";
import { StatusPill } from "@/components/ui/badge";
import { Step, ErrorBox } from "@/components/setup/step";
import { DatasetGrid } from "@/components/setup/dataset-grid";
import { TaskPicker, type TaskFilters } from "@/components/setup/task-picker";
import { SeatCard, ApiDatalist } from "@/components/setup/seat-card";

interface TasksState {
  loading: boolean;
  error: string | null;
  hint: string | null;
  tasks: Task[];
}

const NO_TASKS: TasksState = { loading: false, error: null, hint: null, tasks: [] };

function defaultSeat(catalog: Catalog, agentSlug: string, index: number, id: string): Seat {
  const spec = catalog.agents.find((a) => a.slug === agentSlug) ?? catalog.agents[0];
  return {
    id,
    name: spec ? spec.title : `Seat ${index + 1}`,
    agent: spec ? spec.slug : "stella",
    color: PALETTE[index % PALETTE.length],
    engine: {
      api: "openrouter",
      model: "",
      reasoning: true,
      effort: "high",
      base_url: "",
      budget_usd: "",
      max_tokens: "",
      roles: {},
    },
    env: "",
  };
}

/** The launch payload — also exactly what "download this match" renders.
 * One source of truth, so a saved .toml can never describe something other
 * than what the Start button would run. */
function matchPayload(args: {
  name: string;
  dataset: string | undefined;
  selected: ReadonlySet<string>;
  seats: Seat[];
  attempts: number;
  concurrency: number;
  setupTimeout: number;
  recordVideo: boolean;
}) {
  return {
    name: args.name.trim() || undefined,
    dataset: args.dataset,
    tasks: [...args.selected],
    contestants: args.seats.map((seat) => {
      const roles: Record<string, Record<string, unknown>> = {};
      for (const [role, cfg] of Object.entries(seat.engine.roles)) {
        const entry: Record<string, unknown> = {};
        if (cfg.model) entry.model = cfg.model;
        if (cfg.effort) entry.effort = cfg.effort;
        if (cfg.reasoning) entry.reasoning = cfg.reasoning;
        if (cfg.max_tokens) entry.max_tokens = Number(cfg.max_tokens);
        if (Object.keys(entry).length) roles[role] = entry;
      }
      return {
        id: seat.id,
        name: seat.name,
        agent: seat.agent,
        color: seat.color,
        engine: { ...seat.engine, roles },
        env: seat.env,
      };
    }),
    attempts: args.attempts || 1,
    concurrency: args.concurrency || 1,
    setup_timeout_multiplier: args.setupTimeout || 1,
    record_video: args.recordVideo,
  };
}

export function SetupView({
  catalog,
  bootError,
  onOpenMatch,
  historyVersion,
}: {
  catalog: Catalog | null;
  bootError: string | null;
  onOpenMatch: (id: string, seed?: Snapshot) => void;
  historyVersion: number;
}) {
  // -- step 0: entry point -------------------------------------------------
  const [startMode, setStartMode] = React.useState<"scratch" | "upload" | "preset">(
    "scratch",
  );
  const [templateErrors, setTemplateErrors] = React.useState<string[] | null>(null);
  const [templateOk, setTemplateOk] = React.useState<React.ReactNode>(null);
  const [presets, setPresets] = React.useState<Preset[]>([]);
  const [presetKey, setPresetKey] = React.useState<string | null>(null);
  const fileRef = React.useRef<HTMLInputElement>(null);

  // -- steps 1-2: benchmark + tasks -----------------------------------------
  const [dataset, setDataset] = React.useState<Dataset | null>(null);
  const [tasksState, setTasksState] = React.useState<TasksState>(NO_TASKS);
  const [selected, setSelected] = React.useState<ReadonlySet<string>>(new Set());
  const [draw, setDraw] = React.useState<ReadonlySet<string> | null>(null);
  const [seedText, setSeedText] = React.useState("");
  const [filters, setFilters] = React.useState<TaskFilters>({
    q: "",
    difficulty: "",
    category: "",
    lightOnly: false,
  });
  const [randomN, setRandomN] = React.useState(10);
  const [randomMaxMem, setRandomMaxMem] = React.useState(0);
  const [forceOpen, setForceOpen] = React.useState(false);

  // -- step 3: contestants ---------------------------------------------------
  const [seats, setSeats] = React.useState<Seat[]>([]);
  const seatCounter = React.useRef(0);

  // -- step 4: launch ---------------------------------------------------------
  const [matchName, setMatchName] = React.useState("");
  const [attempts, setAttempts] = React.useState(1);
  const [concurrency, setConcurrency] = React.useState(1);
  const [setupTimeout, setSetupTimeout] = React.useState(1);
  const [recordVideo, setRecordVideo] = React.useState(false);
  const [launching, setLaunching] = React.useState(false);
  const [launchError, setLaunchError] = React.useState<string | null>(null);

  const [history, setHistory] = React.useState<MatchListRow[]>([]);

  const addSeat = React.useCallback(
    (agentSlug: string) => {
      if (!catalog) return;
      setSeats((prev) => [
        ...prev,
        defaultSeat(catalog, agentSlug, prev.length, `seat-${++seatCounter.current}`),
      ]);
    },
    [catalog],
  );

  // Default seating: the head-to-head this tool exists for.
  const seededRef = React.useRef(false);
  React.useEffect(() => {
    if (!catalog || seededRef.current) return;
    seededRef.current = true;
    addSeat("stella");
    addSeat("claude-code");
  }, [catalog, addSeat]);

  const refreshHistory = React.useCallback(async () => {
    try {
      const { matches } = await api<{ matches: MatchListRow[] }>("/api/matches");
      setHistory(matches);
    } catch {
      /* history is a nicety; never block setup on it */
    }
  }, []);
  React.useEffect(() => {
    refreshHistory();
  }, [refreshHistory, historyVersion]);

  // -- dataset / tasks --------------------------------------------------------

  const pickDataset = React.useCallback(async (next: Dataset) => {
    setDataset(next);
    setSelected(new Set());
    setDraw(null);
    setSeedText("");
    setTasksState({ loading: true, error: null, hint: null, tasks: [] });
    try {
      const payload = await api<{ tasks: Task[]; hint?: string }>(
        `/api/datasets/${encodeURIComponent(next.key)}/tasks`,
      );
      setTasksState({
        loading: false,
        error: null,
        hint: payload.tasks.length ? null : payload.hint || "no tasks found",
        tasks: payload.tasks,
      });
    } catch (error) {
      setTasksState({ loading: false, error: String(error), hint: null, tasks: [] });
    }
  }, []);

  const drawRandom = React.useCallback(async () => {
    if (!dataset) return;
    const count = Math.max(1, randomN || 10);
    try {
      const drawn = await api<{
        names: string[];
        seed: number;
        exclude_heavy?: boolean;
        max_memory_mb?: number;
      }>(
        `/api/datasets/${encodeURIComponent(dataset.key)}/sample` +
          `?count=${count}&exclude_heavy=${filters.lightOnly ? "1" : "0"}&max_memory_mb=${Math.max(0, randomMaxMem)}`,
      );
      setSelected(new Set(drawn.names));
      setDraw(new Set(drawn.names));
      setSeedText(
        `· ${drawn.names.length} drawn, seed ${drawn.seed}` +
          (drawn.exclude_heavy ? ", heavy excluded" : "") +
          (drawn.max_memory_mb ? `, ≤${drawn.max_memory_mb} MB` : ""),
      );
    } catch (error) {
      setSeedText(`· draw failed: ${(error as Error).message}`);
    }
  }, [dataset, randomN, randomMaxMem, filters.lightOnly]);

  // The seed describes a draw. The moment the selection stops being that
  // draw — one checkbox toggled, "select all" pressed — the seed no longer
  // reproduces what is about to run, so it stops being shown.
  const changeSelection = React.useCallback(
    (mutate: (next: Set<string>) => void) => {
      setSelected((prev) => {
        const next = new Set(prev);
        mutate(next);
        if (draw) {
          const intact = draw.size === next.size && [...draw].every((name) => next.has(name));
          if (!intact) {
            setDraw(null);
            setSeedText("");
          }
        }
        return next;
      });
    },
    [draw],
  );

  // -- templates ---------------------------------------------------------------

  /** Push a fully-specified match into every piece of wizard state.
   *
   * The one applier, shared by an uploaded .toml and a built-in preset. Two
   * copies of this would be two definitions of what loading a match means,
   * and the drift would be invisible until a preset and its downloaded
   * template disagreed about what they were describing. */
  const applyMatch = React.useCallback(
    async (match: ParsedMatch) => {
      if (!catalog) return;
      const ds = catalog.datasets.find((d) => d.key === match.dataset);
      if (ds) await pickDataset(ds);
      setSelected(new Set(match.tasks || []));
      setDraw(null);
      setSeedText("");
      setSeats(
        (match.contestants || []).map((c, index) => ({
          id: c.id || `seat-${index + 1}`,
          name: c.name,
          agent: c.agent,
          color: c.color || PALETTE[index % PALETTE.length],
          engine: {
            api: c.engine.api,
            model: c.engine.model,
            reasoning: c.engine.reasoning,
            effort: c.engine.effort,
            base_url: c.engine.base_url || "",
            budget_usd: c.engine.budget_usd == null ? "" : String(c.engine.budget_usd),
            max_tokens: c.engine.max_tokens == null ? "" : String(c.engine.max_tokens),
            roles: Object.fromEntries(
              Object.entries(c.engine.roles || {}).map(([name, cfg]) => [
                name,
                {
                  model: cfg.model || "",
                  effort: cfg.effort || "",
                  reasoning: (cfg.reasoning == null
                    ? ""
                    : cfg.reasoning
                      ? "on"
                      : "off") as "" | "on" | "off",
                  max_tokens: cfg.max_tokens == null ? "" : String(cfg.max_tokens),
                },
              ]),
            ),
          },
          env: "", // never from a file, never from a preset
        })),
      );
      setMatchName(match.name || "");
      setAttempts(match.attempts || 1);
      setConcurrency(match.concurrency || 1);
      setSetupTimeout(match.setup_timeout_multiplier || 1);
      setRecordVideo(!!match.record_video);
      setForceOpen(true);
    },
    [catalog, pickDataset],
  );

  const loadTemplate = React.useCallback(
    async (text: string, filename: string) => {
      setTemplateErrors(null);
      setTemplateOk(null);
      setPresetKey(null);
      if (!catalog) return;
      try {
        const res = await postJson<{
          ok: boolean;
          problems: string[];
          match: ParsedMatch;
          required_env?: Record<string, string[]>;
        }>("/api/templates/parse", { toml: text });
        if (!res.ok) {
          setTemplateErrors([`${filename} has problems:`, ...res.problems.map((p) => `• ${p}`)]);
          return;
        }
        const match = res.match;
        await applyMatch(match);
        setStartMode("upload");
        const needed = Object.values(res.required_env || {}).flat();
        setTemplateOk(
          <>
            <div>
              loaded {filename} — {match.contestants?.length ?? 0} contestants,{" "}
              {match.tasks?.length || "all"} tasks
            </div>
            <div className="mt-1 text-muted">
              {needed.length
                ? `paste credentials for: ${[...new Set(needed)].join(", ")} — templates carry no secrets`
                : "no credentials declared"}
            </div>
          </>,
        );
      } catch (error) {
        setTemplateErrors([String(error)]);
      }
    },
    [catalog, applyMatch],
  );

  // -- presets -----------------------------------------------------------------

  React.useEffect(() => {
    let live = true;
    api<{ presets: Preset[] }>("/api/presets")
      .then((res) => {
        if (live) setPresets(res.presets || []);
      })
      .catch(() => {
        /* presets are a shortcut; the wizard works without them */
      });
    return () => {
      live = false;
    };
  }, []);

  const applyPreset = React.useCallback(
    async (preset: Preset) => {
      setTemplateErrors(null);
      setTemplateOk(null);
      try {
        await applyMatch(preset.match);
        setStartMode("preset");
        setPresetKey(preset.key);
        const needed = [...new Set(Object.values(preset.required_env || {}).flat())];
        setTemplateOk(
          <>
            <div>
              loaded “{preset.title}” — {preset.match.contestants?.length ?? 0} seats,{" "}
              {preset.match.tasks?.length ?? 0} tasks
            </div>
            <div className="mt-1 text-muted">
              {needed.includes("CLAUDE_CODE_OAUTH_TOKEN")
                ? "the Claude Code seat authenticates from this machine’s own Claude Code login, read fresh at launch. "
                : ""}
              {needed.length
                ? `credentials needed: ${needed.join(", ")}`
                : "no credentials declared"}
            </div>
          </>,
        );
      } catch (error) {
        setTemplateErrors([String(error)]);
      }
    },
    [applyMatch],
  );

  const downloadTemplate = React.useCallback(async () => {
    setTemplateErrors(null);
    try {
      const { toml } = await postJson<{ toml: string }>(
        "/api/templates/render",
        matchPayload({
          name: matchName,
          dataset: dataset?.key,
          selected,
          seats,
          attempts,
          concurrency,
          setupTimeout,
          recordVideo,
        }),
      );
      const blob = new Blob([toml], { type: "application/toml" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download =
        (matchName.trim() || "match")
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-|-$/g, "") + ".toml";
      document.body.append(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    } catch (error) {
      setTemplateErrors([String(error)]);
    }
  }, [matchName, dataset, selected, seats, attempts, concurrency, setupTimeout, recordVideo]);

  // -- launch --------------------------------------------------------------------

  const launch = React.useCallback(async () => {
    setLaunchError(null);
    const payload = matchPayload({
      name: matchName,
      dataset: dataset?.key,
      selected,
      seats,
      attempts,
      concurrency,
      setupTimeout,
      recordVideo,
    });
    if (!payload.tasks.length) {
      setLaunchError("Select at least one task.");
      return;
    }
    setLaunching(true);
    try {
      const snapshot = await postJson<Snapshot>("/api/matches", payload);
      onOpenMatch(snapshot.match.id, snapshot);
    } catch (error) {
      setLaunchError(String(error));
    } finally {
      setLaunching(false);
    }
  }, [
    matchName,
    dataset,
    selected,
    seats,
    attempts,
    concurrency,
    setupTimeout,
    recordVideo,
    onOpenMatch,
  ]);

  const trials = selected.size * seats.length;
  const showTasks = dataset !== null || forceOpen;
  const showRest = tasksState.tasks.length > 0 || forceOpen;

  const startCard = (on: boolean) =>
    cn(
      "flex cursor-pointer flex-col gap-[3px] rounded-[10px] border p-[13px_15px] text-left transition-colors",
      on
        ? "border-gold bg-[linear-gradient(180deg,rgb(255_176_0/0.07),transparent_70%)]"
        : "border-line bg-panel hover:border-dim hover:bg-panel-2",
    );

  return (
    <div className="mx-auto max-w-[1180px] px-5 pb-24 pt-8">
      <ApiDatalist />

      <Step
        n="0"
        title="new run"
        hint={
          <>
            Start from a template or build from scratch. A template is an{" "}
            <code>arenabench.toml</code> — commit it, and <code>arenabench run it.toml</code>{" "}
            reproduces this match exactly. Templates never contain secrets.
          </>
        }
      >
        <div className="grid gap-2 [grid-template-columns:repeat(auto-fit,minmax(210px,1fr))]">
          <button
            type="button"
            className={startCard(startMode === "scratch")}
            onClick={() => {
              setStartMode("scratch");
              setTemplateOk(null);
              setTemplateErrors(null);
            }}
          >
            <span className="text-[13px] font-bold lowercase">set up from scratch</span>
            <span className="text-[11px] text-muted">walk the wizard below</span>
          </button>
          <button
            type="button"
            className={startCard(startMode === "upload")}
            onClick={() => fileRef.current?.click()}
          >
            <span className="text-[13px] font-bold lowercase">load a template</span>
            <span className="text-[11px] text-muted">upload an arenabench.toml</span>
            <input
              ref={fileRef}
              type="file"
              accept=".toml,text/plain"
              hidden
              onChange={async (event) => {
                const file = event.target.files?.[0];
                if (!file) return;
                await loadTemplate(await file.text(), file.name);
                event.target.value = ""; // let the same file be re-picked
              }}
            />
          </button>
          <button type="button" className={startCard(false)} onClick={downloadTemplate}>
            <span className="text-[13px] font-bold lowercase">download this match</span>
            <span className="text-[11px] text-muted">save the current setup as .toml</span>
          </button>
        </div>
        {presets.length > 0 && (
          <div className="mt-3">
            <div className="mb-2 text-[11px] lowercase text-muted">
              or one click: stella vs claude code, same model and effort on both
              arms, stella verifying with a stronger independent judge
            </div>
            <div className="grid gap-2 [grid-template-columns:repeat(auto-fit,minmax(210px,1fr))]">
              {presets.map((preset) => (
                <button
                  key={preset.key}
                  type="button"
                  className={startCard(startMode === "preset" && presetKey === preset.key)}
                  onClick={() => applyPreset(preset)}
                >
                  <span className="text-[13px] font-bold lowercase">{preset.title}</span>
                  <span className="text-[11px] text-muted">{preset.blurb}</span>
                </button>
              ))}
            </div>
          </div>
        )}
        {templateErrors && (
          <ErrorBox>
            {templateErrors.map((line, i) => (
              <div key={i}>{line}</div>
            ))}
          </ErrorBox>
        )}
        {templateOk && (
          <div className="mt-3 rounded-[10px] border border-ok/35 bg-ok/6 px-[13px] py-2.5 text-[12.5px] text-ok">
            {templateOk}
          </div>
        )}
      </Step>

      <Step
        n="1"
        title="benchmark"
        hint="Datasets are pinned by digest — a name alone is not a freeze."
      >
        {bootError ? (
          <ErrorBox>{bootError}</ErrorBox>
        ) : catalog === null ? (
          <div className="p-5 text-[13px] text-dim">loading datasets…</div>
        ) : (
          <DatasetGrid
            datasets={catalog.datasets}
            selectedKey={dataset?.key ?? null}
            onPick={pickDataset}
          />
        )}
      </Step>

      {showTasks && (
        <Step
          n="2"
          title="tasks"
          hint={
            <>
              Every contestant runs the identical list. <strong>{selected.size}</strong>{" "}
              selected. <span className="font-mono text-dim">{seedText}</span>
            </>
          }
        >
          {tasksState.loading ? (
            <div className="p-5 text-[13px] text-dim">loading tasks…</div>
          ) : tasksState.error ? (
            <ErrorBox>{tasksState.error}</ErrorBox>
          ) : tasksState.hint ? (
            <ErrorBox>{tasksState.hint}</ErrorBox>
          ) : (
            <TaskPicker
              tasks={tasksState.tasks}
              selected={selected}
              filters={filters}
              seedText=""
              randomN={randomN}
              randomMaxMem={randomMaxMem}
              onFilters={setFilters}
              onRandomN={setRandomN}
              onRandomMaxMem={setRandomMaxMem}
              onToggle={(name, on) =>
                changeSelection((next) => (on ? next.add(name) : next.delete(name)))
              }
              onSelectAll={() =>
                changeSelection((next) => tasksState.tasks.forEach((t) => next.add(t.name)))
              }
              onSelectNone={() => changeSelection((next) => next.clear())}
              onSelectVisible={(names) =>
                changeSelection((next) => names.forEach((name) => next.add(name)))
              }
              onDrawRandom={drawRandom}
            />
          )}
        </Step>
      )}

      {showRest && catalog && (
        <>
          <Step
            n="3"
            title="contestants"
            hint={
              <>
                A contestant is an agent plus a full engine configuration plus its own
                environment. Paste that seat&apos;s <code>.env</code> — values stay in this
                process and are never sent back to the browser.
              </>
            }
          >
            <div className="grid gap-3.5">
              {seats.map((seat, index) => (
                <SeatCard
                  key={seat.id}
                  seat={seat}
                  index={index}
                  agents={catalog.agents}
                  efforts={catalog.efforts}
                  roles={catalog.roles}
                  removable={seats.length > 1}
                  onChange={(mutate) =>
                    setSeats((prev) => prev.map((s, i) => (i === index ? mutate(s) : s)))
                  }
                  onAgentChange={(slug) => {
                    const spec = catalog.agents.find((a) => a.slug === slug);
                    setSeats((prev) =>
                      prev.map((s, i) =>
                        i === index ? { ...s, agent: slug, name: spec?.title ?? s.name } : s,
                      ),
                    );
                  }}
                  onRemove={() => setSeats((prev) => prev.filter((_, i) => i !== index))}
                />
              ))}
            </div>
            <Button variant="dashed" className="mt-3.5 w-full" onClick={() => addSeat("stella")}>
              + add contestant
            </Button>
          </Step>

          <Step n="4" title="launch">
            <div className="mb-[18px] flex flex-wrap items-end gap-4">
              <Field label="Match name" className="min-w-[170px] flex-1">
                <Input
                  type="text"
                  placeholder="friday head-to-head"
                  value={matchName}
                  onChange={(e) => setMatchName(e.target.value)}
                />
              </Field>
              <Field label="Attempts / task" className="w-[110px]">
                <Input
                  type="number"
                  min={1}
                  max={10}
                  value={attempts}
                  onChange={(e) => setAttempts(Number(e.target.value) || 1)}
                />
              </Field>
              <Field label="Concurrency" className="w-[110px]">
                <Input
                  type="number"
                  min={1}
                  max={16}
                  value={concurrency}
                  onChange={(e) => setConcurrency(Number(e.target.value) || 1)}
                />
              </Field>
              <Field label="Setup timeout ×" className="w-[110px]">
                <Input
                  type="number"
                  min={1}
                  max={20}
                  step={0.5}
                  value={setupTimeout}
                  onChange={(e) => setSetupTimeout(Number(e.target.value) || 1)}
                  title="Scales the budget for installing each agent's toolchain into its container, before the task starts. Raise this on a slow or contended host so a slow installer does not score as a coding failure."
                />
              </Field>
              <LabeledCheckbox checked={recordVideo} onCheckedChange={setRecordVideo} className="ml-auto">
                Record MP4 <span className="text-muted">real screen capture, sidecar container</span>
              </LabeledCheckbox>
            </div>
            <div className="flex items-center gap-4">
              <Button variant="primary" size="lg" disabled={launching} onClick={launch}>
                {launching ? "starting…" : "▶ Start the match"}
              </Button>
              <span className="text-muted">
                {selected.size} task{selected.size === 1 ? "" : "s"} × {seats.length} contestant
                {seats.length === 1 ? "" : "s"} = {trials} trials
              </span>
            </div>
            {launchError && <ErrorBox>{launchError}</ErrorBox>}
          </Step>
        </>
      )}

      <Step title="recent matches">
        {history.length === 0 ? (
          <span className="text-muted">none yet</span>
        ) : (
          <div className="grid gap-1.5">
            {history.map((m) => (
              <button
                key={m.id}
                type="button"
                onClick={() => onOpenMatch(m.id)}
                className="flex cursor-pointer items-center gap-3 rounded-lg border border-line bg-panel px-[13px] py-[9px] text-left text-[13px] hover:border-dim"
              >
                <StatusPill status={m.status} />
                <strong>{m.name}</strong>
                <span className="font-mono text-muted">
                  {m.tasks} tasks · {m.contestants.join(" vs ")}
                </span>
              </button>
            ))}
          </div>
        )}
      </Step>
    </div>
  );
}
