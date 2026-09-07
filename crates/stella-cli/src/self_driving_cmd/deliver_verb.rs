// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella self-driving deliver` — the pull request verbs a person runs by
//! hand.
//!
//! [`super::drive`] is the loop that runs these steps on its own.
//! [`super::deliver`] is the I/O half both of them go through. This is the
//! one-shot door. Open a pull request. Read one. Merge one.

use super::{DeliverCmd, config, deliver, state};
use crate::query_format::QueryFormat;

/// One pull request verb, run once.
///
/// Every arm that acts reads the forge again first. None of them trusts a
/// verdict handed to it. `merge` in particular does **not** take "the caller
/// already ran `next`" as proof. The forge moves between calls. A merge
/// allowed by a stale read is the very failure the machine's
/// `Mergeability::Unknown` arm exists to stop.
pub(super) fn run(cmd: &DeliverCmd) -> Result<(), String> {
    let root = state::repo_root();
    let forge = crate::pull_request_provider::GhPullRequests::new();

    match cmd {
        DeliverCmd::Open {
            issue,
            branch,
            title,
        } => {
            let attribution = config::load(&root).attribution;
            let pr = deliver::open(
                &forge,
                &root,
                branch,
                issue,
                title,
                &attribution.pull_request,
            )?;
            println!("opened #{pr} for #{issue} from {branch}");
            Ok(())
        }

        DeliverCmd::Observe { pr, format } => {
            let obs =
                deliver::observe(&forge, pr, &config::load(&state::repo_root()).merge)?.observation;
            if *format == QueryFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string(&obs).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "pr #{pr}: ci={:?} base_ci={:?} mergeable={:?} review={:?} draft={}",
                    obs.ci, obs.base_ci, obs.mergeable, obs.review, obs.draft
                );
            }
            Ok(())
        }

        DeliverCmd::Next {
            pr,
            fixes,
            rebases,
            no_review,
            format,
        } => {
            let transition = decide(pr, *fixes, *rebases, *no_review)?;
            if *format == QueryFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string(&transition).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "pr #{pr}: {:?} -> {:?}",
                    transition.state, transition.action
                );
            }
            Ok(())
        }

        DeliverCmd::Merge {
            pr,
            fixes,
            rebases,
            no_review,
        } => {
            let transition = decide(pr, *fixes, *rebases, *no_review)?;
            if transition.action != stella_autonomy::Action::Merge {
                // Refused, never skipped in silence. A caller that asked to
                // merge and got nothing has to be able to tell "already
                // merged" from "not allowed yet".
                return Err(format!(
                    "pr #{pr} is not mergeable yet — the machine says {:?} ({:?})",
                    transition.action, transition.state
                ));
            }
            deliver::merge(&forge, pr)?;
            println!("merged #{pr}");
            Ok(())
        }
    }
}

/// Read the forge. Run the pure machine over what it said.
fn decide(
    pr: &str,
    fixes: u32,
    rebases: u32,
    no_review: bool,
) -> Result<stella_autonomy::Transition, String> {
    let forge = crate::pull_request_provider::GhPullRequests::new();
    let obs = deliver::observe(&forge, pr, &config::load(&state::repo_root()).merge)?.observation;
    let policy = stella_autonomy::DeliverPolicy {
        require_approval: !no_review,
        ..stella_autonomy::DeliverPolicy::default()
    };
    let attempts = stella_autonomy::Attempts { fixes, rebases };

    // The state is worked out afresh each call. It is never stored. The forge
    // is the truth about a pull request. A stored state that disagreed with it
    // would be the stale read this design avoids. `CiPending` is the neutral
    // way in, and every arm of the machine is reachable from it.
    Ok(stella_autonomy::deliver_next(
        stella_autonomy::PrState::CiPending,
        &obs,
        attempts,
        &policy,
    ))
}
