//! The text of the recall block. Each section is drawn here, and so is each
//! line that reports what a budget refused.

use stella_protocol::RecalledFrame;
use stella_records::records::RenderedChannel;

use super::RECORD_CHANNEL_BUDGET;

/// Render recalled frames as the "Relevant context" section of the recall
/// block. Memory-kind frames carry their stable `[nod_…]` id inline — the
/// durable handle a reader (or a later promotion sweep) can resolve back to
/// the record. Other frame kinds (code-graph hits, episodes) keep the plain
/// label form: they are grounding, not memories. `None` when no frame has a
/// citation label (L-C4 filters the rest).
pub fn render_context_section(frames: &[RecalledFrame]) -> Option<String> {
    let lines: Vec<String> = frames.iter().map(frame_recall_line).collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!("Relevant context:\n{}", lines.join("\n")))
}

/// The recall line ONE frame contributes to the section above.
///
/// Only a label that says something the content does not earns its bytes:
/// memory (and episode) nodes mint theirs FROM the content, so rendering both
/// shipped the same sentence twice into a recall budget the packer had
/// already spent on the content alone (#2476). A memory with an id keeps the
/// id visible — it names the record, and it is the join key
/// `receipts::parse_recall_item` reads back. A memory WITHOUT one still has
/// content worth recalling and the budget has already been spent fetching it;
/// `RecalledFrame` documents `id: None` as a legitimate state for a
/// not-yet-materialized frame (`crates/stella-protocol/src/recall.rs`), so it
/// renders as grounding.
///
/// Split out of the section loop so the steering plane's frame adapter
/// (`crate::memory::steering::frame_candidates`) estimates cost over these exact
/// bytes — one producer, per #3334.
pub(in crate::memory) fn frame_recall_line(f: &RecalledFrame) -> String {
    stella_core::receipts::render_recall_line(&stella_core::receipts::RecallLine {
        id: (f.kind == "memory").then_some(f.id.as_deref()).flatten(),
        label: f.distinct_label(),
        body: f.content.trim(),
        source: None,
    })
}

/// The record channel's section, from its rendered bytes. `None` for a blank
/// render — an empty section would only burn tokens.
pub(super) fn record_section_text(rendered: RenderedChannel) -> Option<String> {
    let text = rendered.text;
    (!text.trim().is_empty()).then(|| text.trim_start().to_string())
}

/// The eviction report for one dropped candidate — the single producer every
/// source emits through, in the record channel's sentence shape: *what
/// applied, which budget refused it, its handle, and the remedy*. The remedy
/// is the half that differs, and it has to — telling a user whose skill lost
/// its seat to "raise its precedence" is advice for a different channel.
///
/// `still_selected` is the section-budget class, and it is why this takes the
/// whole ledger rather than a handle. A skill can be in `selected` *and*
/// `dropped` by design: top-k kept it and `skills::section_fit` then left it
/// out of the rendered section. Both classes miss the prompt, so both are
/// reported, and only the remedy differs —
/// `SKILLS_SECTION_TOKEN_BUDGET` is a constant, and nothing widens it until
/// the two budgets are collapsed into one.
///
/// Memory drops return `None`. The frame query already reported them as one
/// summary line naming the budget and the remedy.
///
/// A tool drop names the tool and the allowance that refused it, and the
/// remedy is the allowance: withholding a tool is never a capability change
/// (`crate::tool_lean`), so the advice is to widen what the session may spend
/// on schemas, or to turn the lever off.
///
/// A plugin drop names the plugin and the stage it spoke at, because the
/// handle is `<plugin>/<stage>` and a person can act on both halves. The
/// remedy is the allowance or the plugin, never the stage.
pub(super) fn drop_message(
    drop: &stella_core::steering::DroppedCandidate,
    still_selected: bool,
) -> Option<String> {
    use stella_core::steering::SteeringSource;
    let handle = &drop.handle;
    match drop.source {
        SteeringSource::Record => Some(format!(
            "a record applying to this turn did not fit the {RECORD_CHANNEL_BUDGET}-char \
             record budget: ^{handle} — raise its precedence, or trim the records that \
             outrank it"
        )),
        SteeringSource::Memory => None,
        SteeringSource::Skill if still_selected => Some(format!(
            "a skill matching this turn did not fit the skills section's token budget: \
             {handle} — nothing configurable widens that budget yet"
        )),
        SteeringSource::Skill => Some(format!(
            "a skill matching this turn did not fit the skill budget: {handle} — raise \
             `skills.max_skills`"
        )),
        SteeringSource::Tool => Some(format!(
            "a tool did not fit what this turn's records, skills and frames left of the \
             steering allowance, and was not advertised: {handle} — it still runs if it \
             is called; raise `context.steering.max_tokens`, or set \
             `context.steering.tools.lean` false to advertise every tool"
        )),
        SteeringSource::Plugin => Some(format!(
            "a plugin's contribution did not fit what this turn's records, skills and frames \
             left of the steering allowance, and was not put in front of the model: {handle} \
             — the plugin still ran; raise `context.steering.max_tokens`, or take the plugin \
             out of `active_plugins`"
        )),
    }
}

/// Report every candidate the ledger says was dropped, whatever its source.
/// The ledger records the cuts; this is the half a person reads.
///
/// Memory drops are summarized rather than listed. `memory_budget` is the
/// `context.retrieval.max_tokens` the turn ran with, and the report is one
/// line: how many memories missed it, the budget, and the knob that widens
/// it. One line per memory repeats that remedy under an id nobody can act on.
///
/// Two recall-side filters stay out of this report.
/// `project_recalled_frame` drops a frame the citation-label rule cannot
/// name, and `is_suppressed_local_frame` drops one the session quarantined.
/// Neither is a budget eviction, so reporting either here would tell a user
/// their budget was too small when it was not. The provider's spend on both
/// is already in the usage report taken above the filters.
pub(crate) fn report_steering_drops(
    set: &stella_core::steering::SteeringSet,
    memory_budget: u32,
    mut report: impl FnMut(String),
) {
    let memory_drops = set
        .dropped
        .iter()
        .filter(|d| d.source == stella_core::steering::SteeringSource::Memory)
        .count();
    if memory_drops > 0 {
        report(format!(
            "{memory_drops} memories did not fit this turn's {memory_budget}-token retrieval \
             budget — raise context.retrieval.max_tokens in stella.toml to include them"
        ));
    }
    for drop in &set.dropped {
        let still_selected = set
            .selected
            .iter()
            .any(|c| c.source == drop.source && c.handle == drop.handle);
        if let Some(message) = drop_message(drop, still_selected) {
            report(message);
        }
    }
}

/// The wall clock's current instant, in Unix seconds. The one `SystemTime`
/// read in this module — everything downstream of it (`render_today_section`)
/// takes the value as a parameter instead of reading the clock itself, so a
/// test can inject two different instants without racing real time.
pub(super) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Render today's date for the VOLATILE recall block: the second operand of
/// the knowledge-cutoff staleness clause in
/// `crate::agent::prompt::append_session_environment`, which names a cutoff
/// but never a "now" to measure it against (#2901) — a model handed one
/// endpoint of an interval cannot reason about its width.
///
/// This is why it lives here and not beside the cutoff it completes: the
/// cutoff is session-constant (a model card fact, fixed at catalog load) and
/// rides the byte-stable system prefix, but the date changes once a day —
/// possibly mid-session — so putting it in the prefix would be a guaranteed
/// prompt-cache miss at every UTC midnight for every session alive across it
/// (invariant #7 / L-E8), and `the_assembled_prompt_names_the_session_environment`
/// (`crates/stella-cli/src/agent/prompt.rs`) would start failing the moment a
/// test ran across one. It belongs with the other genuinely volatile facts —
/// selected skills, per-turn records — that ride this per-turn block instead.
///
/// Takes Unix seconds rather than reading the clock itself so it stays a pure
/// function of its input: two calls a day apart must render different text,
/// and neither call may touch `SystemTime::now()` for that difference to be
/// provable without racing real time.
pub(in crate::memory) fn render_today_section(unix_secs: i64) -> String {
    let today = &stella_context::format_rfc3339(unix_secs)[..10];
    format!(
        "Today's date: {today} UTC — measure the knowledge-cutoff staleness clause in the \
         session environment above against this."
    )
}
