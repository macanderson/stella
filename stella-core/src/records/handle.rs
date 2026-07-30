//! `^handle` — the name a record is cited by.
//!
//! A record the model cannot name is a record whose efficacy cannot be measured.
//! Before handles, `render_rules_section` emitted bare bullets, so
//! [`ContextUseKind::Cited`][cited] was structurally unreachable: the model had no
//! token to put in "I followed X", and the loop from *rendered* to *did it help*
//! stayed open. A handle closes it.
//!
//! [cited]: super::super::context_record::context_use::ContextUseKind::Cited
//!
//! # Why the lineage tail, and why it has to widen
//!
//! `ctx.acme.web.pkg-manager` reads best as `^pkg-manager`: short, memorable, and
//! what a person would say out loud. But two record sets can each end a lineage in
//! `pkg-manager` — `ctx.acme.web.pkg-manager` and `ctx.acme.api.pkg-manager` — and
//! a rendered block containing two `^pkg-manager` records makes every citation
//! ambiguous, which is worse than no handle at all: it produces attribution that
//! looks precise and is not.
//!
//! So handles widen only as far as they must. Every record starts at its last
//! lineage segment; any group that collides takes one more segment, and repeats.
//! `^web-pkg-manager` and `^api-pkg-manager` are the result — still readable, now
//! unambiguous, and the record that had no collision keeps its short name.
//!
//! # Determinism
//!
//! Widening is driven by content, never by input order: the final tiebreaker is
//! the content-derived `record_id`, so the same set of records produces the same
//! handles whichever order they were loaded in. That matters because the handles
//! appear in the byte-stable cached prompt prefix — a handle that depended on
//! directory iteration order would break the prompt cache on a rename.

use super::LoadedRecord;

/// How many characters of the record id disambiguate a handle that lineage
/// widening could not separate. Six hex characters over a set small enough to fit
/// in one prompt is not a collision risk; the alternative, an ordinal, would
/// depend on load order.
const TIEBREAK_LEN: usize = 6;

/// Assign every record a `^handle` that is unique across `records`.
///
/// Call this once over the whole loaded set — user records, repository records, and
/// store-published records together. A handle assigned per-file would be unique
/// within a file and ambiguous in the prompt, which is the only place it is used.
pub fn assign_handles(records: &mut [LoadedRecord]) {
    // Start every record at the narrowest handle: its last lineage segment.
    let mut depth: Vec<usize> = vec![1; records.len()];
    let segments: Vec<Vec<String>> = records
        .iter()
        .map(|loaded| lineage_segments(&loaded.record.lineage_id))
        .collect();

    // Widen colliding groups until no group collides, or the collision is one no
    // amount of lineage can separate.
    //
    // The second condition matters: two records sharing an entire lineage would
    // otherwise widen all the way to `acme-web-pkg-manager` and *still* need a
    // tiebreak, having spent every segment to no effect. Lineage is only spent when
    // spending it separates something.
    let full: Vec<String> = (0..records.len())
        .map(|i| handle_at(&segments[i], segments[i].len()))
        .collect();
    loop {
        let handles: Vec<String> = (0..records.len())
            .map(|i| handle_at(&segments[i], depth[i]))
            .collect();
        let mut widened = false;
        for i in 0..records.len() {
            let separable = (0..records.len())
                .any(|j| j != i && handles[j] == handles[i] && full[j] != full[i]);
            if separable && depth[i] < segments[i].len() {
                depth[i] += 1;
                widened = true;
            }
        }
        if !widened {
            break;
        }
    }

    // Anything still colliding shares its entire lineage, so no amount of lineage
    // separates it. Fall back to the content-derived id, which by construction
    // differs whenever the records differ.
    let handles: Vec<String> = (0..records.len())
        .map(|i| handle_at(&segments[i], depth[i]))
        .collect();
    for i in 0..records.len() {
        let collides = (0..records.len()).any(|j| j != i && handles[j] == handles[i]);
        let mut handle = handles[i].clone();
        if collides {
            handle = format!("{handle}-{}", tiebreak(&records[i]));
        }
        records[i].handle = handle;
    }
}

/// The lineage split into handle-ready segments, narrowest last.
///
/// `ctx.acme.web.pkg-manager` → `["acme", "web", "pkg-manager"]`. The `ctx.`
/// prefix is dropped: every lineage carries it, so it distinguishes nothing.
fn lineage_segments(lineage_id: &str) -> Vec<String> {
    let trimmed = lineage_id.strip_prefix("ctx.").unwrap_or(lineage_id);
    let segments: Vec<String> = trimmed
        .split('.')
        .map(slug)
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        vec!["record".to_string()]
    } else {
        segments
    }
}

/// The handle formed from the last `depth` segments, joined with dashes.
fn handle_at(segments: &[String], depth: usize) -> String {
    let start = segments.len().saturating_sub(depth.max(1));
    segments[start..].join("-")
}

/// A short, content-derived suffix for a handle lineage cannot separate.
fn tiebreak(loaded: &LoadedRecord) -> String {
    let id = loaded.record.record_id.as_deref().unwrap_or_default();
    let tail: String = id
        .chars()
        .rev()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(TIEBREAK_LEN)
        .collect();
    if tail.is_empty() {
        // No id to borrow from (a record whose canonicalization failed). The
        // statement is all that is left, and it is at least content.
        return slug(&loaded.record.statement)
            .chars()
            .take(TIEBREAK_LEN)
            .collect();
    }
    tail.chars().rev().collect()
}

/// Lowercase, `[a-z0-9-]`-only, no leading/trailing or doubled dashes.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            'a'..='z' | '0'..='9' => out.push(ch),
            'A'..='Z' => out.push(ch.to_ascii_lowercase()),
            _ if !out.ends_with('-') => out.push('-'),
            _ => {}
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::super::tests::record_named;
    use super::*;

    fn loaded(lineage: &str, set_id: &str) -> LoadedRecord {
        let mut record = record_named(lineage);
        record
            .stamp(&super::super::Defaults::default())
            .expect("stamps");
        LoadedRecord {
            record,
            set_id: set_id.to_string(),
            source: format!("{set_id}.toml"),
            handle: String::new(),
            findings: Vec::new(),
        }
    }

    fn handles(records: &[LoadedRecord]) -> Vec<&str> {
        records.iter().map(|r| r.handle.as_str()).collect()
    }

    #[test]
    fn a_lone_record_gets_the_short_readable_handle() {
        let mut records = vec![loaded("ctx.acme.web.pkg-manager", "acme.web")];
        assign_handles(&mut records);
        assert_eq!(handles(&records), vec!["pkg-manager"]);
    }

    #[test]
    fn colliding_tails_widen_and_non_colliding_ones_stay_short() {
        let mut records = vec![
            loaded("ctx.acme.web.pkg-manager", "acme.web"),
            loaded("ctx.acme.api.pkg-manager", "acme.api"),
            loaded("ctx.acme.web.node-version", "acme.web"),
        ];
        assign_handles(&mut records);
        assert_eq!(
            handles(&records),
            vec!["web-pkg-manager", "api-pkg-manager", "node-version"],
            "only the colliding pair pays for the collision"
        );
    }

    #[test]
    fn widening_repeats_until_the_ambiguity_is_gone() {
        // Both collide at depth 1 (`x`) AND at depth 2 (`b-x`); only depth 3
        // separates them.
        let mut records = vec![loaded("ctx.one.b.x", "s1"), loaded("ctx.two.b.x", "s2")];
        assign_handles(&mut records);
        assert_eq!(handles(&records), vec!["one-b-x", "two-b-x"]);
    }

    #[test]
    fn an_identical_lineage_falls_back_to_the_content_derived_id() {
        let mut records = vec![
            loaded("ctx.acme.web.pkg-manager", "acme.web"),
            LoadedRecord {
                record: {
                    let mut record = record_named("ctx.acme.web.pkg-manager");
                    record.statement = "A different statement entirely.".to_string();
                    record
                        .stamp(&super::super::Defaults::default())
                        .expect("stamps");
                    record
                },
                set_id: "acme.web".to_string(),
                source: "other.toml".to_string(),
                handle: String::new(),
                findings: Vec::new(),
            },
        ];
        assign_handles(&mut records);
        let assigned = handles(&records);
        assert_ne!(
            assigned[0], assigned[1],
            "two records sharing a whole lineage must still be separately citable"
        );
        assert!(assigned.iter().all(|h| h.starts_with("pkg-manager-")));
    }

    #[test]
    fn handles_do_not_depend_on_load_order() {
        let forward = {
            let mut records = vec![
                loaded("ctx.acme.web.pkg-manager", "acme.web"),
                loaded("ctx.acme.api.pkg-manager", "acme.api"),
            ];
            assign_handles(&mut records);
            records
                .iter()
                .map(|r| (r.record.lineage_id.clone(), r.handle.clone()))
                .collect::<Vec<_>>()
        };
        let reversed = {
            let mut records = vec![
                loaded("ctx.acme.api.pkg-manager", "acme.api"),
                loaded("ctx.acme.web.pkg-manager", "acme.web"),
            ];
            assign_handles(&mut records);
            let mut pairs = records
                .iter()
                .map(|r| (r.record.lineage_id.clone(), r.handle.clone()))
                .collect::<Vec<_>>();
            pairs.reverse();
            pairs
        };
        assert_eq!(
            forward, reversed,
            "handles ride the cached prompt prefix — load order must not change them"
        );
    }

    #[test]
    fn a_lineage_with_no_usable_segments_still_gets_a_handle() {
        let mut records = vec![loaded("ctx.", "s")];
        assign_handles(&mut records);
        assert_eq!(handles(&records), vec!["record"]);
    }

    #[test]
    fn slug_normalizes_to_the_handle_charset() {
        assert_eq!(slug("Pkg Manager"), "pkg-manager");
        assert_eq!(slug("--weird__name--"), "weird-name");
        assert_eq!(slug("API/v2"), "api-v2");
    }
}
