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
