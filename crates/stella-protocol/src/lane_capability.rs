//! The names a lane may hold a turn-loop seam under.
//!
//! The engine carries a set of optional seams. A builtin lane fills them
//! with a struct literal, so the compiler makes it answer for each one. A
//! lane a plugin ships cannot get that: its manifest is a file, and no file
//! can fail a build. It gets the weaker check instead, at load time, and it
//! needs a name for each seam to do that (`doc:turn-lane-assembly` §9.2).
//!
//! [`LaneCapability`] is that set of names. It lives here because the two
//! crates that need it may not see each other: `stella-core` owns the seams,
//! `stella-plugin` reads the manifest, and neither may depend on the other.
//! One table under both of them is AGENTS.md #1 applied as written.
//!
//! The table below is the whole of it: the enum, the list, the word each
//! name is written as, and the risk grades. A test pins the serde spelling
//! to the same word. `stella-core` holds itself to the
//! same table with a test that takes its seam struct apart field by field, so
//! a new seam breaks that test's build until a name is added here.
//!
//! Each name carries a [`RiskLevel`] because a manifest **asks** and the host
//! **grants**: `granted = requested ∩ authorized`. Grading the seams here
//! means the ladder a plugin declares and the ceiling a host applies read one
//! set of numbers.

use serde::{Deserialize, Serialize};

use crate::RiskLevel;

macro_rules! lane_capabilities {
    ($( $(#[$doc:meta])* $case:ident => $wire:literal, $risk:ident; )+) => {
        /// One optional seam of the turn loop, named so a manifest can ask
        /// for it.
        ///
        /// The order is the order of the table that defines it, and it is
        /// the order every report prints in.
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum LaneCapability {
            $(
                $(#[$doc])*
                $case,
            )+
        }

        impl LaneCapability {
            /// Every name, in table order.
            pub const ALL: &'static [Self] = &[$( Self::$case, )+];

            /// The word a manifest writes.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$case => $wire, )+
                }
            }

            /// How far this seam lets a lane reach, in the grade a gate
            /// reads.
            #[must_use]
            pub fn risk(self) -> RiskLevel {
                match self {
                    $( Self::$case => RiskLevel::$risk, )+
                }
            }

            /// The name a manifest wrote, or `None` when this build knows no
            /// such seam.
            ///
            /// `None` is a load error at the caller, never a skip. A name
            /// nobody reads is a grant that quietly does nothing, which is
            /// the failure the manifest rules exist to stop.
            #[must_use]
            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $( $wire => Some(Self::$case), )+
                    _ => None,
                }
            }
        }
    };
}

lane_capabilities! {
    /// Lifecycle hooks and the port that runs them.
    Hooks => "hooks", Medium;
    /// Where a hook's approval ask parks until a human answers.
    HookApprovals => "hook_approvals", Medium;
    /// Token-drift calibration the lane owns across turns.
    Calibration => "calibration", Medium;
    /// The step-boundary pause gate, which can hold a turn open.
    Gate => "gate", High;
    /// Mid-turn messages and the soft stop.
    Steering => "steering", Medium;
    /// Step-boundary context re-query.
    Requery => "requery", Medium;
    /// The event bus. Watches; changes nothing.
    Bus => "bus", Low;
    /// Call-outcome feedback for a router's breaker.
    Outcomes => "outcomes", Medium;
    /// Provider fallback once the retries run out.
    Fallback => "fallback", Medium;
}

impl std::fmt::Display for LaneCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name crosses a crate line, so it round-trips byte for byte
    /// (AGENTS.md #4).
    #[test]
    fn every_name_round_trips_byte_for_byte() {
        for capability in LaneCapability::ALL {
            let encoded = serde_json::to_string(capability).expect("serialises");
            assert_eq!(encoded, format!(r#""{}""#, capability.as_str()));
            let decoded: LaneCapability = serde_json::from_str(&encoded).expect("parses back");
            assert_eq!(decoded, *capability);
        }
    }

    /// `parse` is the door a manifest comes through, so it must take every
    /// name this build writes and nothing else.
    #[test]
    fn parse_accepts_exactly_the_names_this_build_knows() {
        for capability in LaneCapability::ALL {
            assert_eq!(
                LaneCapability::parse(capability.as_str()),
                Some(*capability)
            );
        }
        assert_eq!(LaneCapability::parse("teleport"), None);
        assert_eq!(LaneCapability::parse(""), None);
        assert_eq!(LaneCapability::parse("Hooks"), None);
    }

    /// The list is what every caller walks, so a repeat in it would show up
    /// as a doubled row in a report.
    #[test]
    fn the_list_has_no_repeats() {
        let mut seen = LaneCapability::ALL.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), LaneCapability::ALL.len());
    }

    /// The grades are what a ceiling is written against, so each rung has to
    /// hold something. A table where every seam graded the same would make a
    /// ceiling either all or nothing.
    #[test]
    fn the_grades_span_more_than_one_rung() {
        assert_eq!(LaneCapability::Bus.risk(), RiskLevel::Low);
        assert_eq!(LaneCapability::Steering.risk(), RiskLevel::Medium);
        assert_eq!(LaneCapability::Gate.risk(), RiskLevel::High);
        assert!(LaneCapability::Bus.risk().within(RiskLevel::Medium));
        assert!(!LaneCapability::Gate.risk().within(RiskLevel::Medium));
    }
}
