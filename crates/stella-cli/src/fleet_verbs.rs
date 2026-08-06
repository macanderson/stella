//! The `stella fleet <verb>` surface: the subcommand flags, and the single
//! point that dispatches one.
//!
//! Only the flag definitions and the `match` live here. Each verb's actual
//! work is a sibling module — [`crate::fleet_gc`] for `clean`,
//! [`crate::fleet_claims`] for `claims` — for the reason `fleet_gc.rs` was
//! split out in the first place: `fleet_cmd.rs` is a grandfathered god file
//! closed to growth (AGENTS.md § *God files*), so the fleet's non-fan-out
//! surface grows sideways rather than into it.
//!
//! The enum sits in its own module rather than in the first verb's file
//! (where it started, when `clean` was the only one) so adding a verb does
//! not mean declaring it inside a module named after a different one.

use clap::Subcommand;

use crate::config::Config;
use crate::query_format::QueryFormat;

/// The `stella fleet <cmd>` subcommands. Kept in its own enum (rather than
/// more flags on `stella fleet`) so a verb cannot be confused with a task
/// prompt — with one caveat worth knowing: a positional prompt whose **first
/// word** is exactly a subcommand name is read as that subcommand. Prefix the
/// prompts with `--` when that happens
/// (`stella fleet -- clean up the dead code`).
#[derive(Subcommand, Debug, Clone)]
pub(crate) enum FleetCmd {
    /// Reclaim finished isolated worktrees and their fleet/* branches
    ///
    /// Sweeps .stella/worktrees/ and the fleet/ branch namespace, removing
    /// only what is provably safe to remove: a worktree with no unfinished
    /// attempt in the ledger, a clean working tree, and a branch whose
    /// commits the base ref already contains. Anything else is KEPT and the
    /// reason printed. The main checkout and every branch outside fleet/ are
    /// never touched, with or without --force. Offline: git plus
    /// .stella/private/fleet.db, needs no API key.
    Clean {
        /// Decide and print, remove nothing
        #[arg(long)]
        dry_run: bool,

        /// Also reclaim worktrees with uncommitted changes and branches whose
        /// commits are not in the base ref — this DESTROYS that work. Never
        /// touches a worktree whose attempt is still in flight.
        #[arg(long)]
        force: bool,

        /// Only consider worktrees whose last attempt finished at least this
        /// many days ago (a worktree with no recorded finish time is kept)
        #[arg(long, value_name = "DAYS")]
        age: Option<u32>,

        /// The ref a branch's commits must already be contained in to count
        /// as reclaimable (default: origin/HEAD, else main, else master)
        #[arg(long, value_name = "REF")]
        base_ref: Option<String>,
    },

    /// Who currently holds which unit of dispatch
    ///
    /// Reads the dispatch-claim leases in .stella/private/fleet.db: the key
    /// claimed, the run holding it, how long it has held it, and when the
    /// lease lapses. This is what to check before handing out work by hand —
    /// a claim nobody can see is a claim somebody collides with (#1136).
    /// Read-only and offline; needs no API key. There is deliberately no
    /// verb to break a claim: a lease expires on its own, and forcing one
    /// open is how the same task reaches two workers.
    Claims {
        /// Also list lapsed claims — who last held a key and when it expired,
        /// which is the question a dead session leaves behind
        #[arg(long)]
        all: bool,

        /// Output format: text, or json under the versioned query envelope
        #[arg(long, value_enum, default_value = "text")]
        format: QueryFormat,
    },
}

/// Run one `stella fleet` subcommand.
pub(crate) async fn run(cfg: &Config, cmd: &FleetCmd) -> Result<(), String> {
    match cmd {
        FleetCmd::Clean {
            dry_run,
            force,
            age,
            base_ref,
        } => {
            crate::fleet_gc::clean(
                &cfg.workspace_root,
                &stella_fleet::GcOptions {
                    dry_run: *dry_run,
                    force: *force,
                    min_age_days: *age,
                    base_ref: base_ref.clone(),
                },
            )
            .await
        }
        FleetCmd::Claims { all, format } => {
            crate::fleet_claims::claims(&cfg.workspace_root, *all, *format)
        }
    }
}
