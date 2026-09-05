//! Where the escalation record gets written.
//!
//! The record lives in the issue body. To write it, the loop has to send a
//! whole body. That is the only shape a tracker edit takes.
//!
//! So the body it sends must be the body the tracker holds right then. A body
//! read at the start of a turn is minutes old. Sending it back would wipe out
//! any note a person added while the turn ran.
//!
//! [`write`] reads the issue just before it writes. It builds the record from
//! that read. If the read fails it writes nothing. A body nobody can see is a
//! body nobody may replace.
//!
//! Then it reads once more. If the record is gone, a person saved their own
//! text in the gap. Theirs is the newer text, so the same record goes back on
//! top of it. Once only. The loop must not fight a person over one body.
//!
//! GitHub has no compare-and-set on an issue body. One round trip between the
//! read and the write is the floor. An edit saved inside that gap is lost.

use stella_autonomy::escalation::{self, EscalationReason, EscalationRecord};
use stella_protocol::issue::{IssueError, IssueKey, IssueProvider};

/// Stamp one escalation record into the issue's own current body.
///
/// `at` and `at_unix` are one moment in two spellings. One is for a person to
/// read. The other is for the cooldown math. Both come from the caller, so
/// these rules can be tested against a fixed clock.
pub(super) async fn write(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    reason: EscalationReason,
    at: &str,
    at_unix: i64,
) -> Result<EscalationRecord, IssueError> {
    let before = provider.get(key).await?.body;
    let record = escalation::next(escalation::parse(&before).as_ref(), reason, at, at_unix);
    provider
        .edit(key, None, Some(&escalation::stamp(&before, &record)))
        .await?;

    // The second read and the repair are best effort. The record is already
    // written. Both can only add to what the issue holds. Failing here would
    // drop the comment that tells a person what went wrong.
    let Ok(settled) = provider.get(key).await else {
        return Ok(record);
    };
    if escalation::parse(&settled.body).as_ref() != Some(&record) {
        let _ = provider
            .edit(key, None, Some(&escalation::stamp(&settled.body, &record)))
            .await;
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::issue::{
        Issue, IssueClass, IssueDraft, IssueError, IssueKey, IssueLabel, IssueProvider, IssueState,
    };

    use super::*;

    /// A tracker that stores one issue body and hands back what it holds.
    ///
    /// `get` reads the stored body. `edit` replaces it. So a test can watch a
    /// write land the way a person would.
    ///
    /// `person_writes_after` is the race. When it holds text, a person saves
    /// that text right after the first `edit`. A tracker with no
    /// compare-and-set allows that.
    #[derive(Default)]
    struct BodyTracker {
        body: std::sync::Mutex<String>,
        person_writes_after: std::sync::Mutex<Option<String>>,
        labelled: std::sync::Mutex<Vec<String>>,
        comments: std::sync::Mutex<Vec<String>>,
    }

    impl BodyTracker {
        fn holding(body: &str) -> Self {
            Self {
                body: std::sync::Mutex::new(body.to_owned()),
                ..Self::default()
            }
        }

        fn body(&self) -> String {
            self.body.lock().expect("fixture lock").clone()
        }

        fn labelled(&self) -> Vec<String> {
            self.labelled.lock().expect("fixture lock").clone()
        }

        fn comments(&self) -> Vec<String> {
            self.comments.lock().expect("fixture lock").clone()
        }
    }

    #[async_trait]
    impl IssueProvider for BodyTracker {
        fn id(&self) -> &str {
            "body-tracker"
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

        async fn comment(&self, _key: &IssueKey, body: &str) -> Result<(), IssueError> {
            self.comments
                .lock()
                .expect("fixture lock")
                .push(body.to_owned());
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            self.labelled
                .lock()
                .expect("fixture lock")
                .extend(add.iter().cloned());
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            body: Option<&str>,
        ) -> Result<(), IssueError> {
            if let Some(body) = body {
                *self.body.lock().expect("fixture lock") = body.to_owned();
            }
            if let Some(theirs) = self
                .person_writes_after
                .lock()
                .expect("fixture lock")
                .take()
            {
                *self.body.lock().expect("fixture lock") = theirs;
            }
            Ok(())
        }

        async fn get(&self, key: &IssueKey) -> Result<Issue, IssueError> {
            Ok(Issue {
                key: key.clone(),
                title: "an issue somebody is reading".into(),
                body: self.body(),
                state: IssueState::Open,
                class: IssueClass::Bug,
                labels: self
                    .labelled()
                    .into_iter()
                    .map(|name| IssueLabel { name })
                    .collect(),
                created_at: "2026-09-02T00:00:00Z".into(),
                updated_at: "2026-09-02T00:00:00Z".into(),
                url: String::new(),
                parent: None,
            })
        }
    }

    /// A tracker nobody can read. Its writes still work. That is what makes
    /// it worth a test: a body nobody can read is left alone.
    struct BlindTracker {
        writes: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl IssueProvider for BlindTracker {
        fn id(&self) -> &str {
            "blind"
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

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
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
            body: Option<&str>,
        ) -> Result<(), IssueError> {
            self.writes
                .lock()
                .expect("fixture lock")
                .push(body.unwrap_or_default().to_owned());
            Ok(())
        }

        async fn get(&self, _key: &IssueKey) -> Result<Issue, IssueError> {
            Err(IssueError::Unavailable {
                provider: "blind".into(),
                reason: "the tracker cannot be read".into(),
            })
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn stuck() -> EscalationReason {
        escalation::classify("byte-identical output every time")
    }

    /// **The witness.** A note a person adds while the turn runs is still
    /// there after the escalation lands.
    ///
    /// The tracker holds the note. The loop holds nothing. So the record has
    /// to come from a read taken at write time, or the note goes.
    #[test]
    fn a_note_added_during_the_turn_survives_the_escalation() {
        let tracker = BodyTracker::holding(
            "## What happens\nThe queue never gets it back.\n\n\
             somebody's note: this needs the 0.9.3 client\n",
        );

        let record = runtime()
            .block_on(crate::self_driving_cmd::backlog::escalate(
                &tracker,
                &IssueKey::from("17"),
                "the turn exited 1 — byte-identical output every time",
                &stella_autonomy::escalation::EscalationPolicy::default(),
                "created by stella*",
            ))
            .expect("the tracker accepts writes");

        let landed = tracker.body();
        assert!(
            landed.contains("somebody's note: this needs the 0.9.3 client"),
            "the note a person added is gone: {landed}"
        );
        assert!(
            landed.starts_with("## What happens"),
            "the issue's own text is kept: {landed}"
        );
        assert_eq!(
            escalation::parse(&landed).as_ref(),
            Some(&record),
            "the record must survive a read of the body it was written into"
        );
        assert_eq!(
            tracker.labelled(),
            vec![stella_autonomy::ESCALATION_LABEL.to_owned()],
            "the label stays as the marker a person scans for"
        );
        assert_eq!(
            tracker.comments().len(),
            1,
            "one comment says what went wrong"
        );
    }

    /// A save that lands in the gap keeps its text. The record goes back on
    /// top of it.
    #[test]
    fn a_write_that_lands_in_the_gap_keeps_its_text_and_gets_the_record_back() {
        let tracker = BodyTracker::holding("## What happens\nThe queue never gets it back.\n");
        *tracker.person_writes_after.lock().expect("fixture lock") =
            Some("## What happens\nRewritten by hand.\n".to_owned());

        let record = runtime()
            .block_on(write(
                &tracker,
                &IssueKey::from("17"),
                stuck(),
                "2026-09-02T00:00:00Z",
                1_000,
            ))
            .expect("the tracker accepts writes");

        let landed = tracker.body();
        assert!(
            landed.starts_with("## What happens\nRewritten by hand."),
            "the newer text is kept: {landed}"
        );
        assert_eq!(
            escalation::parse(&landed).as_ref(),
            Some(&record),
            "an issue with the label and no record is parked for good"
        );
    }

    /// A body the loop cannot read is a body it cannot replace.
    #[test]
    fn an_unreadable_body_is_never_written_over() {
        let tracker = BlindTracker {
            writes: std::sync::Mutex::new(Vec::new()),
        };

        let outcome = runtime().block_on(write(
            &tracker,
            &IssueKey::from("17"),
            stuck(),
            "2026-09-02T00:00:00Z",
            1_000,
        ));

        assert!(outcome.is_err(), "{outcome:?}");
        assert!(
            tracker.writes.lock().expect("fixture lock").is_empty(),
            "nothing may be sent when the current text is unknown"
        );
    }

    /// The count carries forward, so parking can be reached. The second
    /// escalation reads the record the first one left in the body.
    #[test]
    fn escalating_stamps_a_record_the_next_run_can_read() {
        let tracker = BodyTracker::holding("## What happens\nThe environment was stale.\n");
        let policy = stella_autonomy::escalation::EscalationPolicy::default();
        let key = IssueKey::from("17");

        let first = runtime()
            .block_on(crate::self_driving_cmd::backlog::escalate(
                &tracker,
                &key,
                "the turn exited 1 — the same `bash` call with identical \
                 arguments produced byte-identical output every time",
                &policy,
                "created by stella*",
            ))
            .expect("the tracker accepts writes");

        assert_eq!(first.attempts, 1);
        assert_eq!(
            first.last_reason,
            stella_autonomy::escalation::EscalationReason::Environmental(
                stella_autonomy::escalation::EnvCause::StuckLoop
            ),
            "a stuck-loop abort is a broken machine, and it is retried eagerly"
        );

        let second = runtime()
            .block_on(crate::self_driving_cmd::backlog::escalate(
                &tracker,
                &key,
                "the turn ran and could not work out what the issue asks for",
                &policy,
                "created by stella*",
            ))
            .expect("the tracker accepts writes");
        assert_eq!(
            second.attempts, 2,
            "the count carries forward, or parking is never reached"
        );
        assert_eq!(
            tracker.body().matches(escalation::MARKER_OPEN).count(),
            1,
            "a body escalated twice carries one record"
        );
    }
}
