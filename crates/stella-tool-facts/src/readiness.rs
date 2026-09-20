// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Semantic index coverage. An incomplete index never blocks a prompt.

/// File coverage and whether the reporting pass has stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexReadiness {
    /// Files in the code-graph index.
    pub total_files: usize,
    /// Files with no whole-file vector under the active embedder.
    pub unindexed_files: usize,
    /// True once the reporting pass has stopped, including failure.
    pub settled: bool,
}

impl IndexReadiness {
    /// No coverage measurement is available.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            total_files: 0,
            unindexed_files: 0,
            settled: true,
        }
    }

    /// Files with a whole-file vector the ranker can read.
    #[must_use]
    pub const fn indexed_files(&self) -> usize {
        self.total_files.saturating_sub(self.unindexed_files)
    }

    /// Less than half of the files have a whole-file vector.
    /// Unknown or empty workspaces have no measurable percentage.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        self.total_files > 0 && self.indexed_files() < self.total_files.div_ceil(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degradation_is_strictly_below_half_even_after_a_failed_pass() {
        for settled in [false, true] {
            for (total, indexed, degraded) in [
                (100, 49, true),
                (100, 50, false),
                (100, 51, false),
                (101, 50, true),
                (101, 51, false),
                (1, 0, true),
                (1, 1, false),
                (0, 0, false),
                (usize::MAX, usize::MAX / 2, true),
            ] {
                let state = IndexReadiness {
                    total_files: total,
                    unindexed_files: total - indexed,
                    settled,
                };
                assert_eq!(state.is_degraded(), degraded, "{state:?}");
            }
        }
        assert!(!IndexReadiness::unknown().is_degraded());
    }
}
