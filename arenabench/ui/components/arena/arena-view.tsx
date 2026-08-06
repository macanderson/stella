"use client";

import * as React from "react";
import { postJson } from "@/lib/api";
import type { Snapshot } from "@/lib/types";
import { fmtClock } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { StatusPill } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/ui/alert-dialog";
import type { ConnState } from "@/components/topbar";
import { Race } from "@/components/arena/race";
import { StatStrip } from "@/components/arena/stat-strip";
import { SeatNotices } from "@/components/arena/seat-notices";
import { TaskTable } from "@/components/arena/task-table";

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

  return (
    /* The page gets real margins and a max width. Before this the arena ran
       edge to edge at any window size, which is what made a dense table of
       numbers read as clutter rather than as a table. */
    <div className="mx-auto max-w-[1600px] px-5 pb-10 pt-4 sm:px-7">
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

      {/* Whole-match numbers, full width, before anything per-seat. */}
      <StatStrip snapshot={snapshot} />

      {/* Why a seat's numbers look the way they do — a credential it could
          not find is the difference between "scored 0%" and "never ran". */}
      <SeatNotices snapshot={snapshot} />

      {/* The table carries the detail and gets three quarters; the race is a
          glanceable summary and gets one. They stack on a narrow window
          rather than squeezing the table into an unreadable column. */}
      <section className="grid items-start gap-4 lg:[grid-template-columns:3fr_1fr]">
        <TaskTable snapshot={snapshot} />
        <aside className="min-w-0">
          <div className="mb-1.5 px-1 text-[10px] lowercase tracking-[0.1em] text-dim">
            head to head
          </div>
          <Race snapshot={snapshot} />
        </aside>
      </section>

    </div>
  );
}
