//! The turn-bracketing workspace probe surface of [`super::ToolRegistry`] —
//! arming a before-image, settling a turn's delta into the file ledger, and
//! the shared attribution tail both probe granularities record through. A
//! child module of `registry` so the ledger internals stay reachable, split
//! out to keep the registry under the size gate.

use super::*;

impl ToolRegistry {
    /// Take a before-image of the workspace for the turn about to run.
    ///
    /// Paired with [`Self::settle_workspace_probe`]. `classify_file_op` reads
    /// a tool's *input*, which cannot describe what `bash` will touch, so the
    /// tree is asked instead — see [`crate::shell_touch`] for the measurement
    /// that motivated it (757 of 1,063 Terminal-Bench tool calls were shell
    /// calls, and the ledger recorded none of them).
    ///
    /// Bracketing the TURN rather than each call is deliberate and measured:
    /// one walk of a 20,000-entry tree costs ~1s, a turn issues hundreds of
    /// shell calls, and the harness kills a trial at 900s. Per-call probing
    /// would have spent the entire trial budget watching itself work. The
    /// cost is that a change is attributed to the turn rather than to the
    /// exact call within it — which is all the ledger and the ladder ever
    /// read.
    /// Latches this session as turn-bracketing, which is what turns the
    /// per-call probe in [`Self::execute`] off. Calling this once commits the
    /// session to turn granularity; the two never run together.
    pub fn begin_workspace_probe(&self) {
        self.turn_bracketing
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.bracket_opaque_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let probe = crate::shell_touch::WorkspaceProbe::capture(&self.root);
        *self
            .workspace_probe
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(probe);
    }

    /// Attribute anything this turn changed that no tool schema described,
    /// then re-arm for the next turn from the same walk.
    ///
    /// Re-arming here rather than making the caller call
    /// [`Self::begin_workspace_probe`] again is what keeps the cost at one
    /// walk per settle: the after-image of this turn *is* the before-image of
    /// the next one, taken at the only instant where both are true. Walking
    /// twice in a row to produce two snapshots of the same tree would double
    /// the probe's cost to re-learn what it just measured.
    ///
    /// Routed through `record_touch` so a `FileChange` keeps exactly
    /// one birthplace. Paths the CRUD tools already recorded are re-recorded
    /// here as the same op on the same path — the ledger is keyed by path and
    /// folds repeats, so an edit that a tool declared and the tree confirms
    /// stays one entry rather than becoming two.
    pub fn settle_workspace_probe(&self) {
        let Some(before) = self
            .workspace_probe
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        else {
            return;
        };
        let after = crate::shell_touch::WorkspaceProbe::capture(&self.root);
        // Attribution needs a cause. The probe exists to answer "what did the
        // opaque calls do?", so a bracket that dispatched no opaque call has
        // nothing to ask it — any delta it sees is foreign motion (a
        // concurrent edit in a shared worktree), and recording it would
        // fabricate a `FileChange` for work nothing in this session did
        // (#1553). The fresh after-image still re-arms below either way, so
        // a disowned delta is dropped, never deferred into the next
        // bracket's attribution.
        if self
            .bracket_opaque_calls
            .swap(0, std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            self.record_probe_delta(
                &before,
                &after,
                crate::shell_touch::PROBE_TOOL_LABEL,
                &serde_json::json!({ "reason": "observed in the workspace" }),
                None,
            );
        }
        *self
            .workspace_probe
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(after);
    }

    /// Attribute one probe delta through [`Self::record_touch`] — the shared
    /// tail of the per-call probe in [`Self::execute`] and the turn probe in
    /// [`Self::settle_workspace_probe`], so the naming gate and the pre-image
    /// fallback rule cannot drift between the two granularities.
    pub(super) fn record_probe_delta(
        &self,
        before: &crate::shell_touch::WorkspaceProbe,
        after: &crate::shell_touch::WorkspaceProbe,
        tool: &str,
        input: &Value,
        bus: Option<&HookBus>,
    ) {
        for touch in before.diff(after) {
            // The probe walks the tree itself, but its keys still go
            // through the naming gate before the ledger records them.
            if crate::resolve_within_root(&self.root, &touch.path).is_none() {
                continue;
            }
            // An unmeasurable update carries its own post-image as the
            // pre-image, yielding 0/0. The alternative — an empty
            // pre-image — would render a same-file rewrite as a
            // whole-file insertion, which is a louder lie than silence.
            let pre_content = match (touch.counts_measurable, touch.op) {
                (true, _) => touch.pre_content,
                (false, FileOp::Update | FileOp::Delete) => {
                    crate::rootfd::read_confined_lossy(&self.root, &touch.path)
                }
                (false, _) => None,
            };
            self.record_touch(
                PendingTouch {
                    path: touch.path,
                    op: touch.op,
                },
                pre_content,
                tool,
                input,
                bus,
            );
        }
    }
}
