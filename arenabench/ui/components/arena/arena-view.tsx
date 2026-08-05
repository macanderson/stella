"use client";

import * as React from "react";
import { postJson } from "@/lib/api";
import type { Snapshot } from "@/lib/types";
import { fmtClock } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { StatusPill } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/ui/alert-dialog";
import type { ConnState } from "@/components/topbar";
import { Scoreboard } from "@/components/arena/scoreboard";
import { Race } from "@/components/arena/race";
import { TaskRail } from "@/components/arena/task-rail";
import { Lane } from "@/components/arena/lane";

export function ArenaView({
  matchId,
  seedSnapshot,
  onConnChange,
  onMatchEnded,
}: {
  matchId: string;
  seedSnapshot: Snapshot | null;
  onConnChange: (conn: ConnState) => void;
  onMatchEnded: () => void;
}) {
  const [snapshot, setSnapshot] = React.useState<Snapshot | null>(seedSnapshot);
  const [activeTask, setActiveTask] = React.useState<string | null>(null);
  const [follow, setFollow] = React.useState(true);
  const [showReasoning, setShowReasoning] = React.useState(true);
  const [confirmStop, setConfirmStop] = React.useState(false);
  const [cancelError, setCancelError] = React.useState<string | null>(null);

  // The callbacks live in refs so a parent re-render can never tear down and
  // reopen the match stream.
  const connRef = React.useRef(onConnChange);
  connRef.current = onConnChange;
  const endedRef = React.useRef(onMatchEnded);
  endedRef.current = onMatchEnded;

  React.useEffect(() => {
    const source = new EventSource(`/api/matches/${encodeURIComponent(matchId)}/stream`);
    connRef.current({ tone: "live", label: "live" });
    source.addEventListener("snapshot", (event) => {
      setSnapshot(JSON.parse((event as MessageEvent).data));
    });
    source.addEventListener("end", () => {
      connRef.current({ tone: "", label: "finished" });
      source.close();
      endedRef.current();
    });
    source.onerror = () => connRef.current({ tone: "error", label: "reconnecting…" });
    return () => source.close();
  }, [matchId]);

  const cancelMatch = React.useCallback(async () => {
    setCancelError(null);
    try {
      await postJson(`/api/matches/${encodeURIComponent(matchId)}/cancel`, {});
    } catch (error) {
      setCancelError(String(error));
    }
  }, [matchId]);

  if (!snapshot) {
    return <div className="p-8 text-[13px] text-dim">connecting to the match…</div>;
  }

  const activeRow = activeTask ? snapshot.rows.find((r) => r.task === activeTask) : null;

  return (
    <div className="px-3.5 pb-5 pt-3">
      <section className="mb-5 flex items-center gap-6">
        <div>
          <h1 className="text-[22px]">{snapshot.match.name}</h1>
          <div className="mt-[3px] font-mono text-[11.5px] text-muted">
            {snapshot.dataset.title} · {snapshot.rows.length} tasks ·{" "}
            {snapshot.match.contestants.length} contestants
            {snapshot.match.record_video ? ` · recording ${snapshot.recording_active} live` : ""}
          </div>
        </div>
        <div className="ml-auto text-right">
          <div className="font-mono text-[30px] font-light leading-none tracking-[-0.02em]">
            {fmtClock(snapshot.elapsed)}
          </div>
          <div className="mt-1 text-[10px] lowercase tracking-[0.1em] text-dim">elapsed</div>
        </div>
        <div className="flex items-center gap-2.5">
          <StatusPill status={snapshot.status} />
          <Button variant="ghost" onClick={() => setConfirmStop(true)}>
            stop
          </Button>
        </div>
      </section>

      <ConfirmDialog
        open={confirmStop}
        onOpenChange={setConfirmStop}
        title="stop this match?"
        description="Running trials are terminated and their containers stopped. Judged results stay on disk."
        confirmLabel="stop the match"
        onConfirm={cancelMatch}
      />

      {cancelError && (
        <div className="mb-[18px] rounded-lg border border-bad/40 bg-bad/7 px-3.5 py-2.5 text-[12.5px] text-bad">
          {cancelError}
        </div>
      )}

      {snapshot.note && (
        <div className="mb-[18px] rounded-lg border border-acc-citron/35 bg-acc-citron/6 px-3.5 py-2.5 text-[12.5px] text-acc-citron">
          {snapshot.note}
        </div>
      )}

      <Scoreboard snapshot={snapshot} />
      <Race snapshot={snapshot} />

      <section className="grid items-start gap-2.5 [grid-template-columns:216px_1fr] max-lg:[grid-template-columns:1fr]">
        <TaskRail snapshot={snapshot} activeTask={activeTask} onSelect={setActiveTask} />

        <section className="rounded-[10px] border border-line bg-panel">
          {activeTask === null ? (
            <div className="grid min-h-[180px] place-items-center p-5 text-center text-dim">
              <div>
                <div className="mb-1.5 text-[22px] text-line">◆</div>
                <p>Select a task to watch its transcripts stream side by side.</p>
              </div>
            </div>
          ) : (
            <>
              <div className="flex items-center gap-3.5 border-b border-line px-4 py-[13px]">
                <h3 className="font-mono text-sm">{activeTask}</h3>
                <div className="ml-auto flex gap-[7px]">
                  <Button variant="ghost" size="sm" onClick={() => setShowReasoning((v) => !v)}>
                    {showReasoning ? "hide reasoning" : "show reasoning"}
                  </Button>
                  <Button variant="ghost" size="sm" onClick={() => setFollow((v) => !v)}>
                    {follow ? "following" : "paused"}
                  </Button>
                </div>
              </div>
              <div
                className="grid gap-px bg-line"
                style={{
                  gridTemplateColumns: `repeat(${Math.min(snapshot.contestants.length, 3)}, minmax(0, 1fr))`,
                }}
              >
                {snapshot.contestants.map((c) => (
                  <Lane
                    key={`${activeTask}:${c.id}`}
                    matchId={matchId}
                    contestant={c}
                    task={activeTask}
                    cell={activeRow?.cells[c.id]}
                    follow={follow}
                    showReasoning={showReasoning}
                  />
                ))}
              </div>
            </>
          )}
        </section>
      </section>
    </div>
  );
}
