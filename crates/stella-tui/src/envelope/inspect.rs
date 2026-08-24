//! The INSPECT overlay's payload types — one recorded model call, and the
//! reconstructed context behind it.
//!
//! Split out of `envelope.rs` when that file reached the 1500-line limit: this
//! cluster is the one part of the envelope that is a self-contained read-model
//! rather than a routing tag, and nothing outside it refers to these types
//! except the two [`Inbound`](super::Inbound) variants that carry them.
//!
//! Like every other type on the envelope, these mirror `stella_store` shapes
//! the TUI never links — the driver maps one to the other.

/// One recorded model call — the INSPECT overlay's row. Mirrors
/// `stella_store::RecordedCall`; the TUI never links the store crate, so the
/// driver maps one to the other (same contract as [`IssueRow`](super::IssueRow)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedCallInfo {
    pub turn_instance: u32,
    pub step: u64,
    /// Which call at this step: 0 the engine's worker, 1 the overflow
    /// summarizer, 2+ a pipeline management role.
    pub call_seq: u64,
    pub call_role: String,
    pub provider: String,
    pub model: String,
    pub estimated_input_tokens: u64,
}

impl RecordedCallInfo {
    /// The coordinate triple, for the detail header and the request echo.
    pub fn coordinate(&self) -> (u32, u64, u64) {
        (self.turn_instance, self.step, self.call_seq)
    }
}

/// One provenance-labelled span of a system prompt: which setting put these
/// bytes in the prompt this call was sent.
///
/// The split is the driver's, not this crate's — the markers are the prompt
/// assembler's own section-opener constants, which live in `stella-cli` where
/// the assembler does (`agent/prompt/provenance.rs`). The overlay renders what
/// it is handed and knows no headings, exactly as it knows no protocol types.
///
/// The bodies concatenate, in order, to the exact bytes the call was sent, so
/// a reader who distrusts a boundary has lost nothing by reading the sections
/// instead of the raw message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectSection {
    /// What this span is, in a reader's words — `Workspace rules`, …
    pub label: String,
    /// The configuration surface that produced it.
    pub source: String,
    /// The exact bytes, marker heading included.
    pub body: String,
}

/// One message of a reconstructed call, flattened for display: tool calls and
/// results are already rendered into `content` by the driver, so the overlay
/// never needs the protocol types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectMessage {
    pub role: String,
    pub content: String,
    /// The provenance breakdown of `content`, when the driver could compute
    /// one — the system prefix, and nothing else. Empty everywhere else, and
    /// the overlay falls back to `content` when it is: a message with no
    /// breakdown renders exactly as it always did.
    pub sections: Vec<InspectSection>,
}

/// Which compaction-journaling era wrote the journal a reconstruction came
/// from — the deck's mirror of `stella_store::JournalEra` (this crate links no
/// store; the driver maps one to the other, exactly as it does for every other
/// type on this envelope).
///
/// It exists here for one reason: it decides whether a digest mismatch is
/// routine housekeeping or a real integrity signal, and the overlay must not
/// guess (#1981).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JournalEra {
    /// Compaction rewrote tool results in place and journaled nothing about it
    /// (#1667 and earlier), so a compacted block mismatches as a matter of
    /// course. The default: an era the deck was not told reads as the one
    /// where a mismatch means the least.
    #[default]
    CompactionUnjournaled,
    /// Every compaction rewrite is journaled, so a compacted block resolves to
    /// exactly what the step sent and a mismatch has no routine explanation.
    CompactionJournaled,
}

/// The reconstructed context of one model call — what the INSPECT overlay
/// shows. Carries the verification verdict alongside the bytes, because a
/// transcript you cannot vouch for is worse than none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectView {
    pub call: RecordedCallInfo,
    pub messages: Vec<InspectMessage>,
    /// Every journal-resolved block re-hashed to its recorded digest.
    pub verified: bool,
    /// Blocks with no recoverable preimage — a documented coverage gap
    /// (synthetic results, discarded speculation, attachments).
    pub unresolved: usize,
    /// Blocks whose recovered bytes did NOT re-hash to the digest they
    /// recorded — the preimage shown is the closest one the journal holds, not
    /// the exact bytes this step sent. Kept distinct from `unresolved`: the two
    /// mean very different things. What a mismatch *means* depends on
    /// [`Self::journal_era`] — see [`Self::digest_mismatch_line`].
    pub digest_mismatches: usize,
    /// Who wrote the journal these blocks were resolved from.
    pub journal_era: JournalEra,
}

impl InspectView {
    /// The overlay's mismatch line and whether it is a genuine integrity
    /// signal, or `None` when nothing mismatched.
    ///
    /// The wording lives here rather than in the renderer for two reasons.
    /// `deck_render.rs` is a god file at its ceiling, so a second branch there
    /// costs lines it does not have; and the overlay **clips** rather than
    /// wraps, so each variant has to be short enough to survive the right edge
    /// intact — which is a property of the string, testable here, not of the
    /// draw call. Both variants stay under 80 columns and put the *meaning*
    /// before the detail, so the part that distinguishes them survives even on
    /// the narrowest terminal the popup can be drawn in.
    ///
    /// A legacy journal's mismatch stays the benign warning #1668 introduced,
    /// naming compaction as the cause. A journal that records every rewrite
    /// has no such excuse, so the same mismatch reads as the alarm it is
    /// (#1981).
    pub fn digest_mismatch_line(&self) -> Option<(String, bool)> {
        let n = self.digest_mismatches;
        if n == 0 {
            return None;
        }
        Some(match self.journal_era {
            JournalEra::CompactionUnjournaled => (
                format!("  ! {n} block(s) did not re-hash — an older journal's compaction rewrite"),
                false,
            ),
            JournalEra::CompactionJournaled => (
                format!("  !! {n} block(s) unaccounted for — this journal records its rewrites"),
                true,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The INSPECT overlay clips rather than wraps, and its popup is
    /// `min(frame - 6, 120)` columns wide — so on an 80-column terminal a
    /// banner line has about 72 to work with. Both variants are written to
    /// that budget and put the meaning first; this pins it, because a wording
    /// change that silently pushes the distinguishing half off the right edge
    /// would undo #1981 without failing anything else (see #2029).
    #[test]
    fn both_mismatch_lines_survive_an_eighty_column_terminal() {
        for era in [
            JournalEra::CompactionUnjournaled,
            JournalEra::CompactionJournaled,
        ] {
            let view = InspectView {
                call: RecordedCallInfo {
                    turn_instance: 0,
                    step: 1,
                    call_seq: 0,
                    call_role: "worker".into(),
                    provider: "zai".into(),
                    model: "glm-5.2".into(),
                    estimated_input_tokens: 10,
                },
                messages: Vec::new(),
                verified: false,
                digest_mismatches: 999,
                unresolved: 0,
                journal_era: era,
            };
            let (line, _) = view.digest_mismatch_line().expect("a mismatch is reported");
            assert!(line.chars().count() <= 72, "{} cols: {line}", line.len());
        }
    }
}
