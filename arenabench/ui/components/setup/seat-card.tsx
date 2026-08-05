"use client";

import * as React from "react";
import type { AgentInfo, RoleDraft, Seat } from "@/lib/types";
import { envStatus, CREDENTIALS } from "@/lib/env";
import { cn, seatStyle } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/input";
import { SimpleSelect } from "@/components/ui/select";
import { Disclosure } from "@/components/ui/collapsible";

const EMPTY_ROLE: RoleDraft = { model: "", effort: "", reasoning: "", max_tokens: "" };

export function SeatCard({
  seat,
  index,
  agents,
  efforts,
  roles,
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
  removable: boolean;
  onChange: (mutate: (seat: Seat) => Seat) => void;
  onAgentChange: (slug: string) => void;
  onRemove: () => void;
}) {
  const spec = agents.find((a) => a.slug === seat.agent);
  const honours = new Set(spec?.honours ?? []);
  const env = envStatus(seat.env, seat.engine.api);

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

  return (
    <div
      className="rounded-[10px] border border-line border-l-[3px] bg-panel p-3 [border-left-color:var(--seat)]"
      style={seatStyle(seat.color)}
    >
      <div className="mb-3.5 flex items-center gap-2.5">
        <div className="grid size-[26px] place-items-center rounded-[7px] bg-(--seat) font-mono text-xs font-bold text-ink">
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
          "advanced — base URL, budget, output cap" + (spec?.has_pipeline ? ", pipeline roles" : "")
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
          <Field label="Budget $/task" className="max-w-[130px]">
            <Input
              type="number"
              step="0.01"
              placeholder="none"
              value={seat.engine.budget_usd}
              onChange={(e) => setEngine({ budget_usd: e.target.value })}
            />
          </Field>
          <Field label="Max output tok" className="max-w-[130px]">
            <Input
              type="number"
              step="1000"
              placeholder="agent default"
              value={seat.engine.max_tokens}
              onChange={(e) => setEngine({ max_tokens: e.target.value })}
            />
          </Field>
        </div>

        {spec?.has_pipeline && (
          <div className="mt-2.5 grid gap-2">
            <div className="mt-1.5 text-[11.5px] text-muted">
              Per-role overrides. Blank inherits the engine baseline above.
            </div>
            {roles.map((role) => {
              const current = seat.engine.roles[role] ?? EMPTY_ROLE;
              return (
                <div
                  key={role}
                  className="grid items-center gap-2 [grid-template-columns:74px_1fr_108px_96px_96px] max-lg:[grid-template-columns:1fr_1fr]"
                >
                  <span className="font-mono text-[11.5px] text-muted">{role}</span>
                  <Input
                    type="text"
                    placeholder="inherit model"
                    value={current.model}
                    onChange={(e) => setRole(role, { model: e.target.value })}
                    className="px-2 py-[5px] text-xs"
                  />
                  <SimpleSelect
                    ariaLabel={`${role} effort`}
                    size="sm"
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
                    value={current.reasoning}
                    onValueChange={(v) => setRole(role, { reasoning: v as RoleDraft["reasoning"] })}
                    items={[
                      { value: "", label: "inherit" },
                      { value: "on", label: "on" },
                      { value: "off", label: "off" },
                    ]}
                  />
                  <Input
                    type="number"
                    placeholder="max tok"
                    value={current.max_tokens}
                    onChange={(e) => setRole(role, { max_tokens: e.target.value })}
                    className="px-2 py-[5px] text-xs"
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
