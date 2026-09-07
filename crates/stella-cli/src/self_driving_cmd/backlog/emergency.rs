//! Is this emergency already filed?
//!
//! The label on a `main-red` or `release-red` issue is the dedup key. It is
//! how a restarted loop, or a second one, finds the emergency already
//! filed. Without it, each pass files a new one. This is that read.
//!
//! Its own file because `backlog.rs` is at the size ceiling.

use stella_protocol::issue::IssueProvider;

use super::{BASE_BREAKAGE_LABEL, DEPLOY_BREAKAGE_LABEL, QUEUE_READ_LIMIT, read_filled_the_page};

/// What a read for an already-filed emergency found.
///
/// Three answers, not two. "Not in the page I read" is not "not filed". The
/// page is bounded. A backlog that fills it would make every emergency read
/// as unfiled, and each pass would file one more.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EmergencyRead {
    /// The tracker answered and one is open, under this key.
    Filed(String),
    /// The tracker answered a page with room left, and none is in it.
    NotFiled,
    /// Nothing usable came back: the tracker could not be read, or the read
    /// filled its page and the answer is not in what crossed.
    Unknown(&'static str),
}

impl EmergencyRead {
    /// The key of an emergency already filed, or `None`.
    ///
    /// `Unknown` answers `None`, so the loop files. Here is the cost of the
    /// other choice. Refuse to file, and the base branch stays broken with
    /// nothing on the tracker to say so. Every pull request stays red behind
    /// a hold that never fires. A duplicate is one issue somebody closes.
    /// The read says what it could not answer first, so the duplicate is
    /// explained.
    #[must_use]
    pub(crate) fn filed(self) -> Option<String> {
        match self {
            Self::Filed(key) => Some(key),
            Self::NotFiled => None,
            Self::Unknown(why) => {
                eprintln!(
                    "warning: could not tell whether this emergency is already filed ({why}), \
                     so it is being filed. Close the duplicate if there is one."
                );
                None
            }
        }
    }
}

/// Scan one read for an issue carrying `label`.
fn emergency_labelled(provider: &dyn IssueProvider, label: &str) -> EmergencyRead {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return EmergencyRead::Unknown("no runtime for the issue provider");
    };
    let Ok(issues) = runtime.block_on(provider.list_open(QUEUE_READ_LIMIT)) else {
        return EmergencyRead::Unknown("the tracker could not be read");
    };

    let filled = read_filled_the_page(issues.len());
    match issues
        .into_iter()
        .find(|issue| issue.labels.iter().any(|carried| carried.name == label))
    {
        Some(issue) => EmergencyRead::Filed(issue.key.as_str().to_owned()),
        None if filled => EmergencyRead::Unknown("the read filled its page"),
        None => EmergencyRead::NotFiled,
    }
}

/// The open base-breakage issue, if one exists.
///
/// Read through the port like everything else, and matched on
/// [`BASE_BREAKAGE_LABEL`] rather than on words in the title. An unreachable
/// tracker answers [`EmergencyRead::Unknown`], and so does a read that filled
/// its page — see that type for what the caller does with it.
#[must_use]
pub(crate) fn open_base_breakage(provider: &dyn IssueProvider) -> EmergencyRead {
    emergency_labelled(provider, BASE_BREAKAGE_LABEL)
}

/// The open deploy-breakage issue, if one exists.
///
/// Matched on [`DEPLOY_BREAKAGE_LABEL`] and read through the port. It
/// degrades as [`open_base_breakage`] does, and on the same terms.
#[must_use]
pub(crate) fn open_deploy_breakage(provider: &dyn IssueProvider) -> EmergencyRead {
    emergency_labelled(provider, DEPLOY_BREAKAGE_LABEL)
}
