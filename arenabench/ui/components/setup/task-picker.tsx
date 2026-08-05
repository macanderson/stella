"use client";

import * as React from "react";
import type { Task } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SimpleSelect } from "@/components/ui/select";
import { LabeledCheckbox } from "@/components/ui/checkbox";

export interface TaskFilters {
  q: string;
  difficulty: string;
  category: string;
  lightOnly: boolean;
}

export function visibleTasks(tasks: Task[], filters: TaskFilters): Task[] {
  const q = filters.q.trim().toLowerCase();
  return tasks.filter((task) => {
    if (filters.difficulty && task.difficulty !== filters.difficulty) return false;
    if (filters.category && task.category !== filters.category) return false;
    if (filters.lightOnly && task.heavy) return false;
    if (q && !(task.name + " " + (task.description || "")).toLowerCase().includes(q)) return false;
    return true;
  });
}

const DIFF_TONE: Record<string, string> = {
  easy: "text-ok",
  medium: "text-acc-citron",
  hard: "text-acc-rose",
};

export function TaskPicker({
  tasks,
  selected,
  filters,
  seedText,
  randomN,
  randomMaxMem,
  onFilters,
  onRandomN,
  onRandomMaxMem,
  onToggle,
  onSelectAll,
  onSelectNone,
  onSelectVisible,
  onDrawRandom,
}: {
  tasks: Task[];
  selected: ReadonlySet<string>;
  filters: TaskFilters;
  seedText: string;
  randomN: number;
  randomMaxMem: number;
  onFilters: (next: TaskFilters) => void;
  onRandomN: (n: number) => void;
  onRandomMaxMem: (mb: number) => void;
  onToggle: (name: string, on: boolean) => void;
  onSelectAll: () => void;
  onSelectNone: () => void;
  onSelectVisible: (names: string[]) => void;
  onDrawRandom: () => void;
}) {
  const shown = visibleTasks(tasks, filters);
  const uniq = (key: "difficulty" | "category") =>
    [...new Set(tasks.map((t) => t[key]).filter((v): v is string => Boolean(v)))].sort();

  return (
    <>
      <div className="mb-3.5 flex flex-wrap items-center gap-[9px]">
        <Input
          type="search"
          placeholder="filter tasks…"
          spellCheck={false}
          value={filters.q}
          onChange={(e) => onFilters({ ...filters, q: e.target.value })}
          className="min-w-[210px]"
        />
        <SimpleSelect
          ariaLabel="difficulty filter"
          className="w-auto min-w-[130px]"
          value={filters.difficulty}
          onValueChange={(difficulty) => onFilters({ ...filters, difficulty })}
          items={[
            { value: "", label: "all difficulty" },
            ...uniq("difficulty").map((v) => ({ value: v, label: v })),
          ]}
        />
        <SimpleSelect
          ariaLabel="category filter"
          className="w-auto min-w-[140px]"
          value={filters.category}
          onValueChange={(category) => onFilters({ ...filters, category })}
          items={[
            { value: "", label: "all categories" },
            ...uniq("category").map((v) => ({ value: v, label: v })),
          ]}
        />
        <LabeledCheckbox
          checked={filters.lightOnly}
          onCheckedChange={(lightOnly) => onFilters({ ...filters, lightOnly })}
        >
          exclude heavy <span className="text-muted">(&gt;4 GB or multi-CPU)</span>
        </LabeledCheckbox>
        <div className="flex-1" />
        <Button variant="ghost" onClick={onSelectAll}>
          select all
        </Button>
        <Button variant="ghost" onClick={onSelectNone}>
          clear
        </Button>
        <Button variant="ghost" onClick={() => onSelectVisible(shown.map((t) => t.name))}>
          select filtered
        </Button>
        <Button variant="ghost" onClick={onDrawRandom}>
          random
        </Button>
        <Input
          type="number"
          min={1}
          max={500}
          value={randomN}
          onChange={(e) => onRandomN(Number(e.target.value) || 1)}
          title="how many tasks to draw at random"
          className="w-[68px]"
        />
        <Input
          type="number"
          min={0}
          step={1024}
          value={randomMaxMem || ""}
          placeholder="≤ MB"
          onChange={(e) => onRandomMaxMem(Number(e.target.value) || 0)}
          title="draw only tasks asking for at most this many MB (0 = no ceiling). With N contestants racing, use your Docker memory allocation divided by N."
          className="w-[76px]"
        />
      </div>

      {seedText && <div className="mb-2 font-mono text-xs text-muted">{seedText}</div>}

      <div className="grid max-h-[460px] gap-1.5 overflow-y-auto rounded-[10px] border border-line bg-panel p-1 [grid-template-columns:repeat(auto-fill,minmax(268px,1fr))]">
        {shown.map((task) => {
          const on = selected.has(task.name);
          return (
            <label
              key={task.name}
              className={cn(
                "flex cursor-pointer items-start gap-[9px] rounded-[7px] border px-[11px] py-[9px] transition-colors",
                on ? "border-gold/30 bg-gold/8" : "border-transparent hover:bg-panel-2",
              )}
            >
              <input
                type="checkbox"
                checked={on}
                onChange={(e) => onToggle(task.name, e.target.checked)}
                className="mt-[3px] accent-(--accent)"
              />
              <span className="min-w-0 flex-1">
                <span
                  className="block truncate font-mono text-[12.5px]"
                  title={task.description}
                >
                  {task.name}
                </span>
                <span className="mt-[3px] flex gap-[7px] text-[10.5px] text-dim">
                  {task.difficulty && (
                    <span className={DIFF_TONE[task.difficulty] ?? ""}>{task.difficulty}</span>
                  )}
                  {task.category && <span>{task.category}</span>}
                  {task.heavy && <span className="text-acc-violet">heavy</span>}
                </span>
              </span>
            </label>
          );
        })}
      </div>
    </>
  );
}
