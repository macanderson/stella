// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `/models` role table — **an open vocabulary** (#3472).
//!
//! # What was closed, and where
//!
//! Not the type. [`RoleWiringRow::role`] has always been a `String`, so a
//! driver could always have sent a row for a role the deck had never heard of.
//! Two things closed it anyway, and both were prose rather than structure:
//! [`EngineConfigState::roles`] documented itself as "one row per role in
//! [`EngineRole::ALL`] order", and the dialog iterated its own fixed slot list
//! and looked each key *up*, so a row outside that list was silently dropped
//! on the floor — rendered nowhere, counted nowhere, reported nowhere.
//!
//! That is the shape a host cannot work around: a role contributed by
//! something the operator installed would resolve, run, and spend, and the one
//! surface that exists to answer "what will each role run" would not admit it
//! existed.
//!
//! # What is open now, and what stays closed
//!
//! [`role_table`] folds the driver's rows into render order: the roles the
//! deck knows first, in the caller's order, then every other row **in the
//! order the driver sent it**. A contributed role therefore appears because
//! the driver sent a row for it and disappears when the driver stops — which
//! is the whole contract, because the driver stops when whatever contributed
//! the role is removed.
//!
//! [`EngineRole`] stays closed and is deliberately untouched. It is not the
//! role table: it is the ENGINE overlay's *editing* vocabulary, one variant per
//! `agents.<key>` block the settings schema defines, and a role the settings
//! schema has never heard of has no block to edit. So a contributed row is
//! rendered, never edited — a strictly smaller claim than the built-in rows
//! make, and an honest one.
//!
//! # The deck still does not learn what a plugin is
//!
//! Nothing here names one. A row arrives pre-rendered, including the `from`
//! cell that says where it came from, and this module's only judgement is
//! *ordering* — the deck's own words for the roles it has words for, the
//! driver's key for the rest. Whether a role exists because of a plugin, a
//! settings block or a hard-coded default is a question the deck cannot ask
//! and does not need to.

use super::{EngineConfigState, RoleWiringRow};

/// One row of the rendered role table: the driver's row, and the word the deck
/// says out loud for it.
///
/// Borrowed rather than owned because this is a view over the snapshot the
/// deck already holds — a fold that copied the rows could be rendered while
/// the snapshot behind it had moved on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleTableEntry<'a> {
    /// The resolved wiring, exactly as the driver sent it.
    pub row: &'a RoleWiringRow,
    /// What to call this role on screen: the deck's own word when it has one
    /// (copy law D6), otherwise the role's own key.
    ///
    /// The fallback is the honest one available: the deck has no word for a
    /// role it has never heard of, and inventing one — "plugin", "other" —
    /// would name the row after a category instead of after itself.
    pub word: &'a str,
}

/// Every role row the driver sent, in table order.
///
/// `known` is the caller's `(role key, deck word)` list in display order — the
/// roles the deck has words for. Rows whose key is not in it follow, in the
/// order the driver sent them, each labelled by its own key.
///
/// Order matters more than it looks: the built-in prefix is a fixed reading
/// order someone has learned, so a contributed role must not be able to push
/// `work` and `verify` around by sorting ahead of them. Appending also makes
/// the driver's order the contributed order, which is the only ordering the
/// deck could honour without ranking contributors itself.
///
/// A key `known` names but the driver did not send is skipped rather than
/// filled in: the dialog reports resolution, and a row nobody resolved is not
/// a row. A key the driver sent twice is taken once — the first — because a
/// second row for one role is a driver defect, and rendering both would print
/// two answers to a question that has one.
#[must_use]
pub fn role_table<'a>(
    state: &'a EngineConfigState,
    known: &[(&'a str, &'a str)],
) -> Vec<RoleTableEntry<'a>> {
    let mut entries: Vec<RoleTableEntry<'a>> = known
        .iter()
        .filter_map(|&(key, word)| {
            let row = state.roles.iter().find(|row| row.role == key)?;
            Some(RoleTableEntry { row, word })
        })
        .collect();
    entries.extend(
        state
            .roles
            .iter()
            .filter(|row| !known.iter().any(|(key, _)| *key == row.role))
            .map(|row| RoleTableEntry {
                row,
                word: row.role.as_str(),
            }),
    );
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: [(&str, &str); 2] = [("default", "lead"), ("worker", "work")];

    fn row(role: &str) -> RoleWiringRow {
        RoleWiringRow {
            role: role.to_string(),
            model: format!("zai/glm-5.2 ({role})"),
            effort: "medium".to_string(),
            thinking: "thinking on".to_string(),
            source: "default_model".to_string(),
            next_session: None,
        }
    }

    fn state(roles: &[&str]) -> EngineConfigState {
        EngineConfigState {
            roles: roles.iter().map(|role| row(role)).collect(),
            ..Default::default()
        }
    }

    fn words(state: &EngineConfigState) -> Vec<&str> {
        role_table(state, &KNOWN)
            .into_iter()
            .map(|entry| entry.word)
            .collect()
    }

    /// **The #3472 witness.** A role the deck has never heard of appears in the
    /// table, and leaves when the driver stops sending it — which is what
    /// happens when whatever contributed it is removed.
    ///
    /// Both directions, because only the second one is required: a table
    /// that could admit a role but not lose one would keep printing a routing
    /// answer for something that no longer routes anywhere, which is the exact
    /// failure this dialog exists to prevent.
    #[test]
    fn a_contributed_role_appears_in_the_table_and_leaves_with_its_contributor() {
        let installed = state(&["default", "worker", "vera-witness"]);
        assert_eq!(words(&installed), vec!["lead", "work", "vera-witness"]);

        let removed = state(&["default", "worker"]);
        assert_eq!(
            words(&removed),
            vec!["lead", "work"],
            "a role whose contributor is gone must leave the table with it"
        );
    }

    /// The built-in prefix is a reading order someone has learned. A
    /// contributed role joins the end of it and cannot reorder it, whatever it
    /// is called or wherever in the snapshot it arrives.
    #[test]
    fn a_contributed_role_cannot_reorder_the_roles_the_deck_knows() {
        let state = state(&["aaa-first", "worker", "zzz-last", "default"]);
        assert_eq!(
            words(&state),
            vec!["lead", "work", "aaa-first", "zzz-last"],
            "known roles keep the caller's order; the rest keep the driver's"
        );
    }

    /// Copy law D6: for a role the deck has a word for, the deck's word wins
    /// over the settings key — `work`, not `worker`.
    #[test]
    fn the_deck_keeps_its_own_word_for_a_role_it_knows() {
        let state = state(&["worker"]);
        assert_eq!(words(&state), vec!["work"]);
    }

    /// A role nobody resolved is not invented, and a row sent twice is
    /// rendered once.
    #[test]
    fn the_table_holds_exactly_what_the_driver_sent() {
        assert!(words(&state(&[])).is_empty());
        assert_eq!(words(&state(&["worker"])), vec!["work"]);
        assert_eq!(words(&state(&["worker", "worker"])), vec!["work"]);
    }
}
