//! Global + per-agent CPU/MEM sampling.
//!
//! One of the two labeled **out-of-band read-models**: these numbers are
//! sampled from the OS on the shell tick, never folded from `AgentEvent`s. The
//! same sample feeds both the dashboard/status-bar gauges and (later) dispatch
//! backpressure, so there is one source of truth for "how loaded are we".
//!
//! Backed by `sysinfo`. CPU usage is a diff over time: the first `sample()`
//! call after construction reports 0% (there is no prior snapshot to diff
//! against) and later calls report real utilization. `sample()` self-throttles
//! to [`SAMPLE_INTERVAL`]: the shell tick may call it at animation rate
//! (~30 Hz), but real refreshes stay ~1 s apart — which is both the spacing
//! sysinfo needs for an accurate CPU diff and what keeps the OS query off the
//! per-frame budget. The process refresh is also narrowed to the registered
//! agent pids and to cpu+memory only, so the cost scales with the number of
//! agents rather than with the size of the machine's process table.

use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::deck::{ResourceSample, WorkspaceModel};

/// Minimum spacing between real refreshes; `sample()` calls inside the
/// window are no-ops and the previously stamped values stay put.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// Samples system + per-process resource usage.
pub struct ResourceMonitor {
    sys: System,
    /// When the last real refresh ran; `None` until the first `sample()`.
    last_refresh: Option<Instant>,
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceMonitor {
    pub fn new() -> Self {
        // `System::new()` starts with nothing loaded; the first `sample()`
        // populates the CPU list and process table and establishes the
        // baseline diff, so it reports zeroed usage by construction.
        Self {
            sys: System::new(),
            last_refresh: None,
        }
    }

    /// Refresh the sample and stamp it onto the model: set
    /// `model.global_cpu_pct` and each agent's `res` from its `meta.pid`.
    /// Self-throttled to [`SAMPLE_INTERVAL`] — safe to call every UI tick.
    pub fn sample(&mut self, model: &mut WorkspaceModel) {
        if self
            .last_refresh
            .is_some_and(|t| t.elapsed() < SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_refresh = Some(Instant::now());

        // The global gauge comes from the CPU list, not the process table, so
        // it refreshes unconditionally — even when no agent has a pid yet.
        self.sys.refresh_cpu_all();

        // Only the registered pids are ever read below, so refresh only those
        // and only the two fields we display. `ProcessesToUpdate::All` would
        // enumerate and stat every process on the machine once per second for
        // the life of the session. Built before `&mut self.sys` is taken so the
        // borrow of `model` ends first.
        let mut pids: Vec<Pid> = model
            .agents
            .iter()
            .filter_map(|a| a.meta.pid)
            .map(Pid::from_u32)
            .collect();
        // MUST be deduplicated. Agents in one workspace routinely share a pid
        // — the lead and every subagent register `std::process::id()` — and
        // sysinfo's `Some(..)` arm walks the caller's slice verbatim, flipping
        // each process's one-shot `updated` flag. The second visit to a
        // repeated pid sees the flag already consumed, reads it as "did not
        // refresh", and (with `remove_dead_processes = true`) evicts a LIVE
        // process from the table — so `process()` below returns `None` and
        // every agent renders 0% / 0 B. The old `ProcessesToUpdate::All` path
        // used `retain()`, which visits each process exactly once and was
        // structurally immune. Witness: `duplicate_pids_still_report_memory`.
        pids.sort_unstable();
        pids.dedup();
        if !pids.is_empty() {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&pids),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }

        model.global_cpu_pct = self.sys.global_cpu_usage();

        for agent in &mut model.agents {
            agent.res = agent
                .meta
                .pid
                .and_then(|pid| self.sys.process(Pid::from_u32(pid)))
                .map(|process| ResourceSample {
                    cpu_pct: process.cpu_usage(),
                    mem_bytes: process.memory(),
                })
                .unwrap_or_default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};

    #[test]
    fn sample_on_empty_model_does_not_panic() {
        let mut monitor = ResourceMonitor::new();
        let mut model = WorkspaceModel::new();
        monitor.sample(&mut model);
        assert_eq!(model.agents.len(), 0);
    }

    #[test]
    fn sample_dead_or_missing_pid_zeroes_the_sample() {
        let mut monitor = ResourceMonitor::new();
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("a1", "agent one", 0)));

        // No pid set — sampling must not panic and must leave a zeroed
        // reading.
        monitor.sample(&mut model);
        assert_eq!(model.agents[0].res, ResourceSample::default());
    }

    #[test]
    fn sample_current_process_reports_nonzero_memory() {
        let mut monitor = ResourceMonitor::new();
        let mut model = WorkspaceModel::new();
        let mut meta = AgentMeta::new("self", "current process", 0);
        meta.pid = Some(std::process::id());
        model.apply_inbound(&Inbound::Register(meta));

        // sysinfo needs the process table populated before `process()` can
        // resolve the pid; one `sample()` call does that.
        monitor.sample(&mut model);

        assert!(model.agents[0].res.mem_bytes > 0);
    }

    /// Witness for the duplicate-pid regression: a lead plus subagents all
    /// register the SAME `std::process::id()`, so the refresh list arrives as
    /// `[P, P, P]`. sysinfo's `ProcessesToUpdate::Some` arm walks that slice
    /// verbatim and consumes each process's one-shot `updated` flag, so the
    /// second visit to `P` reads as "stale" and — with `remove_dead_processes`
    /// on — deletes the live process from the table. Every agent then resolves
    /// to `None` and the AGENTS tab renders 0% / 0 B for the whole deck.
    ///
    /// The single-pid test above cannot catch this: n = 1 is the only arity
    /// that behaves correctly. Deduplicating before the refresh is what keeps
    /// this green.
    #[test]
    fn duplicate_pids_still_report_memory() {
        let mut monitor = ResourceMonitor::new();
        let mut model = WorkspaceModel::new();

        // Three agents, one shared pid — exactly what `command_deck.rs` and
        // `subsession.rs` produce for a lead with two subagents.
        let self_pid = std::process::id();
        for (i, id) in ["lead", "sub-a", "sub-b"].iter().enumerate() {
            let mut meta = AgentMeta::new(*id, format!("agent {i}"), 0);
            meta.pid = Some(self_pid);
            model.apply_inbound(&Inbound::Register(meta));
        }
        assert_eq!(model.agents.len(), 3, "all three agents registered");

        monitor.sample(&mut model);

        for agent in &model.agents {
            assert!(
                agent.res.mem_bytes > 0,
                "agent {} zeroed: duplicate pids evicted the live process from \
                 the sysinfo table (sample = {:?})",
                agent.meta.id,
                agent.res,
            );
        }
    }
}
