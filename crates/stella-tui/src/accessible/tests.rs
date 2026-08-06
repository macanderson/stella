//! The invariants #1258 names as "easy to get wrong and must be tested":
//! a settled entry reaches scrollback exactly once, the flush counter never
//! advances without a write, and a lane change neither re-flushes nor drops.

use super::*;
use crate::envelope::{AgentMeta, Inbound};
use stella_protocol::{AgentEvent, StageKind};

fn reg(id: &str) -> Inbound {
    Inbound::Register(AgentMeta::new(id, format!("goal for {id}"), 0))
}

fn say(agent: &str, text: &str) -> Inbound {
    Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::Text {
            text: text.to_string(),
        },
    }
}

/// A stage row, used here only as a separator: consecutive `Text` events
/// coalesce into one transcript entry (`SessionModel::push_text`), so without
/// something between them a lane of "four messages" is one entry.
fn stage(agent: &str) -> Inbound {
    Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::Stage {
            name: StageKind::Execute,
        },
    }
}

/// A lane of `messages` separate transcript entries, the last of which is
/// still the trailing (growable) one.
fn lane(model: &mut WorkspaceModel, id: &str, messages: usize) {
    model.apply_inbound(&reg(id));
    for i in 0..messages {
        model.apply_inbound(&stage(id));
        model.apply_inbound(&say(id, &format!("{id} line {i}")));
    }
}

/// The lane's transcript length, which the flush counters index into.
fn entries(model: &WorkspaceModel, id: &str) -> usize {
    model
        .agents
        .iter()
        .find(|a| a.meta.id == id)
        .map(|a| a.model.transcript.len())
        .unwrap_or(0)
}

fn live() -> Scrollback {
    let mut sb = Scrollback::default();
    sb.set_live(true);
    sb
}

#[test]
fn a_session_without_an_inline_viewport_plans_nothing() {
    // The whole mechanism is gated on the viewport, not on the mode: with any
    // other viewport `insert_before` is a silent no-op, so a plan produced
    // here could only ever advance a counter against a write that never
    // happened.
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 5);
    let sb = Scrollback::default();
    assert!(!sb.is_live());
    assert!(sb.plan(&model, false).is_empty());
    assert!(sb.plan(&model, true).is_empty());
}

#[test]
fn a_settled_entry_is_planned_exactly_once() {
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 4);
    let mut sb = live();

    let first = sb.plan(&model, false);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].from, 0);
    for block in &first {
        sb.record(block);
    }
    assert!(
        sb.plan(&model, false).is_empty(),
        "a recorded block must never be planned again — a reader would hear it twice"
    );
}

#[test]
fn the_counter_does_not_advance_without_a_write() {
    // `plan` is pure. Only `record` — which the shell calls with the
    // `insert_before` result in hand — may move the counter.
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 3);
    let sb = live();

    let once = sb.plan(&model, false);
    let twice = sb.plan(&model, false);
    assert_eq!(once, twice, "planning is not a side effect");
    assert_eq!(
        sb.flushed_for("lead"),
        0,
        "nothing was written, so nothing is in scrollback"
    );
}

#[test]
fn the_trailing_entry_is_held_back_until_teardown() {
    // Streaming deltas coalesce into the trailing entry, so flushing it
    // mid-session writes half a sentence and then the whole sentence.
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 3);
    let total = entries(&model, "lead");
    let sb = live();

    let mid = sb.plan(&model, false);
    assert_eq!(mid[0].to, total - 1, "the tail can still grow");

    let teardown = sb.plan(&model, true);
    assert_eq!(
        teardown[0].to, total,
        "by teardown nothing can grow, so the remainder goes out"
    );
}

#[test]
fn every_lane_is_flushed_and_a_lane_change_neither_re_flushes_nor_drops() {
    // The deck has lanes; #1237's single-conversation rule did not. Switching
    // tabs or focus touches neither the transcripts nor the counters, so the
    // plan after a switch must be empty — and when new work lands on the
    // *other* lane, only that lane's new entries go out.
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 3);
    lane(&mut model, "sub", 2);
    let mut sb = live();

    let plan = sb.plan(&model, false);
    assert_eq!(plan.len(), 2, "both lanes flush: {plan:?}");
    for block in &plan {
        sb.record(block);
    }
    let lead_flushed = sb.flushed_for("lead");
    let sub_flushed = sb.flushed_for("sub");

    // A focus/tab change is not a model mutation.
    assert!(sb.plan(&model, false).is_empty());

    model.apply_inbound(&stage("sub"));
    model.apply_inbound(&say("sub", "one more"));
    let after = sb.plan(&model, false);
    assert_eq!(after.len(), 1, "only the lane that grew: {after:?}");
    assert_eq!(after[0].agent, "sub");
    assert_eq!(
        after[0].from, sub_flushed,
        "resumes exactly where the lane left off — no gap, no repeat"
    );
    assert_eq!(
        sb.flushed_for("lead"),
        lead_flushed,
        "the untouched lane is unmoved"
    );
}

#[test]
fn a_lane_header_is_written_only_when_the_voice_changes() {
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 3);
    let mut sb = live();

    let solo = sb.plan(&model, false);
    assert!(
        solo[0].header.is_none(),
        "one lane is one voice; naming it every flush is noise"
    );
    for block in &solo {
        sb.record(block);
    }

    lane(&mut model, "sub", 3);
    let mixed = sb.plan(&model, false);
    assert_eq!(mixed.len(), 1);
    assert_eq!(mixed[0].agent, "sub");
    let header = mixed[0].header.as_deref().expect("the voice changed");
    assert!(header.starts_with(NOTICE_MARKER), "{header:?}");
    assert!(header.contains("sub"), "{header:?}");
    for block in &mixed {
        sb.record(block);
    }

    model.apply_inbound(&stage("sub"));
    model.apply_inbound(&say("sub", "still me"));
    let same = sb.plan(&model, false);
    assert!(
        same[0].header.is_none(),
        "the same lane twice running is one voice: {same:?}"
    );
}

#[test]
fn front_eviction_shifts_the_counter_instead_of_dropping_entries() {
    // Front-eviction moves every retained index down. Left alone, the counter
    // would suppress entries from the live pane that were never written
    // anywhere — the exact failure the two-step design exists to prevent.
    let mut sb = live();
    let block = FlushBlock {
        agent: "lead".into(),
        from: 0,
        to: 500,
        header: None,
    };
    sb.record(&block);
    sb.shift_after_eviction("lead", 399);
    assert_eq!(sb.flushed_for("lead"), 101);
    sb.shift_after_eviction("lead", 10_000);
    assert_eq!(
        sb.flushed_for("lead"),
        0,
        "a shift past the front floors at zero rather than wrapping"
    );
}

#[test]
fn announcements_are_dropped_when_there_is_nowhere_to_put_them() {
    let mut dark = Scrollback::default();
    dark.announce("SESSION tab");
    assert!(
        dark.take_announcements().is_empty(),
        "with no inline viewport there is no scrollback to announce into"
    );

    let mut lit = live();
    lit.announce("SESSION tab");
    assert_eq!(lit.take_announcements(), vec!["SESSION tab".to_string()]);
    assert!(lit.take_announcements().is_empty(), "taking drains");
}

#[test]
fn a_block_renders_its_header_and_only_its_own_range() {
    // `lane` interleaves a stage row before each message, so the entries are
    // 0:stage 1:"line 0" 2:stage 3:"line 1" 4:stage 5:"line 2" 6:stage
    // 7:"line 3". This block owns 3..6.
    let mut model = WorkspaceModel::new();
    lane(&mut model, "lead", 4);
    let block = FlushBlock {
        agent: "lead".into(),
        from: 3,
        to: 6,
        header: Some("▸ lead".into()),
    };
    let text: String = block_lines(&model, &block, false, 60)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("▸ lead"), "{text}");
    assert!(text.contains("lead line 1"), "{text}");
    assert!(text.contains("lead line 2"), "{text}");
    assert!(
        !text.contains("lead line 3"),
        "the range is half-open: {text}"
    );
    assert!(
        !text.contains("lead line 0"),
        "already in scrollback: {text}"
    );
}

#[test]
fn the_inline_viewport_always_leaves_a_row_above_itself() {
    // The viewport anchors to the cursor's row; claiming the whole screen
    // leaves `insert_before` nowhere to append and every flush scrolls the
    // world instead.
    assert_eq!(inline_viewport_rows(40), DECK_INLINE_VIEWPORT_ROWS);
    assert_eq!(inline_viewport_rows(24), 23);
    assert_eq!(inline_viewport_rows(2), 1);
    assert_eq!(
        inline_viewport_rows(1),
        1,
        "a terminal with no row to spare gets a cramped viewport, never a broken flush"
    );
    assert_eq!(inline_viewport_rows(0), 1);
}

#[test]
fn a_degraded_session_says_so_and_a_working_one_stays_quiet() {
    // The promise of the mode is that finished messages become durable
    // terminal output. When the viewport cannot be anchored they do not, and a
    // user who scrolls back to re-read an answer that is not there has been
    // told a silent lie.
    let notice = degrade_notice(true, false).expect("a degraded session announces");
    assert!(notice.contains("scrollback"), "{notice}");
    assert_eq!(
        degrade_notice(true, true),
        None,
        "a session that got what it asked for has nothing to report"
    );
    assert_eq!(
        degrade_notice(false, false),
        None,
        "an ordinary session never promised scrollback, so this is not its news"
    );
}

#[test]
fn accessible_mode_owns_the_screen_and_the_mouse() {
    assert_eq!(screen_for(true), Screen::Normal);
    assert_eq!(screen_for(false), Screen::Alternate);
    assert!(
        !mouse_capture_enabled(true, true),
        "capture takes the terminal's own selection away, and selection is how \
         several assistive technologies read"
    );
    assert!(mouse_capture_enabled(true, false));
    assert!(!mouse_capture_enabled(false, false));
}
