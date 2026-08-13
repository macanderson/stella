"use client";

import * as React from "react";
import type {
  AgentInfo,
  ModelsPayload,
  ResponsibilityDraft,
  RoleDraft,
  Seat,
} from "@/lib/types";
import { envStatus, CREDENTIALS } from "@/lib/env";
import { cn, seatStyle } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/input";
import { SimpleSelect } from "@/components/ui/select";
import { Disclosure } from "@/components/ui/collapsible";

const EMPTY_ROLE: RoleDraft = { model: "", effort: "", reasoning: "" };
const EMPTY_RESPONSIBILITY: ResponsibilityDraft = { enabled: "", agent: "" };

/** Inherit, on, off — the three states every pipeline override has.
 *  `""` is inherit and is not `"off"`: conflating them turns an untouched
 *  stage into an ablated one, which is a different experiment. */
const INHERIT_ON_OFF = [
  { value: "", label: "inherit" },
  { value: "on", label: "on" },
  { value: "off", label: "off" },
];

export function SeatCard({
  seat,
  index,
  agents,
  efforts,
  roles,
  responsibilities,
  responsibilityAgents,
  models,
  modelsError,
  removable,
  onChange,
  onAgentChange,
  onRemove,
}: {
  seat: Seat;
  index: number;
  agents: AgentInfo[];
  efforts: string[];
  roles: string[];
  responsibilities: string[];
  responsibilityAgents: string[];
  models: ModelsPayload | null;
  modelsError: string | null;
  removable: boolean;
  onChange: (mutate: (seat: Seat) => Seat) => void;
  onAgentChange: (slug: string) => void;
  onRemove: () => void;
}) {
  const spec = agents.find((a) => a.slug === seat.agent);
  const honours = new Set(spec?.honours ?? []);
  const env = envStatus(seat.env, seat.engine.api);

  // The role-model options (#2065): Stella's catalog, filtered to the
  // WORKER's provider — a trial receives exactly one credential, resolved
  // from the worker's provider, so a role on any other provider always
  // fails at runtime, and offering it would be worse than free text. When
  // the list cannot be trusted, `roleModelFallback` names which case it is
  // and the free-text input stays.
  const providerModels = models?.providers?.[(seat.engine.api || "").trim().toLowerCase()];
  const roleModelFallback = modelsError
    ? `catalog unavailable — ${modelsError}`
    : models && (!providerModels || providerModels.length === 0)
      ? `no catalog models for provider "${seat.engine.api}"`
      : env.tone !== "ok"
        ? "no credential for this seat yet"
        : null;
  const roleModelItems =
    providerModels && roleModelFallback === null
      ? [
          { value: "", label: "inherit" },
          ...providerModels.map((m) => ({
            value: m.slug,
            label:
              m.slug +
              (m.benchmarked ? "" : " · unbenchmarked") +
              (m.output_cap === null ? " · no declared cap" : ""),
          })),
        ]
      : null;

  const setEngine = (patch: Partial<Seat["engine"]>) =>
    onChange((s) => ({ ...s, engine: { ...s.engine, ...patch } }));
  const setRole = (role: string, patch: Partial<RoleDraft>) =>
    onChange((s) => ({
      ...s,
      engine: {
        ...s.engine,
        roles: { ...s.engine.roles, [role]: { ...(s.engine.roles[role] ?? EMPTY_ROLE), ...patch } },
      },
    }));
  const setResponsibility = (name: string, patch: Partial<ResponsibilityDraft>) =>
    onChange((s) => ({
      ...s,
      engine: {
        ...s.engine,
        responsibilities: {
          ...s.engine.responsibilities,
          [name]: { ...(s.engine.responsibilities[name] ?? EMPTY_RESPONSIBILITY), ...patch },
        },
      },
    }));

  // A bare-loop arm has no staged pipeline, so every override below it is
  // inert — the runner never consults `roles` once the loop is settled. The
  // controls stay visible and disabled rather than hidden: a template that
  // loaded with both set is exactly the incoherence an operator needs to see,
  // and hiding it would present a coherent form for an arm that is not.
  const pipelineInert = seat.engine.bare_loop;

  return (
    <div
      className=" border border-line border-l-[3px] bg-panel p-3 [border-left-color:var(--seat)]"
      style={seatStyle(seat.color)}
    >
      <div className="mb-3.5 flex items-center gap-2.5">
        <div className="grid size-[26px] place-items-center bg-(--seat) font-mono text-xs font-bold text-on-seat">
          {index + 1}
        </div>
        <input
          type="text"
          value={seat.name}
          aria-label="contestant name"
          onChange={(e) => onChange((s) => ({ ...s, name: e.target.value }))}
          className="min-w-0 flex-1 border-b border-dashed border-line bg-transparent py-1 font-semibold outline-none focus:border-accent"
        />
        <SimpleSelect
          ariaLabel="agent"
          className="w-auto min-w-[150px]"
          value={seat.agent}
          onValueChange={onAgentChange}
          items={agents.map((a) => ({ value: a.slug, label: a.title }))}
        />
        {removable && (
          <Button variant="danger" size="sm" onClick={onRemove}>
            remove
          </Button>
        )}
      </div>

      <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
        <Field label="API / provider">
          <Input
            type="text"
            list="arena-apis"
            value={seat.engine.api}
            onChange={(e) => setEngine({ api: e.target.value })}
          />
        </Field>
        <Field label="Model">
          <Input
            type="text"
            placeholder="z-ai/glm-5.2"
            value={seat.engine.model}
            onChange={(e) => setEngine({ model: e.target.value })}
          />
        </Field>
        <Field label="Reasoning" className="max-w-[130px]">
          <SimpleSelect
            ariaLabel="reasoning"
            disabled={!honours.has("reasoning")}
            value={seat.engine.reasoning ? "on" : "off"}
            onValueChange={(v) => setEngine({ reasoning: v === "on" })}
            items={[
              { value: "on", label: "on" },
              { value: "off", label: "off" },
            ]}
          />
        </Field>
        <Field label="Effort" className="max-w-[130px]">
          <SimpleSelect
            ariaLabel="effort"
            disabled={!honours.has("effort")}
            value={seat.engine.effort}
            onValueChange={(effort) => setEngine({ effort })}
            items={efforts.map((e) => ({ value: e, label: e }))}
          />
        </Field>
      </div>

      <Disclosure
        className="mt-3.5"
        summary={
          "advanced — base URL" +
          (spec?.has_pipeline ? ", loop mode, pipeline roles, stage roster" : "")
        }
      >
        <div className="mt-2.5 grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
          <Field label="Base URL">
            <Input
              type="text"
              placeholder="provider default"
              value={seat.engine.base_url}
              onChange={(e) => setEngine({ base_url: e.target.value })}
            />
          </Field>
          {/* No budget and no output cap, at engine or role level. A ceiling only
              one seat carries stops that agent where the work finishes, and the
              scoreboard then reports our limit as its capability — measured on
              match 5292a68cdabf, where all three of the capped seat's losses
              were the guard firing at a third of the allowed clock (#2411). The
              server refuses the keys by name; offering them here made every
              launch from this form fail. Bound spend at the provider key. */}
          {spec?.has_pipeline && (
            <Field label="Loop" className="max-w-[190px]">
              <SimpleSelect
                ariaLabel="loop mode"
                value={seat.engine.bare_loop ? "bare" : "pipeline"}
                onValueChange={(v) => setEngine({ bare_loop: v === "bare" })}
                items={[
                  { value: "pipeline", label: "staged pipeline" },
                  { value: "bare", label: "bare loop (--no-pipeline)" },
                ]}
              />
            </Field>
          )}
          {/* The tools this seat is offered. Free text and comma-separated
              rather than a picker: the list is the independent variable of a
              tool-effectiveness run, and an operator adding one tool per arm
              needs to read the whole set at a glance and diff it against the
              last arm. Blank is the shipping catalog — the reading every
              match before this field ran. */}
          <Field label="Tools" className="grow basis-full">
            <Input
              type="text"
              placeholder="all registered tools"
              value={seat.engine.tool_set.join(", ")}
              onChange={(e) =>
                setEngine({
                  tool_set: e.target.value
                    .split(",")
                    .map((name) => name.trim())
                    .filter(Boolean),
                })
              }
            />
          </Field>
        </div>

        {spec?.has_pipeline && (
          <div className="mt-2.5 grid gap-2">
            <div className="mt-1.5 text-[11.5px] text-muted">
              Per-role overrides. Blank inherits the engine baseline above.
              {roleModelFallback && (
                <span className="text-warn"> Model list unavailable ({roleModelFallback}) — free text.</span>
              )}
              {pipelineInert && (
                <span className="text-warn">
                  {" "}
                  Inert — this seat runs the bare loop, so no staged role is consulted.
                </span>
              )}
            </div>
            {roles.map((role) => {
              const current = seat.engine.roles[role] ?? EMPTY_ROLE;
              // A template may carry a model the catalog does not list;
              // keep it selectable (and say so) rather than silently
              // showing "inherit" for a role that actually pins something.
              const items =
                roleModelItems &&
                (current.model === "" ||
                roleModelItems.some((item) => item.value === current.model)
                  ? roleModelItems
                  : [
                      ...roleModelItems,
                      { value: current.model, label: `${current.model} · not in catalog` },
                    ]);
              return (
                <div
                  key={role}
                  className="grid items-center gap-2 [grid-template-columns:74px_1fr_108px_108px] max-lg:[grid-template-columns:1fr_1fr]"
                >
                  <span className="font-mono text-[11.5px] text-muted">{role}</span>
                  {items ? (
                    <SimpleSelect
                      ariaLabel={`${role} model`}
                      size="sm"
                      disabled={pipelineInert}
                      value={current.model}
                      onValueChange={(model) => setRole(role, { model })}
                      items={items}
                    />
                  ) : (
                    <Input
                      type="text"
                      placeholder="inherit model"
                      title={roleModelFallback ?? undefined}
                      disabled={pipelineInert}
                      value={current.model}
                      onChange={(e) => setRole(role, { model: e.target.value })}
                      className="px-2 py-[5px] text-xs"
                    />
                  )}
                  <SimpleSelect
                    ariaLabel={`${role} effort`}
                    size="sm"
                    disabled={pipelineInert}
                    value={current.effort}
                    onValueChange={(effort) => setRole(role, { effort })}
                    items={[
                      { value: "", label: "inherit" },
                      ...efforts.map((e) => ({ value: e, label: e })),
                    ]}
                  />
                  <SimpleSelect
                    ariaLabel={`${role} reasoning`}
                    size="sm"
                    disabled={pipelineInert}
                    value={current.reasoning}
                    onValueChange={(v) => setRole(role, { reasoning: v as RoleDraft["reasoning"] })}
                    items={INHERIT_ON_OFF}
                  />
                  {/* No per-role output cap. Omitting `max_tokens` is what asks
                      for the model's own ceiling; sending a number would mean
                      knowing every booked model's real limit, and guessing one
                      is what #2128 was. */}
                </div>
              );
            })}

            {/* The stage roster (#2381): the narrow instrument next to the
                bare loop's blunt one. Turning off exactly triage, or handing
                the verdict to a different agent, is the only way a measured
                difference can be attributed to one stage. */}
            <div className="mt-3.5 text-[11.5px] text-muted">
              Stage roster — ablate or reassign one responsibility.{" "}
              <strong>inherit</strong> leaves the shipped binding alone; it is not{" "}
              <strong>off</strong>.
              {pipelineInert && (
                <span className="text-warn">
                  {" "}
                  Inert under the bare loop, which settles every stage at once.
                </span>
              )}
            </div>
            {responsibilities.map((name) => {
              const current = seat.engine.responsibilities[name] ?? EMPTY_RESPONSIBILITY;
              return (
                <div
                  key={name}
                  className="grid items-center gap-2 [grid-template-columns:150px_108px_1fr] max-lg:[grid-template-columns:1fr_1fr]"
                >
                  <span className="font-mono text-[11.5px] text-muted">{name}</span>
                  <SimpleSelect
                    ariaLabel={`${name} enabled`}
                    size="sm"
                    disabled={pipelineInert}
                    value={current.enabled}
                    onValueChange={(v) =>
                      setResponsibility(name, {
                        enabled: v as ResponsibilityDraft["enabled"],
                      })
                    }
                    items={INHERIT_ON_OFF}
                  />
                  <SimpleSelect
                    ariaLabel={`${name} agent`}
                    size="sm"
                    disabled={pipelineInert}
                    value={current.agent}
                    onValueChange={(agent) => setResponsibility(name, { agent })}
                    items={[
                      { value: "", label: "default agent" },
                      ...responsibilityAgents.map((a) => ({
                        value: a,
                        label: `run by ${a}`,
                      })),
                    ]}
                  />
                </div>
              );
            })}
          </div>
        )}
      </Disclosure>

      <div className="mt-3">
        <Field label=".env for this seat">
          <Textarea
            spellCheck={false}
            placeholder={"OPENROUTER_API_KEY=sk-or-...\n# comments and `export` are fine"}
            value={seat.env}
            onChange={(e) => onChange((s) => ({ ...s, env: e.target.value }))}
            className="min-h-[92px]"
          />
        </Field>
        <div
          className={cn(
            "mt-1.5 font-mono text-[11.5px]",
            env.tone === "ok" ? "text-ok" : "text-warn",
          )}
        >
          {env.text}
        </div>
      </div>
    </div>
  );
}

/** Provider names the API field autocompletes to — the ones with a known key. */
export function ApiDatalist() {
  return (
    <datalist id="arena-apis">
      {Object.keys(CREDENTIALS).map((name) => (
        <option key={name} value={name} />
      ))}
    </datalist>
  );
}
