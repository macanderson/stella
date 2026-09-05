// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a `!!` or `!!!` mark is saved.
//!
//! The deck cannot write the file. It is a fold, and a write is I/O. So the
//! deck cuts the mark off, sends the usual [`WorkspaceInput::Interrupt`], and
//! tags it with the strength it was asked for. The tag is spent here.
//!
//! `intercept` runs in front of both read points in the driver. It does not
//! sit in the arms that take an interrupt:
//!
//! - **A bad write must not eat the stop.** A save that fails hands the
//!   message back as it came. The turn still stops. The error goes on screen.
//! - **One save, not three.** An interrupt lands in three arms: at rest, at
//!   the lead, and at a lane. A save in each is three ways to write the file
//!   twice, or not at all.
//! - **`command_deck.rs` is a god file, closed to growth.** Each call site
//!   there spends one word. The work is here.
//!
//! What the save reaches, and what it does not: this session picks the record
//! up on its next load, and so does every session started after it. A session
//! already running elsewhere does not. Those read the record set once, at
//! start, and nothing swaps it under them yet.
//!
//! Split out of [`super`], on the `steer` and `settle` pattern.

use std::path::Path;

use tokio::sync::mpsc::UnboundedSender;

use crate::context_records::decree;
use stella_tui::{Inbound, KeepStrength, WorkspaceInput};

/// Spend the mark on `input`. Hand the message back for the driver to route.
///
/// The mark is cleared on the way through. So the file is written once, even
/// if a later arm reads the message again. All else passes as it came.
pub(super) fn intercept(
    input: Option<WorkspaceInput>,
    root: &Path,
    in_tx: &UnboundedSender<Inbound>,
) -> Option<WorkspaceInput> {
    match input {
        Some(WorkspaceInput::Interrupt {
            agent,
            texts,
            keep: Some(keep),
        }) => {
            save(root, keep, &texts, in_tx);
            Some(WorkspaceInput::Interrupt {
                agent,
                texts,
                keep: None,
            })
        }
        other => other,
    }
}

/// Write the file. Say what happened, on one line either way.
///
/// The mark sends one text. Joining is what makes this total. A message that
/// somehow held two would save both, not drop one on the quiet.
fn save(root: &Path, keep: KeepStrength, texts: &[String], in_tx: &UnboundedSender<Inbound>) {
    let statement = texts.join(" ");
    let note = match decree::publish(root, keep, &statement) {
        Ok(saved) if saved.unchanged => {
            format!(
                "already kept — {} says this already",
                shown(root, &saved.path)
            )
        }
        Ok(saved) => match &saved.superseded {
            Some(_) => format!(
                "kept as {} in {} — it replaces the earlier wording",
                strength(keep),
                shown(root, &saved.path)
            ),
            None => format!("kept as {} in {}", strength(keep), shown(root, &saved.path)),
        },
        // The stop runs either way, since the message goes back whole. Say
        // nothing here and a person is left trusting a rule nobody wrote.
        Err(why) => format!("the turn stopped, but keeping that failed: {why}"),
    };
    let _ = in_tx.send(super::chrome_note(note));
}

/// How the note names the strength. It uses the words the file uses, so the
/// two agree.
fn strength(keep: KeepStrength) -> &'static str {
    match keep {
        KeepStrength::Guidance => "guidance (should)",
        KeepStrength::Rule => "a rule (must)",
    }
}

/// The path as a person reads it. Short, when the file is in the tree. Full,
/// when it is not.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests;
