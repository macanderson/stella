// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The JSON shape a diff-serving payload carries, so every surface that draws
//! one reads the same three fields.
//!
//! Behind the off-by-default `json` feature: a caller that wants only the
//! differ still links nothing (see the crate's manifest). It exists because
//! three producers — the Observatory's context-diff route, the per-turn diff
//! projection, and the exported dashboard — were each about to hand-write the
//! same hunk serializer for the same renderer, which is precisely the
//! acknowledged copy this crate was extracted to end (#1511).
//!
//! # The three fields, and why the elision is two of them
//!
//! `hunks` alone cannot express "and there is more": a renderer walking line
//! numbers from each `@@` header has no way to know a gap exists, let alone
//! how big it is or where. So a payload carries the hunks
//! [`crate::view::elide`] kept, the count it dropped, and the index that count
//! belongs in front of. A consumer that ignores the latter two still renders a
//! valid — merely shorter — diff, which is the right failure mode for a field
//! an older dashboard has never heard of.

use serde_json::{Value, json};

use crate::Hunk;
use crate::view::Elided;

/// One hunk as `{old_start, old_count, new_start, new_count, lines[]}`, each
/// line as `{op, text}` with [`crate::Op::tag`]'s stable lowercase tag.
#[must_use]
pub fn hunk(hunk: &Hunk) -> Value {
    json!({
        "old_start": hunk.old_start,
        "old_count": hunk.old_count,
        "new_start": hunk.new_start,
        "new_count": hunk.new_count,
        "lines": hunk.lines.iter().map(|l| json!({
            "op": l.op.tag(),
            "text": l.text,
        })).collect::<Vec<_>>(),
    })
}

/// Every hunk of a diff, in document order.
#[must_use]
pub fn hunks(hunks: &[Hunk]) -> Value {
    Value::Array(hunks.iter().map(hunk).collect())
}

/// A capped view as the object a payload merges in: `hunks`, plus `elided`
/// and `fold_before` describing the one gap.
///
/// `fold_before` is `null` when nothing was elided, so a renderer never has to
/// treat `0` as "no fold" — index `0` is a real and reachable position (a view
/// that kept only a tail folds in front of its first hunk).
#[must_use]
pub fn view(view: &Elided) -> Value {
    json!({
        "hunks": hunks(&view.hunks),
        "elided": view.hidden,
        "fold_before": view.fold_before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiffLine, Op};

    fn sample() -> Hunk {
        Hunk {
            old_start: 4,
            old_count: 2,
            new_start: 4,
            new_count: 2,
            lines: vec![
                DiffLine {
                    op: Op::Equal,
                    text: "ctx".into(),
                },
                DiffLine {
                    op: Op::Remove,
                    text: "old".into(),
                },
                DiffLine {
                    op: Op::Add,
                    text: "new".into(),
                },
            ],
        }
    }

    #[test]
    fn a_hunk_serializes_to_the_shape_the_dashboard_reads() {
        let v = hunk(&sample());
        assert_eq!(v["old_start"], 4);
        assert_eq!(v["new_count"], 2);
        assert_eq!(v["lines"][0]["op"], "equal");
        assert_eq!(v["lines"][1]["op"], "remove");
        assert_eq!(v["lines"][1]["text"], "old");
        assert_eq!(v["lines"][2]["op"], "add");
    }

    #[test]
    fn an_unelided_view_reports_no_fold_rather_than_index_zero() {
        // `0` is a reachable fold position, so "nothing elided" has to be a
        // different value entirely or a renderer will draw a phantom marker
        // above the first hunk of every complete diff.
        let v = view(&crate::view::elide(&[sample()], 100));
        assert_eq!(v["elided"], 0);
        assert!(v["fold_before"].is_null(), "{v}");
        assert_eq!(v["hunks"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn an_elided_view_reports_the_count_and_where_it_belongs() {
        let big = Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 60,
            lines: (1..=60)
                .map(|i| DiffLine {
                    op: Op::Add,
                    text: format!("line{i}"),
                })
                .collect(),
        };
        let v = view(&crate::view::elide(&[big], 10));
        assert_eq!(v["elided"], 51);
        assert_eq!(v["fold_before"], 1, "between the head and the tail: {v}");
        assert_eq!(v["hunks"].as_array().map(Vec::len), Some(2));
    }
}
