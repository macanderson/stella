//! Global + per-agent CPU/MEM sampling.
//!
//! One of the two labeled **out-of-band read-models**: these numbers are
//! sampled from the OS on the shell tick, never folded from `AgentEvent`s. The
//! same sample feeds both the dashboard/status-bar gauges and (later) dispatch
//! backpressure, so there is one source of truth for "how loaded are we".
//!
//! STUB: the real `sysinfo` wiring is filled in by the resource builder. The
//! signatures here are the frozen contract the shell tick calls.

use crate::deck::WorkspaceModel;

/// Samples system + per-process resource usage.
#[derive(Default)]
pub struct ResourceMonitor {
    _private: (),
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh the sample and stamp it onto the model: set
    /// `model.global_cpu_pct` and each agent's `res` from its `meta.pid`.
    /// A no-op until the sysinfo wiring lands.
    pub fn sample(&mut self, model: &mut WorkspaceModel) {
        let _ = model;
    }
}
