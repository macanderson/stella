//! Which authority published a record.
//!
//! Everything else about a record is self-described: `origin`, `truth.basis`,
//! `verified_by` are fields *inside the file being judged*. That is fine for a
//! record you wrote, and worthless for one that arrived with a checkout — a
//! repository can assert `origin = "user"` about itself as easily as it can assert
//! anything else. So the one fact the record is not allowed to claim is where it
//! came from: [`Trust`] is stamped by the loader from **which directory the file
//! was read out of**, and nothing in the file can change it.
//!
//! # Why the default is the lower tier
//!
//! [`Trust::Project`] is [`Default`] on purpose. A caller that forgets to stamp
//! the tier gets the restrictive answer — a record that cannot self-approve
//! blocking and cannot displace a user record's enforcement. The failure mode of
//! defaulting the other way is a repository silently inheriting the user's
//! authority, which is the whole thing this type exists to prevent.
//!
//! # What it is not
//!
//! Not a permission to *steer*. Whether an untrusted checkout's records reach the
//! prompt at all is `stella-cli`'s `AuthorityPolicy` question, decided before any
//! file is read. `Trust` answers a narrower one, asked of files that are already
//! allowed to load: may this record *remove* enforcement another record
//! established, and may it approve itself for the tool boundary.

use super::super::ingest::record::{Record, TruthBasis};

/// The base rule both self-attestation gates share: the loader stamped
/// [`Trust::User`], and the record's own claim is a decree with a person's
/// name on it.
///
/// A record's fields cannot establish the authority they claim. `origin`,
/// `truth.basis` and `verified_by` are all written inside the file being
/// judged, so a checkout can set them as easily as it sets anything else. The
/// tier the loader stamps is the one fact the file cannot write, and a decree
/// only counts when somebody signed it.
///
/// Two gates ask this, and each adds one condition of its own:
///
/// - `super::bridge`'s `self_attested` adds `origin = "user"`, because a
///   record approving its own blocking guard must be the user's own record
///   and not a `system` one.
/// - `super::sweep`'s `honored_probe` adds a stamped origin that is neither
///   `imported` nor `inferred`, because a probe runs a command or reaches a
///   host and mined content must never arm one.
///
/// The extra conditions stay with their callers. Moving either one here would
/// tighten the other gate silently, which is the drift naming the shared half
/// exists to prevent.
pub(crate) fn decreed_by_a_named_human_at(record: &Record, trust: Trust) -> bool {
    if trust != Trust::User {
        return false;
    }
    record.truth.as_ref().is_some_and(|truth| {
        truth.basis == TruthBasis::Decree
            && truth
                .verified_by
                .as_deref()
                .is_some_and(|by| !by.trim().is_empty())
    })
}

/// The authority a record was published under.
///
/// Ordered: `Project < User`, so `>=` reads as "at least as trusted as".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum Trust {
    /// Published in the repository (`<repo>/.stella/rules`, `<repo>/.claude/rules`,
    /// or the workspace store). Anyone who can open a pull request can write one,
    /// so a project record may add enforcement and may never remove it.
    #[default]
    Project,
    /// Published in the user's own `~/.stella/rules`. Writing the file, naming
    /// yourself in it, and keeping it on your own disk *is* the approval — there is
    /// no second party to ceremony against.
    User,
}

impl Trust {
    /// The spelling used in refusals and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

impl std::fmt::Display for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_lower_tier() {
        assert_eq!(
            Trust::default(),
            Trust::Project,
            "a forgotten stamp must fail toward the restrictive answer"
        );
    }

    #[test]
    fn user_outranks_project() {
        assert!(Trust::User > Trust::Project);
    }
}
