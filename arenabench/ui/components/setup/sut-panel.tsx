"use client";

import * as React from "react";
import { api, postJson } from "@/lib/api";
import type { SutBranch, SutBuild, SutStatus } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ErrorBox } from "@/components/setup/step";

/**
 * Which Stella the match will run, and how to change it.
 *
 * The panel exists because the answer used to be invisible: the arena ran
 * whatever binary happened to be staged, and on 2026-08-06 that was 291
 * commits behind main and three days old — with every match in between
 * reporting numbers for it silently. So the commit is stated up front, in
 * words, before anything launches.
 *
 * A branch is what you pick; the commit it resolves to is what gets shown and
 * recorded. `main` here means `origin/main`, never a local branch of the same
 * name.
 */
export function SutPanel({
  sutRef,
  onChangeRef,
}: {
  sutRef: string;
  onChangeRef: (next: string) => void;
}) {
  const [status, setStatus] = React.useState<SutStatus | null>(null);
  const [branches, setBranches] = React.useState<SutBranch[]>([]);
  const [branchProblem, setBranchProblem] = React.useState<string | null>(null);
  const [build, setBuild] = React.useState<SutBuild | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);

  const refresh = React.useCallback(async (ref: string) => {
    if (!ref) {
      setStatus(null);
      return;
    }
    try {
      setStatus(await api<SutStatus>(`/api/sut?ref=${encodeURIComponent(ref)}`));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  React.useEffect(() => {
    refresh(sutRef);
  }, [sutRef, refresh]);

  React.useEffect(() => {
    api<{ branches: SutBranch[]; problem: string | null }>("/api/sut/branches")
      .then((res) => {
        setBranches(res.branches || []);
        setBranchProblem(res.problem);
      })
      .catch((err) => setBranchProblem(String(err)));
  }, []);

  // Poll only while a build is live. A finished build is terminal, so a timer
  // that keeps running past it is pure noise on an idle machine.
  React.useEffect(() => {
    if (!build || build.done) return;
    const timer = setInterval(async () => {
      try {
        const next = await api<SutBuild>(`/api/sut/builds/${build.id}`);
        setBuild(next);
        if (next.done) {
          setBusy(false);
          if (next.status === "done") refresh(sutRef);
        }
      } catch {
        /* a dropped poll is not a failed build; the next tick retries */
      }
    }, 3000);
    return () => clearInterval(timer);
  }, [build, sutRef, refresh]);

  const startBuild = React.useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const started = await postJson<SutBuild>("/api/sut/build", { ref: sutRef });
      setBuild(started);
      if (started.done) {
        setBusy(false);
        if (started.status === "done") refresh(sutRef);
      }
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }, [sutRef, refresh]);

  const pinned = sutRef !== "";
  const ready = pinned && !!status?.ready;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <label className="text-[11px] lowercase text-muted" htmlFor="sut-ref">
          build from
        </label>
        <select
          id="sut-ref"
          value={sutRef}
          onChange={(event) => onChangeRef(event.target.value)}
          className="min-w-[240px] rounded-[7px] border border-line bg-panel px-2 py-1.5 font-mono text-[12px]"
        >
          {branches.map((branch) => (
            <option key={branch.ref} value={branch.name}>
              {branch.name}
              {branch.is_default ? " (default)" : ""} · {branch.short}
            </option>
          ))}
          {/* Kept last so it reads as the deliberate exception it is. */}
          <option value="">— unpinned (do not verify the binary) —</option>
        </select>

        {pinned && !ready && (
          <Button variant="primary" size="sm" onClick={startBuild} disabled={busy}>
            {busy ? "building…" : "build it"}
          </Button>
        )}
      </div>

      {branchProblem && <ErrorBox>{branchProblem}</ErrorBox>}

      {!pinned && (
        <div className="rounded-[10px] border border-warn/35 bg-warn/6 px-[13px] py-2.5 text-[12.5px] text-warn">
          This match will run whatever binary is staged, without checking which
          commit it is. Results are recorded as an <strong>unverified</strong> SUT
          and cannot be attributed to a Stella revision.
        </div>
      )}

      {pinned && status && (
        <div
          className={cn(
            "rounded-[10px] border px-[13px] py-2.5 text-[12.5px]",
            ready
              ? "border-ok/35 bg-ok/6 text-ok"
              : "border-warn/35 bg-warn/6 text-warn",
          )}
        >
          {ready ? (
            <>
              <div>
                will run <span className="font-mono">{status.staged?.short}</span> —
                exactly <span className="font-mono">{status.ref}</span>
              </div>
              <div className="mt-1 text-muted">
                sha256 {status.staged?.sha256?.slice(0, 16) || "unrecorded"}
              </div>
            </>
          ) : (
            <>
              <div>{status.problem}</div>
              {status.drift && !status.drift.comparable && (
                <div className="mt-1 text-muted">
                  the staged binary cannot be placed relative to this commit, so
                  how stale it is cannot be stated
                </div>
              )}
            </>
          )}
        </div>
      )}

      {build && (
        <div className="rounded-[10px] border border-line bg-panel px-[13px] py-2.5 text-[12px]">
          <div className="flex items-center gap-2">
            <span className="font-mono">{build.short || build.ref}</span>
            <span className="text-muted">
              {build.cached
                ? "already built — nothing to compile"
                : `${build.status} · ${Math.round(build.elapsed)}s`}
            </span>
          </div>
          {build.status === "failed" && (
            <div className="mt-1.5 text-danger">{build.error}</div>
          )}
          {!build.cached && build.log_tail?.length > 0 && (
            <pre className="mt-1.5 max-h-32 overflow-auto whitespace-pre-wrap font-mono text-[11px] text-muted">
              {build.log_tail.slice(-6).join("\n")}
            </pre>
          )}
        </div>
      )}

      {error && <ErrorBox>{error}</ErrorBox>}
    </div>
  );
}
