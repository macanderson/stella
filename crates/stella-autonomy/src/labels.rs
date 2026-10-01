//! Whether two label names name the same label.
//!
//! A tracker stores one spelling of a label. The code that asks about it may
//! hold another. GitHub ignores case when it adds a label or lists issues by
//! one, but each issue it returns holds the stored spelling. So an exact
//! test reads `AGENT-ESCALATED` and `agent-escalated` as two labels, and the
//! loop stops seeing its own marks. [`same`] is the one test every label
//! check in the loop goes through.
//!
//! [`RENAMED`] lets the old name of a renamed label match its new name. It
//! holds the move this repository made to the uppercase scheme (ADR 0046),
//! plus GitHub's stock `enhancement`. An operator's own words go in
//! `[self_driving.triage]`.

/// Old label names and the names that took their place, old first.
///
/// Each pair changes more than case. A pair that only changed case would
/// add nothing, since [`same`] does not look at case. GitHub gives each new
/// repository `bug`, `documentation`, and `enhancement` too, so a repository
/// that kept its stock labels still fits the default policy.
pub const RENAMED: &[(&str, &str)] = &[
    ("bug", "KIND:BUG"),
    ("feature", "KIND:FEATURE"),
    ("enhancement", "KIND:FEATURE"),
    ("chore", "KIND:CHORE"),
    ("tech-debt", "KIND:CHORE"),
    ("documentation", "KIND:DOCUMENTATION"),
    ("docs", "KIND:DOCUMENTATION"),
    ("size/XS", "SIZE:EXTRA-SMALL"),
    ("size/S", "SIZE:SMALL"),
    ("size/M", "SIZE:MEDIUM"),
    ("size/L", "SIZE:LARGE"),
    ("size/XL", "SIZE:EXTRA-LARGE"),
    ("use-model:cheap", "MODEL:T1"),
    ("use-model:balanced", "MODEL:T2"),
    ("use-model:pro", "MODEL:T3"),
    ("use-model:ultra", "MODEL:T4"),
    ("area:ocp", "AREA:CGP"),
];

/// The name a label goes by now: its new name if it is an old spelling in
/// [`RENAMED`], and itself otherwise.
#[must_use]
pub fn current(name: &str) -> &str {
    RENAMED
        .iter()
        .find(|(old, _)| old.eq_ignore_ascii_case(name))
        .map_or(name, |&(_, new)| new)
}

/// Whether `a` and `b` name the same label.
///
/// Case is ignored, and an old spelling matches the name that replaced it.
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    current(a).eq_ignore_ascii_case(current(b))
}

/// Whether a label belongs to the family a prefix names, such as `AREA:`.
///
/// Case is ignored, and an old spelling is judged by its new name, so
/// `size/M` is in the `SIZE:` family.
#[must_use]
pub fn in_family(name: &str, prefix: &str) -> bool {
    current(name)
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_does_not_tell_two_labels_apart() {
        assert!(same("AGENT-ESCALATED", "agent-escalated"));
        assert!(same("Status:Ready", "STATUS:READY"));
        assert!(!same("P1", "P2"));
    }

    #[test]
    fn an_old_spelling_matches_its_new_name_in_either_direction() {
        assert!(same("bug", "KIND:BUG"));
        assert!(same("KIND:BUG", "Bug"));
        assert!(same("size/xl", "SIZE:EXTRA-LARGE"));
        assert!(same("use-model:pro", "MODEL:T3"));
        assert!(same("area:ocp", "AREA:CGP"));
        assert!(same("tech-debt", "chore"), "both fold into KIND:CHORE");
        assert!(!same("bug", "KIND:FEATURE"));
    }

    #[test]
    fn a_family_is_read_through_the_new_name() {
        assert!(in_family("size/M", "SIZE:"));
        assert!(in_family("area:core", "AREA:"));
        assert!(in_family("AREA:CORE", "area:"));
        assert!(!in_family("P1", "SIZE:"));
        assert!(!in_family("S", "SIZE:"), "shorter than the prefix");
    }

    /// Each pair names two labels. A pair that only changed case would add
    /// nothing, since [`same`] does not look at case.
    #[test]
    fn every_rename_changes_more_than_case() {
        for (old, new) in RENAMED {
            assert!(!old.eq_ignore_ascii_case(new), "{old} -> {new}");
        }
    }
}
