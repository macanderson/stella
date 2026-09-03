//! Mirrors a local dispatch claim onto the issue tracker.
//!
//! [`super::claim`] closes the race between two `stella self-driving drive`
//! processes sharing one clone. It does this with a lease, kept in
//! `.stella/private/fleet.db`. That lease works for two local processes.
//! It does not help a human. A human deciding what to hand out next
//! cannot see the lease without opening the ledger by hand. Neither can a
//! session on a second clone.
//!
//! [`MirroredLease`] wraps a granted [`super::claim::Lease`] and posts one
//! comment on the tracker when the lease is granted, and a second when it
//! is given back. A human reading the issue then sees the same two events
//! a peer reading the ledger would see.
//!
//! # A mirror, never an arbiter
//!
//! GitHub has no compare-and-set. So nothing here decides a claim. The
//! lease still does, exactly as `lease.rs` describes. A comment that fails
//! to post — `gh` offline, no login, a rate limit — is dropped rather than
//! treated as a reason to stop working the issue. [`super::claim::acquire`]
//! already takes this same stance for the ledger side. The lease this
//! session holds stays the same lease whether or not GitHub heard about
//! it.
//!
//! The mirror is a snapshot, not a live view. The claimed comment states
//! the lease's own TTL. It does not promise to stay current. So a reader
//! who finds it later can tell it is stale on sight, with no second look
//! at the issue's history needed. Posting on every heartbeat would turn a
//! lease renewed every couple of minutes into comment spam. The released
//! comment is what actually closes the loop for a human watching.

use stella_protocol::issue::{IssueKey, IssueProvider};

use super::backlog;
use super::claim::Lease;

/// A granted dispatch lease, plus its tracker-visible trail.
///
/// Wraps [`Lease`] rather than replacing it. Dropping this drops the lease
/// at the exact moment it always dropped. So every release site already in
/// the codebase — the end of a scope, an early `continue`, a bulk `drop` —
/// mirrors for free. See the module docs for why the mirroring half is
/// best-effort.
pub(super) struct MirroredLease<'p> {
    lease: Lease,
    provider: &'p dyn IssueProvider,
    key: IssueKey,
    owner: String,
    signature: String,
}

impl<'p> MirroredLease<'p> {
    /// Wrap a just-granted lease and post the claim comment.
    ///
    /// `number` is the bare issue number the lease was taken under — what
    /// [`super::claim::acquire`] was called with. It is not the ledger's
    /// `issue:<n>` claim key. The tracker has never heard that spelling.
    pub(super) fn new(
        lease: Lease,
        provider: &'p dyn IssueProvider,
        number: &str,
        signature: &str,
    ) -> Self {
        let dispatch = lease.dispatch_lease().clone();
        let key = IssueKey::from(number);
        post(
            provider,
            &key,
            &claimed_body(&dispatch.owner, dispatch.ttl_ms),
            signature,
        );
        Self {
            lease,
            provider,
            key,
            owner: dispatch.owner,
            signature: signature.to_owned(),
        }
    }
}

impl Drop for MirroredLease<'_> {
    fn drop(&mut self) {
        post(
            self.provider,
            &self.key,
            &released_body(&self.owner),
            &self.signature,
        );
    }
}

/// The comment posted the moment a lease is granted.
fn claimed_body(owner: &str, ttl_ms: u64) -> String {
    let minutes = ttl_ms.div_ceil(60_000).max(1);
    format!(
        "\u{1F512} Claimed by `{owner}`.\n\n\
         This is a local dispatch lease, not a GitHub assignment. It lasts \
         about {minutes} minute(s) and renews on its own while the run \
         stays alive. If this comment is older than that with no new \
         activity below, the lease has almost certainly lapsed and the \
         issue is free again.\n\n\
         Run `stella fleet claims --all` in the workspace for live status."
    )
}

/// The comment posted the moment a lease is given up.
fn released_body(owner: &str) -> String {
    format!("\u{1F513} Released by `{owner}`. Free for the next claimant.")
}

/// Post one mirror comment, and drop whatever goes wrong.
///
/// See the module docs: nothing here may turn a tracker hiccup into a
/// reason to stop working the issue. A failure here has no caller left to
/// report it to.
fn post(provider: &dyn IssueProvider, key: &IssueKey, body: &str, signature: &str) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let _ = runtime.block_on(backlog::comment(provider, key, body, signature));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueDraft, IssueError};

    use super::*;

    /// A tracker that only records what it was told. The witness below
    /// checks the two comments a mirrored lease posts against this record,
    /// with no real network call.
    #[derive(Default)]
    struct RecordingProvider {
        posted: Mutex<Vec<(String, String)>>,
    }

    impl RecordingProvider {
        fn posted(&self) -> Vec<(String, String)> {
            self.posted.lock().expect("fixture lock").clone()
        }
    }

    #[async_trait]
    impl IssueProvider for RecordingProvider {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(Vec::new())
        }

        async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            Ok(IssueKey::from("1"))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn comment(&self, key: &IssueKey, body: &str) -> Result<(), IssueError> {
            self.posted
                .lock()
                .expect("fixture lock")
                .push((key.as_str().to_owned(), body.to_owned()));
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }
    }

    fn granted(root: &std::path::Path, number: &str, owner: &str) -> Lease {
        match super::super::claim::acquire_as(root, number, owner) {
            super::super::claim::Claim::Granted(lease) => lease,
            other => panic!("the key was free, so the claim must be granted: {other:?}"),
        }
    }

    /// **The witness.** Wrapping a granted lease posts the claim comment
    /// right away. Dropping the wrapper posts a second comment giving it
    /// back. Those are the two events a human needs to see on the issue —
    /// the same two events the local ledger already knows.
    ///
    /// Fails on the parent commit by construction: `MirroredLease` does not
    /// exist there, so nothing posts either comment.
    #[test]
    fn a_lease_posts_a_claim_comment_and_a_release_comment_around_its_life() {
        let root = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::default();
        let lease = granted(root.path(), "1675", "self-driving:4242");

        let mirrored = MirroredLease::new(lease, &provider, "1675", "");
        // `claim::acquire_as` grants under `claim::LEASE_TTL`. That
        // constant is private to its own module, so its value — five
        // minutes — is spelled out here in milliseconds instead.
        let five_minutes_ms = 5 * 60 * 1000;
        assert_eq!(
            provider.posted(),
            vec![(
                "1675".to_owned(),
                claimed_body("self-driving:4242", five_minutes_ms)
            )],
            "granting the wrapper posts the claim comment right away"
        );

        drop(mirrored);
        let posted = provider.posted();
        assert_eq!(
            posted.len(),
            2,
            "dropping it posts exactly one more comment"
        );
        assert_eq!(posted[1].0, "1675");
        assert_eq!(posted[1].1, released_body("self-driving:4242"));
    }

    /// The claimed comment names the owner, a whole number of minutes, and
    /// the command a human can run for live status. It has to name all
    /// three, or a reader has no way to judge whether the claim is still
    /// good.
    #[test]
    fn the_claim_comment_names_the_owner_and_a_whole_minute_ttl() {
        let body = claimed_body("self-driving:1", 5 * 60_000);
        assert!(body.contains("self-driving:1"));
        assert!(body.contains("5 minute"));
        assert!(body.contains("stella fleet claims --all"));
    }

    /// A TTL under a minute still reads as at least one minute, not zero.
    /// "Expires in 0 minutes" would read as already gone.
    #[test]
    fn a_sub_minute_ttl_rounds_up_rather_than_down_to_zero() {
        assert!(claimed_body("owner", 1).contains("1 minute"));
    }

    /// The release comment names who released it, and nothing more. There
    /// is no lease state left to report once it is gone.
    #[test]
    fn the_release_comment_names_the_owner() {
        let body = released_body("self-driving:1");
        assert!(body.contains("self-driving:1"));
        assert!(body.contains("Released"));
    }
}
