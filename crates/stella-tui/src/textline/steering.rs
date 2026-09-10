// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One line for a steering candidate the turn could not afford.

use super::{EventLine, Tone};

/// What a budget refused, and the knob that widens it.
///
/// The remedy rides `detail`. A narrow screen then drops the advice and
/// keeps the handle, which is the split [`super::skill_injected`] makes and
/// the one [`crate::notice`] draws.
///
/// The em dash comes from the emitter, and the first one splits the line. A
/// remedy is always written after one. A handle cannot hold one.
#[must_use]
pub fn steering_dropped(advisory: &str) -> EventLine {
    let (body, detail) = match advisory.split_once(" — ") {
        Some((head, remedy)) => (head.to_string(), Some(remedy.to_string())),
        None => (advisory.to_string(), None),
    };
    EventLine {
        glyph: "\u{26a0}",
        tone: Tone::Warn,
        strong: false,
        body,
        detail,
    }
}
