//! The diff of what the agent *meant* to change.
//!
//! `git diff` answers "how does the tree differ from a commit". That is the
//! wrong question in two workspaces the agent is routinely run in, and the
//! judge has been paying for it in both:
//!
//! * **No repository at all.** A Terminal-Bench task directory is a plain
//!   directory, so the diff probe can only ever report that it could not look.
//!   Every judge call on that benchmark has been handed a sentence of English
//!   where a diff belongs.
//! * **A new file.** `git diff` cannot see an untracked path, so the single
//!   most common shape of agent work — *create the file that solves the task* —
//!   reaches the judge as a one-line marker naming the path and its length,
//!   carrying none of its content.
//!
//! Neither gap is a defect in git. They are the same category error twice:
//! asking a tool that reports **tree state** to answer a question about
//! **authorship**. This module answers the authorship question from the one
//! place that already knows it — every CRUD tool in this registry funnels
//! through `ToolRegistry::record_touch`, which holds the pre-image and
//! the post-image of a write at the instant it happens, and then throws both
//! away after rendering a single event.
//!
//! # Why the two provenances are not interchangeable
//!
//! A touch reaches the ledger by one of two routes, and they carry very
//! different confidence:
//!
//! * [`Provenance::Declared`] — a CRUD tool **named this path in its input**.
//!   The agent asked for this write, by path, through a schema we own. That is
//!   as close to intent as this process can observe.
//! * [`Provenance::Observed`] — [`crate::shell_touch`] saw the path change
//!   across an opaque call. Something changed it; *the agent* may not have
//!   meant it. A `tar xzf`, a `./configure`, a `make` and a deliberate
//!   `sed -i` are indistinguishable at this layer.
//!
//! The distinction is not academic. A single extraction of the SQLite source
//! tree produced ~1,200 `Observed` creations in one turn, every one recorded
//! as a full-content authored file. Rendering those as authored diff would
//! bury the agent's actual three-line fix under a megabyte of upstream test
//! fixtures, and would tell a judge that the turn wrote SQLite.
//!
//! So the two render differently, on purpose: `Declared` entries render as a
//! real unified diff with their content, and `Observed` entries render as
//! bounded marker lines that name the path and its size and nothing else. The
//! tree-state channel still reports them; this channel declines to call them
//! authorship.
//!
//! They are also **budgeted** separately, for the same reason and against the
//! same failure: every ceiling here — lines, retained paths, rendered markers
//! — reserves headroom that no volume of observation can spend. A bound shared
//! with the probe is a bound the probe wins, and the turn that floods it is
//! exactly the turn whose three-line fix most needs describing.
//!
//! # Cumulative, like the tool it replaces
//!
//! Entries fold by path and hold the **first** pre-image against the **latest**
//! post-image, so ten edits to one file render as one hunk describing the net
//! change. This matches what `git diff` would have shown against the session
//! baseline. Rendering only the last edit would be the more obvious
//! implementation and would quietly understate every iterated fix.

use std::collections::BTreeMap;

use crate::file_touch::{FileOp, changed_region_diff, line_diff};

/// The marker line shape for a change this ledger will not render in full.
///
/// Mirrors `stella_pipeline::witness::warrant::untracked_change_line`
/// **exactly**, and must keep mirroring it: the warrant parses this prefix back
/// out to hold marker-only paths to the same path rules as tracked ones, and a
/// hand-rolled variant here would silently blind it — the same defect that once
/// let a turn creating a new source file classify as docs-only. The two crates
/// cannot share a constant (neither depends on the other), so
/// [`tests::marker_line_matches_the_pipeline_contract`] pins the literal
/// instead.
const MARKER_PREFIX: &str = "+ untracked change: ";

/// Paths whose content this ledger will hold. Past this the ledger keeps
/// counting but stops retaining, so a runaway turn cannot grow it without
/// bound. Chosen well above any plausible authored change: an agent that
/// declares writes to more than 512 distinct paths in one session is not doing
/// the thing this channel exists to describe.
const MAX_TRACKED_PATHS: usize = 512;

/// Of [`MAX_TRACKED_PATHS`], the most that may be held for
/// [`Provenance::Observed`] paths. The remainder is reserved for authorship,
/// which is the only thing this channel exists to report.
///
/// Without the split the path budget is first-come, and opaque calls come
/// first. [`crate::shell_touch`] walks up to 20,000 entries and descends into
/// `target/` on purpose, so one `cargo build` spends every slot on `.rmeta`
/// files nobody authored; the ~1,200-file extraction in the module docs above
/// does the same thing twice over. The agent's actual three-line fix then
/// arrives at slot 513 and becomes a `dropped` counter — the ledger reports
/// *that* it stopped tracking, but the diff it exists to hand the judge is
/// gone, on precisely the turns it was built to survive.
///
/// So the rule [`AuthoredDiff::lines`] already states for the line budget — a
/// tarball must not be able to spend it — governs the budget that decides
/// whether a change is retained at all. Set well above
/// [`MAX_OBSERVED_MARKERS`] so the bound never starves the marker set it
/// feeds, and low enough that declared writes keep a floor of
/// `MAX_TRACKED_PATHS - MAX_TRACKED_OBSERVED` paths no observation can touch.
const MAX_TRACKED_OBSERVED: usize = 128;

/// Largest pre/post image held for one path. Matches
/// [`crate::shell_touch`]'s per-file content ceiling so the two channels agree
/// about what "too big to describe" means. A file over this is still recorded —
/// as a marker, never as silence.
const MAX_RETAINED_BYTES: usize = 256 * 1024;

/// Ceiling on the rendered text. The judge's context is finite and this is one
/// of several things competing for it; past this the render stops and says how
/// many files it did not reach.
const MAX_RENDER_BYTES: usize = 128 * 1024;

/// Ceiling on marker lines for [`Provenance::Observed`] paths. Fifty names
/// enough to see a pattern; the remainder is reported as a count. Without this
/// the SQLite extraction above would contribute 1,200 lines of noise.
const MAX_OBSERVED_MARKERS: usize = 50;

/// How a touch came to be known — see the module docs for why this is load
/// bearing rather than bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A CRUD tool named this path in its input.
    Declared,
    /// The workspace probe saw the path change across an opaque call.
    Observed,
}

impl Provenance {
    /// Classify by the `tool` label `ToolRegistry::record_touch` already
    /// carries.
    ///
    /// Defaults to [`Self::Declared`] for anything unrecognized, because every
    /// route into `record_touch` except the probe *is* a tool that named its
    /// path. A future emitter that observes rather than declares must be added
    /// here — and the failure mode of forgetting is over-reporting authorship
    /// for one tool, which is visible in the rendered diff, rather than
    /// dropping a real change silently.
    #[must_use]
    pub fn for_tool(tool: &str) -> Self {
        if tool == crate::shell_touch::PROBE_TOOL_LABEL {
            Self::Observed
        } else {
            Self::Declared
        }
    }
}

/// One path's cumulative story across the session.
#[derive(Debug, Clone)]
struct Entry {
    /// The op of the FIRST touch — decides whether the left side of the render
    /// is the file or `/dev/null`.
    first_op: FileOp,
    /// The op of the LATEST touch — decides the right side the same way.
    last_op: FileOp,
    provenance: Provenance,
    /// Content before the first touch. `None` when it was never measurable.
    origin: Option<String>,
    /// Content after the latest touch. `None` when it was never measurable.
    latest: Option<String>,
    /// Set when either image was too large to hold, so the render can say so
    /// instead of implying the file is empty.
    oversized: bool,
}

impl Entry {
    /// Whether this entry can render a real hunk, as opposed to a marker.
    fn renderable(&self) -> bool {
        self.provenance == Provenance::Declared && !self.oversized
    }

    /// The net line delta, computed from the SAME pair the hunk renders from.
    fn counts(&self) -> (u64, u64) {
        match (&self.origin, &self.latest) {
            (Some(origin), Some(latest)) => line_diff(origin, latest),
            _ => (0, 0),
        }
    }
}

/// What one render produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthoredDiff {
    /// Unified-diff text for declared changes, plus bounded markers for the
    /// rest. Empty when the session declared nothing.
    pub text: String,
    /// Net lines changed by **declared** touches only. Observed paths are
    /// deliberately excluded: this number feeds a size budget, and a tarball
    /// must not be able to spend it.
    pub lines: u32,
    /// Declared paths rendered.
    pub declared_files: usize,
    /// Observed paths seen (whether or not a marker was rendered for each).
    pub observed_files: usize,
}

impl AuthoredDiff {
    /// Whether this carries anything a reader could act on.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// Per-session ledger of authored changes, folded by path.
#[derive(Debug, Clone, Default)]
pub struct AuthoredDiffLedger {
    entries: BTreeMap<String, Entry>,
    /// Retained entries whose provenance is still [`Provenance::Observed`].
    /// Carried as a counter rather than recounted per call: `record` runs on
    /// every write in the session and needs this on every insert.
    observed: usize,
    /// Observed paths refused for [`MAX_TRACKED_OBSERVED`]. Counted even
    /// though they are not held — declining to retain a path is not the same
    /// as failing to notice it, and [`AuthoredDiff::observed_files`] promises
    /// the count regardless of whether a marker was rendered.
    observed_dropped: usize,
    /// Paths dropped for exceeding [`MAX_TRACKED_PATHS`]. Reported, never
    /// hidden — a bound that goes unmentioned reads as a complete answer.
    dropped: usize,
}

impl AuthoredDiffLedger {
    /// Fold one touch into the ledger.
    ///
    /// `pre`/`post` are the two images `record_touch` already computed for its
    /// own event; passing them here costs nothing beyond the retention bound
    /// above and is what makes a cumulative render possible.
    ///
    /// Reads are rejected here rather than at the call site, deliberately. A
    /// read is not a change, and the caller that forgets to filter one is the
    /// bug this guard exists to survive — the ledger must not depend on every
    /// future emitter remembering.
    pub fn record(
        &mut self,
        path: &str,
        op: FileOp,
        provenance: Provenance,
        pre: &str,
        post: &str,
    ) {
        if matches!(op, FileOp::Read) {
            return;
        }
        let oversized = pre.len() > MAX_RETAINED_BYTES || post.len() > MAX_RETAINED_BYTES;
        if let Some(entry) = self.entries.get_mut(path) {
            entry.last_op = op;
            entry.latest = (!oversized).then(|| post.to_string());
            entry.oversized |= oversized;
            // A path a CRUD tool declared stays declared even if the probe
            // later observes it again: the probe re-reports every declared
            // write it can see, and letting that downgrade the provenance
            // would erase authorship from exactly the files that have it.
            if provenance == Provenance::Declared && entry.provenance == Provenance::Observed {
                entry.provenance = Provenance::Declared;
                // It has moved off the observed budget onto the declared one.
                // Reachable at most once per path — the guard above is false
                // ever after — so the counter cannot drift or underflow.
                self.observed -= 1;
            }
            return;
        }
        // Observations are bounded first and separately, so a burst of them
        // cannot spend the slots authorship needs. This ceiling is reached on
        // any turn that builds, which is why it cannot be the shared one.
        if provenance == Provenance::Observed && self.observed >= MAX_TRACKED_OBSERVED {
            self.observed_dropped += 1;
            return;
        }
        if self.entries.len() >= MAX_TRACKED_PATHS {
            self.dropped += 1;
            return;
        }
        if provenance == Provenance::Observed {
            self.observed += 1;
        }
        self.entries.insert(
            path.to_string(),
            Entry {
                first_op: op,
                last_op: op,
                provenance,
                origin: (!oversized).then(|| pre.to_string()),
                latest: (!oversized).then(|| post.to_string()),
                oversized,
            },
        );
    }

    /// Render the session's authored change.
    ///
    /// Declared paths render as a real unified diff — `---`/`+++` headers and
    /// `@@` hunks — because every downstream consumer already parses that
    /// shape. That is not a formatting preference: `verify::mutation`'s mutant
    /// builder requires a `+++` header before it will emit anything, and the
    /// warrant reads the same headers for its path rules. A bespoke format
    /// here would leave both no-ops on precisely the workspaces this module
    /// exists to serve.
    #[must_use]
    pub fn render(&self) -> AuthoredDiff {
        let mut out = AuthoredDiff::default();
        let mut text = String::new();
        let mut lines_added = 0u64;
        let mut lines_removed = 0u64;
        let mut observed_rendered = 0usize;
        // Paths refused by [`MAX_TRACKED_OBSERVED`] were never held, so the
        // loop below cannot see them. They are unlisted for the same reason as
        // the ones it elides, and are reported in the same sentence: a reader
        // counting "listed plus unlisted" must arrive at every path observed.
        let mut observed_elided = self.observed_dropped;
        let mut truncated = 0usize;

        for (path, entry) in &self.entries {
            if entry.provenance == Provenance::Observed {
                out.observed_files += 1;
            }
            if text.len() >= MAX_RENDER_BYTES {
                truncated += 1;
                continue;
            }
            if !entry.renderable() {
                // Everything that is not a declared, measurable change gets a
                // marker: the path is real evidence even when its content is
                // not ours to claim or too large to carry.
                if observed_rendered >= MAX_OBSERVED_MARKERS {
                    observed_elided += 1;
                    continue;
                }
                let (added, _) = entry.counts();
                text.push_str(&marker_line(path, added));
                text.push('\n');
                observed_rendered += 1;
                continue;
            }
            let (origin, latest) = match (&entry.origin, &entry.latest) {
                (Some(origin), Some(latest)) => (origin, latest),
                _ => continue,
            };
            // A file written back byte-identical is not a change, whatever the
            // ledger recorded on the way there.
            let Some(hunk) = changed_region_diff(origin, latest) else {
                continue;
            };
            let (added, removed) = line_diff(origin, latest);
            lines_added += added;
            lines_removed += removed;
            out.declared_files += 1;
            // `/dev/null` on the side that did not exist, exactly as git renders
            // it — and exactly what the mutant builder keys on to skip a
            // deletion rather than trying to mutate lines that are gone.
            let left = if matches!(entry.first_op, FileOp::Create) {
                "/dev/null".to_string()
            } else {
                format!("a/{path}")
            };
            let right = if matches!(entry.last_op, FileOp::Delete) {
                "/dev/null".to_string()
            } else {
                format!("b/{path}")
            };
            text.push_str(&format!("--- {left}\n+++ {right}\n{hunk}"));
        }

        // Retention is bounded; noticing is not. A path refused for the
        // observed ceiling still changed, and still counts as one.
        out.observed_files += self.observed_dropped;

        // Every bound this render hit, stated. A truncated diff that does not
        // say it was truncated is read as a complete one, and a judge acting on
        // that is the failure this whole channel exists to prevent.
        if observed_elided > 0 {
            text.push_str(&format!(
                " … and {observed_elided} further path(s) changed by opaque calls, not listed\n"
            ));
        }
        if truncated > 0 {
            text.push_str(&format!(
                " … and {truncated} further changed file(s) omitted — the rendered diff hit its \
                 {MAX_RENDER_BYTES}-byte ceiling\n"
            ));
        }
        if self.dropped > 0 {
            text.push_str(&format!(
                " … and {} further path(s) not tracked — the ledger hit its {MAX_TRACKED_PATHS}-path \
                 ceiling\n",
                self.dropped
            ));
        }

        out.lines = u32::try_from(lines_added + lines_removed).unwrap_or(u32::MAX);
        out.text = text;
        out
    }
}

/// One marker line, in the shape the pipeline's warrant already parses.
fn marker_line(path: &str, added_lines: u64) -> String {
    format!("{MARKER_PREFIX}{path} (+{added_lines} lines)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> AuthoredDiffLedger {
        AuthoredDiffLedger::default()
    }

    /// The coupling this module cannot express in the type system: the marker
    /// shape is mirrored from the pipeline's warrant, which parses it back out.
    /// If that format moves, this test is the thing that notices.
    #[test]
    fn marker_line_matches_the_pipeline_contract() {
        assert_eq!(
            marker_line("src/main.rs", 12),
            "+ untracked change: src/main.rs (+12 lines)"
        );
    }

    /// The defect this module exists for: a created file reaches the render
    /// with its CONTENT, not merely its name and length.
    #[test]
    fn a_declared_create_renders_its_content() {
        let mut led = ledger();
        led.record(
            "solution.py",
            FileOp::Create,
            Provenance::Declared,
            "",
            "print(1)\nprint(2)\n",
        );
        let out = led.render();
        assert!(out.text.contains("+++ b/solution.py"), "{}", out.text);
        assert!(out.text.contains("--- /dev/null"), "{}", out.text);
        assert!(out.text.contains("+print(1)"), "{}", out.text);
        assert!(out.text.contains("+print(2)"), "{}", out.text);
        assert_eq!(out.lines, 2);
        assert_eq!(out.declared_files, 1);
    }

    /// The render must be parseable by the mutant builder, which is a no-op
    /// without a `+++` header. This is the shape contract, asserted directly.
    #[test]
    fn a_declared_render_carries_a_header_before_its_hunk() {
        let mut led = ledger();
        led.record(
            "a.rs",
            FileOp::Create,
            Provenance::Declared,
            "",
            "let x=1;\n",
        );
        let text = led.render().text;
        let header = text.find("+++ ").expect("a header");
        let hunk = text.find("@@").expect("a hunk");
        assert!(header < hunk, "header must precede its hunk: {text}");
    }

    /// Ten edits to one file are one net change, not ten — and not just the
    /// last one.
    #[test]
    fn repeated_edits_fold_into_one_cumulative_hunk() {
        let mut led = ledger();
        led.record("f.txt", FileOp::Create, Provenance::Declared, "", "v1\n");
        led.record(
            "f.txt",
            FileOp::Update,
            Provenance::Declared,
            "v1\n",
            "v2\n",
        );
        led.record(
            "f.txt",
            FileOp::Update,
            Provenance::Declared,
            "v2\n",
            "v3\n",
        );
        let out = led.render();
        assert_eq!(out.declared_files, 1, "one file, one entry");
        assert!(out.text.contains("+v3"), "the net post-image: {}", out.text);
        assert!(
            !out.text.contains("+v2"),
            "intermediate states are not the net change: {}",
            out.text
        );
        assert!(
            out.text.contains("--- /dev/null"),
            "created first, so the left side never existed: {}",
            out.text
        );
    }

    /// The SQLite-extraction case: observed paths are named and counted, never
    /// rendered as authored content.
    #[test]
    fn observed_paths_render_as_markers_not_content() {
        let mut led = ledger();
        led.record(
            "sqlite/test/tkt1435.test",
            FileOp::Create,
            Provenance::Observed,
            "",
            "a\nb\nc\n",
        );
        let out = led.render();
        assert!(
            out.text
                .contains("+ untracked change: sqlite/test/tkt1435.test"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("+a"), "no content: {}", out.text);
        assert_eq!(out.observed_files, 1);
        assert_eq!(out.declared_files, 0);
        assert_eq!(out.lines, 0, "an observed path must not spend the budget");
    }

    /// A bulk materialization is bounded and says how much it withheld.
    #[test]
    fn a_bulk_observation_is_bounded_and_reports_the_bound() {
        let mut led = ledger();
        for i in 0..300 {
            led.record(
                &format!("vendor/f{i:04}.c"),
                FileOp::Create,
                Provenance::Observed,
                "",
                "x\n",
            );
        }
        let out = led.render();
        let markers = out
            .text
            .lines()
            .filter(|l| l.starts_with(MARKER_PREFIX))
            .count();
        assert_eq!(markers, MAX_OBSERVED_MARKERS);
        assert!(
            out.text.contains("further path(s) changed by opaque calls"),
            "the bound is stated: {}",
            out.text
        );
        assert_eq!(out.observed_files, 300, "all of them are still counted");
        assert_eq!(out.lines, 0);
    }

    /// A declared write that the probe re-observes stays declared. Otherwise
    /// the turn-end probe would strip authorship from every file the agent
    /// actually wrote.
    #[test]
    fn a_probe_re_observation_cannot_downgrade_a_declared_path() {
        let mut led = ledger();
        led.record("fix.rs", FileOp::Create, Provenance::Declared, "", "ok\n");
        led.record(
            "fix.rs",
            FileOp::Update,
            Provenance::Observed,
            "ok\n",
            "ok\n",
        );
        let out = led.render();
        assert_eq!(out.declared_files, 1, "still authored: {}", out.text);
    }

    /// Reads are not changes, and the ledger refuses them itself rather than
    /// trusting every caller to filter.
    #[test]
    fn reads_are_refused_at_the_ledger() {
        let mut led = ledger();
        led.record("seen.rs", FileOp::Read, Provenance::Declared, "x\n", "x\n");
        assert!(led.render().is_empty());
    }

    /// A file written back byte-identical is not a change.
    #[test]
    fn an_identical_rewrite_renders_nothing() {
        let mut led = ledger();
        led.record(
            "same.rs",
            FileOp::Update,
            Provenance::Declared,
            "x\n",
            "x\n",
        );
        assert!(led.render().is_empty());
    }

    /// A deletion renders what went, and points its right side at `/dev/null`
    /// so the mutant builder skips it instead of mutating absent lines.
    #[test]
    fn a_delete_renders_to_dev_null() {
        let mut led = ledger();
        led.record(
            "gone.rs",
            FileOp::Delete,
            Provenance::Declared,
            "a\nb\n",
            "",
        );
        let out = led.render();
        assert!(out.text.contains("--- a/gone.rs"), "{}", out.text);
        assert!(out.text.contains("+++ /dev/null"), "{}", out.text);
        assert!(out.text.contains("-a"), "{}", out.text);
        assert_eq!(out.lines, 2);
    }

    /// An oversized file is recorded as a marker, never as silence and never
    /// as an empty-file rewrite.
    #[test]
    fn an_oversized_file_degrades_to_a_marker() {
        let mut led = ledger();
        let big = "x\n".repeat(MAX_RETAINED_BYTES);
        led.record("huge.bin", FileOp::Create, Provenance::Declared, "", &big);
        let out = led.render();
        assert!(
            out.text.contains("+ untracked change: huge.bin"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("+x"), "content withheld: {}", out.text);
    }

    /// The path ceiling is a bound that gets reported, not a silent cap.
    #[test]
    fn the_path_ceiling_is_reported() {
        let mut led = ledger();
        for i in 0..(MAX_TRACKED_PATHS + 10) {
            led.record(
                &format!("f{i:05}.rs"),
                FileOp::Create,
                Provenance::Declared,
                "",
                "x\n",
            );
        }
        let out = led.render();
        assert!(
            out.text.contains("further path(s) not tracked"),
            "the bound is stated: {}",
            out.text
        );
    }

    /// The failure the path budget used to have: opaque calls fill the ledger,
    /// and the write the agent actually declared arrives after them.
    ///
    /// This is not a contrived ordering. The probe descends into `target/`, so
    /// a turn that builds before it edits produces exactly this — thousands of
    /// observed artifacts, then the fix. The fix is the whole product of this
    /// channel and must survive any volume of things nobody authored.
    #[test]
    fn an_observed_flood_cannot_crowd_out_a_later_declared_write() {
        let mut led = ledger();
        for i in 0..(MAX_TRACKED_PATHS * 4) {
            led.record(
                &format!("target/debug/deps/dep{i:05}.rmeta"),
                FileOp::Create,
                Provenance::Observed,
                "",
                "binary\n",
            );
        }
        led.record(
            "src/fix.rs",
            FileOp::Create,
            Provenance::Declared,
            "",
            "let fixed = true;\n",
        );
        let out = led.render();
        assert_eq!(
            out.declared_files, 1,
            "the fix survived the flood: {}",
            out.text
        );
        assert!(
            out.text.contains("+let fixed = true;"),
            "with its content, not as a counter: {}",
            out.text
        );
        assert_eq!(out.lines, 1, "and only the fix spent the line budget");
    }

    /// Retaining fewer observed paths must not mean knowing about fewer. Every
    /// one is still counted and still accounted for in the render.
    #[test]
    fn observed_paths_past_their_ceiling_are_still_counted() {
        let mut led = ledger();
        let total = MAX_TRACKED_OBSERVED + 200;
        for i in 0..total {
            led.record(
                &format!("vendor/f{i:05}.c"),
                FileOp::Create,
                Provenance::Observed,
                "",
                "x\n",
            );
        }
        let out = led.render();
        assert_eq!(
            out.observed_files, total,
            "every one is counted: {}",
            out.text
        );
        let markers = out
            .text
            .lines()
            .filter(|l| l.starts_with(MARKER_PREFIX))
            .count();
        assert_eq!(markers, MAX_OBSERVED_MARKERS);
        assert!(
            out.text.contains(&format!(
                "{} further path(s) changed by opaque calls",
                total - MAX_OBSERVED_MARKERS
            )),
            "listed plus unlisted accounts for all of them: {}",
            out.text
        );
    }

    /// A path the probe observed and a CRUD tool then declares moves off the
    /// observed budget onto the declared one. Without the hand-back, a session
    /// that edits what it built leaks observed slots it no longer occupies.
    #[test]
    fn a_declared_upgrade_hands_back_its_observed_slot() {
        let mut led = ledger();
        for i in 0..MAX_TRACKED_OBSERVED {
            led.record(
                &format!("built/f{i:05}.o"),
                FileOp::Create,
                Provenance::Observed,
                "",
                "x\n",
            );
        }
        // Full: the next observed path is counted but not held.
        led.record(
            "built/extra.o",
            FileOp::Create,
            Provenance::Observed,
            "",
            "x\n",
        );
        assert!(
            !led.render().text.contains("built/extra.o"),
            "refused while the observed budget is full"
        );
        // Declaring one of the held paths returns its slot to the budget.
        led.record(
            "built/f00000.o",
            FileOp::Update,
            Provenance::Declared,
            "x\n",
            "y\n",
        );
        led.record(
            "built/extra.o",
            FileOp::Create,
            Provenance::Observed,
            "",
            "x\n",
        );
        let out = led.render();
        assert!(
            out.text.contains("+ untracked change: built/extra.o"),
            "the freed slot took the next observed path: {}",
            out.text
        );
    }

    /// The probe's label is the one thing that makes a touch observed.
    #[test]
    fn provenance_is_read_from_the_tool_label() {
        assert_eq!(
            Provenance::for_tool(crate::shell_touch::PROBE_TOOL_LABEL),
            Provenance::Observed
        );
        assert_eq!(Provenance::for_tool("write_file"), Provenance::Declared);
        assert_eq!(Provenance::for_tool("edit_file"), Provenance::Declared);
    }
}
