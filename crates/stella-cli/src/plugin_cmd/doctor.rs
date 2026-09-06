// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella plugin doctor` — what each plugin lane asked for, what it holds,
//! and what nobody decided (`doc:turn-lane-assembly` §9.2).
//!
//! # Why the command exists
//!
//! A lane this tree ships is checked by the compiler. The seam set has no
//! default. A new seam breaks the build until each lane answers for it.
//!
//! A lane a plugin ships gets no such check. Its manifest is a file, and a
//! file cannot fail a build. So a seam added later reaches that lane as
//! nothing at all, and nobody chose that. This command names it. **A
//! defaulted slot is reported, not assumed.**
//!
//! # It reports; it changes nothing
//!
//! Every line comes from the manifests on disk, from the merged settings, and
//! from the gate this session would bind. Nothing is written. No process
//! starts. No grant moves.
//!
//! # Three authorities, reported apart
//!
//! A seam has to clear three things. The rung a person took at install. The
//! operator's `lanes.custom` ceiling. The session's [`AuthzGate`]. Each one
//! can only take a seam away. The report names which one did. Only the
//! gate's reason is one a plugin author can act on.

use std::path::Path;

use stella_core::ports::AuthzGate;
use stella_plugin::{ConsentedGrade, LaneAuthority, PluginManifest};
use stella_protocol::{LaneCapability, LaneId};

use super::roster::PluginRoster;
use crate::plugin_authz::lanes::narrowed_by_gate;
use crate::settings::{LaneCeiling, LaneSettings, Settings};

/// What a lane may hold on this machine: the rung a person accepted at
/// install, narrowed by the ceiling the operator wrote.
///
/// This is the host's half of `granted = requested ∩ authorized`. The rung is
/// what a person read and agreed to. The ceiling is what they wrote after. A
/// seam has to clear both, so neither can undo the other.
struct HostLaneAuthority<'a> {
    /// The rung the manifest declared and a person accepted.
    grade: ConsentedGrade,
    /// The operator's own ceiling for this lane, when a scope named it.
    ceiling: Option<&'a LaneCeiling>,
}

impl LaneAuthority for HostLaneAuthority<'_> {
    fn authorizes(&self, lane: &LaneId, capability: LaneCapability) -> bool {
        if !self.grade.authorizes(lane, capability) {
            return false;
        }
        match self.ceiling {
            Some(ceiling) => ceiling.known().contains(&capability),
            // No scope named this lane, so the operator set no ceiling and
            // the rung is the whole answer. Absent is not empty.
            None => true,
        }
    }
}

/// Run the command: read what is installed, print the report.
///
/// # Errors
///
/// None of its own. The signature matches the other verbs so the dispatch in
/// [`super::run_plugin`] stays one shape.
pub(super) fn doctor(workspace_root: &Path, settings: &Settings) -> Result<(), String> {
    let (roster, notices) = PluginRoster::load(workspace_root, settings);
    for notice in &notices {
        eprintln!("{notice}");
    }
    if roster.plugins().is_empty() {
        println!("no plugins installed — add one with `stella plugin install <dir>`");
        return Ok(());
    }
    // The same gate a session binds, read once here: `check_lane` is pure over
    // what it prefetched, so the roster read happens now and the report asks a
    // value.
    let gate = crate::agent::tool_stack::session_gate(workspace_root);
    for plugin in roster.plugins() {
        for line in lane_lines(&plugin.manifest, settings.lanes.as_ref(), gate.as_ref()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The report for one plugin.
///
/// Pure, and kept apart from the printing. What it says is what a test needs
/// to read. A function that also opened directories could not have one.
fn lane_lines(
    manifest: &PluginManifest,
    ceilings: Option<&LaneSettings>,
    gate: &dyn AuthzGate,
) -> Vec<String> {
    let mut lines = vec![manifest.name.clone()];
    let lanes = match manifest.declared_lanes() {
        Ok(lanes) => lanes,
        // Unreachable for a manifest the roster loaded, which came through
        // `from_toml_str`. Said out loud rather than unwrapped: a report that
        // panicked on a hand-built manifest would be worse than one that
        // names the reason.
        Err(error) => {
            lines.push(format!("  its lanes do not load: {error}"));
            return lines;
        }
    };
    if lanes.is_empty() {
        lines.push("  ships no lane of its own".into());
        return lines;
    }

    let grade = ConsentedGrade(manifest.loop_grant.participation);
    for lane in lanes {
        let ceiling = ceilings.and_then(|settings| settings.ceiling(lane.id()));
        let authority = HostLaneAuthority { grade, ceiling };
        let consented = lane.grant(&authority);
        let gated = narrowed_by_gate(&consented, gate, &manifest.name);
        let grant = &gated.grant;

        lines.push(format!(
            "  lane `{}` — {}, resumed by {}",
            lane.id(),
            lane.participation(),
            lane.resume()
        ));
        lines.push(format!("    asks for: {}", say(grant.requested.iter())));
        lines.push(format!("    holds:    {}", say(grant.granted.iter())));

        let withheld: Vec<LaneCapability> = grant
            .withheld()
            .into_iter()
            .filter(|capability| !gated.refused.contains_key(capability))
            .collect();
        if !withheld.is_empty() {
            lines.push(format!(
                "    withheld: {} — above the `{}` rung accepted at install, or \
                 outside this workspace's `lanes.custom` ceiling",
                say(withheld.iter()),
                manifest.loop_grant.participation
            ));
        }

        // A gate's refusal is not the rung being too low. It is a rule an
        // operator wrote. Its reason is the one part of the report a plugin
        // author can act on.
        for (capability, reason) in &gated.refused {
            lines.push(format!(
                "    refused:  {capability} — `{}` says: {reason}",
                gate.name()
            ));
        }

        // The line the command exists for. A seam in neither of the lane's
        // lists is one nobody answered for, and it holds nothing because
        // nothing filled it — not because somebody said no.
        let defaulted = lane.defaulted();
        if defaulted.is_empty() {
            lines.push("    every seam is answered for".into());
        } else {
            lines.push(format!(
                "    nobody decided: {} — this lane was written before these \
                 seams existed, so it holds none of them",
                say(defaulted.iter())
            ));
        }

        for unknown in ceiling.map(LaneCeiling::unknown).unwrap_or_default() {
            lines.push(format!(
                "    ! `lanes.custom.{}.capabilities` names `{unknown}`, which this \
                 build has no seam for",
                lane.id()
            ));
        }
    }
    lines
}

/// A set of seam names as one line, or the word for an empty one.
fn say<'a>(capabilities: impl Iterator<Item = &'a LaneCapability>) -> String {
    let named: Vec<String> = capabilities.map(ToString::to_string).collect();
    if named.is_empty() {
        "nothing".into()
    } else {
        named.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::{AuthzDecision, AuthzEvalError, LaneSeam, NoAuthz, Principal};
    use stella_plugin::Participation;
    use stella_protocol::ToolContract;

    const REPLAY: &str = r#"
        name = "acme"

        [loop]
        participation = "steering"

        [lanes.custom."acme.replay"]
        resume = "redispatch"
        capabilities = ["bus", "steering"]
        declined = ["gate"]
    "#;

    fn manifest(text: &str) -> PluginManifest {
        PluginManifest::from_toml_str(text).expect("the manifest loads")
    }

    fn report(text: &str, settings: &str) -> String {
        report_under(text, settings, &NoAuthz)
    }

    /// The same report, under a gate a test supplies.
    fn report_under(text: &str, settings: &str, gate: &dyn AuthzGate) -> String {
        let ceilings: LaneSettings =
            serde_json::from_str(settings).expect("the lane settings parse");
        lane_lines(&manifest(text), Some(&ceilings), gate).join("\n")
    }

    /// A gate that refuses the watching seam and allows the rest.
    struct NoBus;

    impl AuthzGate for NoBus {
        fn name(&self) -> &'static str {
            "no-bus"
        }

        fn check(
            &self,
            _contract: &ToolContract,
            _principal: &Principal,
            _input: &serde_json::Value,
        ) -> Result<AuthzDecision, AuthzEvalError> {
            Ok(AuthzDecision::Allow)
        }

        fn check_lane(
            &self,
            seam: &LaneSeam,
            principal: &Principal,
        ) -> Result<AuthzDecision, AuthzEvalError> {
            if seam.capability == LaneCapability::Bus {
                return Ok(AuthzDecision::Deny {
                    reason: format!("{} may not watch the bus", principal.label()),
                });
            }
            Ok(AuthzDecision::Allow)
        }
    }

    /// **The witness for the report.** A seam nobody answered for is named.
    /// That is the load-time stand-in for a compile error.
    ///
    /// Nothing on the base commit can declare a lane, so no report there has
    /// anything to say.
    #[test]
    fn a_defaulted_seam_is_named() {
        let printed = report(REPLAY, "{}");
        assert!(printed.contains("lane `acme.replay`"), "{printed}");
        assert!(printed.contains("nobody decided:"), "{printed}");
        assert!(printed.contains("requery"), "{printed}");
        assert!(
            !printed.contains("nobody decided: bus"),
            "a seam the lane asked for was not left undecided: {printed}"
        );
    }

    /// A seam the lane turned down in writing is an answer, so it is not
    /// reported as undecided.
    #[test]
    fn a_seam_turned_down_in_writing_is_not_undecided() {
        let printed = report(REPLAY, "{}");
        let undecided = printed
            .lines()
            .find(|line| line.contains("nobody decided:"))
            .expect("the report names the undecided seams");
        assert!(
            !undecided.contains("gate"),
            "`gate` was declined in writing: {undecided}"
        );
    }

    /// **The witness for the two columns.** The ask is printed as written.
    /// What the lane holds is printed beside it. So a lane that lost a seam
    /// to the operator's ceiling still shows that it asked.
    #[test]
    fn the_ask_and_the_holding_are_reported_apart() {
        let printed = report(
            REPLAY,
            r#"{"custom":{"acme.replay":{"capabilities":["bus"]}}}"#,
        );
        assert!(printed.contains("asks for: bus, steering"), "{printed}");
        assert!(printed.contains("holds:    bus"), "{printed}");
        assert!(printed.contains("withheld: steering"), "{printed}");
    }

    /// With no ceiling written, the rung accepted at install is the whole
    /// answer — an absent entry is not an empty one.
    #[test]
    fn a_lane_no_scope_named_keeps_what_its_rung_allows() {
        let printed = report(REPLAY, "{}");
        assert!(printed.contains("holds:    bus, steering"), "{printed}");
        assert!(!printed.contains("withheld:"), "{printed}");
    }

    /// A name in the settings this build has no seam for is reported rather
    /// than read as a grant.
    #[test]
    fn a_settings_name_this_build_does_not_know_is_reported() {
        let printed = report(
            REPLAY,
            r#"{"custom":{"acme.replay":{"capabilities":["bus","teleport"]}}}"#,
        );
        assert!(printed.contains("names `teleport`"), "{printed}");
        assert!(printed.contains("holds:    bus"), "{printed}");
    }

    /// A plugin that ships no lane says so, so a reader can tell "none" from
    /// "the command found nothing".
    #[test]
    fn a_plugin_with_no_lane_says_so() {
        let printed = lane_lines(&manifest("name = \"acme\"\n"), None, &NoAuthz).join("\n");
        assert!(printed.contains("ships no lane of its own"), "{printed}");
    }

    /// The rung is read out of the seam list, not out of the `[loop]` block,
    /// so a lane weaker than its plugin renders as the weaker thing.
    #[test]
    fn the_lane_rung_is_the_lanes_own() {
        let printed = report(
            r#"
            name = "acme"

            [loop]
            participation = "steering"

            [lanes.custom."acme.watch"]
            resume = "parent"
            capabilities = ["bus"]
            "#,
            "{}",
        );
        assert!(
            printed.contains("lane `acme.watch` — observer, resumed by parent"),
            "{printed}"
        );
    }

    /// The grade half of the authority stands on its own: a rung that does
    /// not reach a seam withholds it with no ceiling written anywhere.
    #[test]
    fn the_rung_alone_can_withhold_a_seam() {
        let lane = manifest(REPLAY)
            .declared_lanes()
            .expect("the lanes load")
            .remove(0);
        let authority = HostLaneAuthority {
            grade: ConsentedGrade(Participation::Observer),
            ceiling: None,
        };
        let grant = lane.grant(&authority);
        assert_eq!(grant.withheld().len(), 1);
        assert!(grant.withheld().contains(&LaneCapability::Steering));
    }

    /// **The witness for the gate's line.** A seam the gate refused is
    /// reported as refused. The gate's own name and reason go with it. The
    /// old report had no third authority to name.
    #[test]
    fn a_seam_the_gate_refuses_is_reported_as_refused() {
        let printed = report_under(REPLAY, "{}", &NoBus);
        assert!(printed.contains("holds:    steering"), "{printed}");
        assert!(
            printed.contains("refused:  bus — `no-bus` says: plugin:acme may not watch the bus"),
            "{printed}"
        );
        assert!(
            !printed.contains("withheld: bus"),
            "a seam the gate refused is not blamed on the rung: {printed}"
        );
    }

    /// A gate that keeps the default `check_lane` leaves the report exactly
    /// where it was, so a deployment whose gate governs only tool calls reads
    /// what it always read.
    #[test]
    fn a_gate_with_no_lane_opinion_changes_no_line() {
        let tool_only = stella_core::ports::RiskCeiling::new(stella_protocol::RiskLevel::Low);
        assert_eq!(
            report_under(REPLAY, "{}", &NoAuthz),
            report_under(REPLAY, "{}", &tool_only)
        );
        assert!(!report(REPLAY, "{}").contains("refused:"));
    }
}
