"use client";

import * as React from "react";
import { api } from "@/lib/api";
import type { Catalog, Dataset, Snapshot } from "@/lib/types";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Topbar, type ConnState } from "@/components/topbar";
import { SetupView } from "@/components/setup/setup-view";
import { ArenaView } from "@/components/arena/arena-view";

export type View = "setup" | "arena";

/**
 * Owns what both views need: the catalog, which view is showing, which match
 * is open, and the connection badge. Both views stay mounted and toggle with
 * CSS — switching tabs must never tear down a live SSE stream or a
 * half-assembled match.
 */
export function AppShell() {
  const [catalog, setCatalog] = React.useState<Catalog | null>(null);
  const [bootError, setBootError] = React.useState<string | null>(null);
  const [view, setView] = React.useState<View>("setup");
  const [matchId, setMatchId] = React.useState<string | null>(null);
  const [seedSnapshot, setSeedSnapshot] = React.useState<Snapshot | null>(null);
  const [conn, setConn] = React.useState<ConnState>({ tone: "", label: "idle" });
  const [historyVersion, setHistoryVersion] = React.useState(0);

  React.useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [datasets, agents] = await Promise.all([
          api<{ datasets: Dataset[] }>("/api/datasets"),
          api<Omit<Catalog, "datasets">>("/api/agents"),
        ]);
        if (cancelled) return;
        setCatalog({
          datasets: datasets.datasets,
          agents: agents.agents,
          efforts: agents.efforts,
          roles: agents.roles,
        });
      } catch (error) {
        if (!cancelled) setBootError(String(error));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const openMatch = React.useCallback((id: string, seed?: Snapshot) => {
    setSeedSnapshot(seed ?? null);
    setMatchId(id);
    setView("arena");
  }, []);

  return (
    <TooltipProvider>
      <Topbar view={view} onViewChange={setView} arenaEnabled={matchId !== null} conn={conn} />
      <main className={view === "setup" ? "block" : "hidden"}>
        <SetupView
          catalog={catalog}
          bootError={bootError}
          onOpenMatch={openMatch}
          historyVersion={historyVersion}
        />
      </main>
      {matchId !== null && (
        <main className={view === "arena" ? "block" : "hidden"}>
          <ArenaView
            key={matchId}
            matchId={matchId}
            seedSnapshot={seedSnapshot}
            onConnChange={setConn}
            onMatchEnded={() => setHistoryVersion((v) => v + 1)}
          />
        </main>
      )}
    </TooltipProvider>
  );
}
