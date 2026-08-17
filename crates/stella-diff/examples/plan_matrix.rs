//! Prints [`stella_diff::view::plan`]'s answer over a fixed matrix of
//! `(total, hunk size, cap)`, so the hand-port of the same policy in
//! `arenabench/ui/components/arena/transcript-page.tsx::selectDiffLines` can
//! be diffed against it.
//!
//! Two languages, no shared test runner, one policy that must not drift —
//! and the arena transcript is read beside the deck and the Observatory when
//! arguing about a run, so drift there means one edit looking like two.
//!
//! Deliberately an example rather than a gate step: it proves nothing about
//! the Rust on its own (the unit tests do that), and running it usefully
//! means running the TypeScript beside it, which needs a Node toolchain the
//! gate does not have. Run it by hand when either side changes.
//!
//! It has already earned itself once: the first diff disagreed, and the bug
//! was in the Rust — `Plan::fold_before` put the elision marker *after* the
//! only surviving hunk whenever the budget kept a tail and no head.
fn main() {
    for &(total, step) in &[
        (15usize, 3usize),
        (80, 4),
        (31, 31),
        (100, 7),
        (6, 3),
        (500, 500),
    ] {
        let starts: Vec<usize> = (0..total).step_by(step).collect();
        for cap in 0..=40 {
            let p = stella_diff::view::plan(&starts, total, cap);
            let spans: Vec<String> = p
                .shown
                .iter()
                .map(|r| format!("{}-{}", r.start, r.end))
                .collect();
            println!(
                "total={total} step={step} cap={cap} shown=[{}] hidden={} fold={:?}",
                spans.join(","),
                p.hidden,
                p.fold_before()
            );
        }
    }
}
