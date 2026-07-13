//! Global + per-agent CPU/MEM sampling.
//!
//! One of the two labeled **out-of-band read-models**: these numbers are
//! sampled from the OS on the shell tick, never folded from `AgentEvent`s. The
//! same sample feeds both the dashboard/status-bar gauges and (later) dispatch
//! backpressure, so there is one source of truth for "how loaded are we".
//!
//! Backed by `sysinfo`. CPU usage is a diff over time: the first `sample()`
//! call after construction reports 0% (there is no prior snapshot to diff
//! against) and subsequent calls — driven by the periodic shell tick, which
//! naturally spaces refreshes apart — report real utilization. This mirrors
//! the sysinfo-recommended pattern without an artificial startup sleep.

use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::deck::{ResourceSample, WorkspaceModel};

/// Samples system + per-process resource usage.
pub struct ResourceMonitor {
    sys: System,
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
        Self { sys: System::new() }
    }

    /// Refresh the sample and stamp it onto the model: set
    /// `model.global_cpu_pct` and each agent's `res` from its `meta.pid`.
    pub fn sample(&mut self, model: &mut WorkspaceModel) {
        self.sys.refresh_cpu_all();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);

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
}
