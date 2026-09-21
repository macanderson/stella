//! Witnesses for the forge tool.
//!
//! These prove that Rust puts the footer on, rather than the prompt asking a
//! model to. They also prove that `merge` cannot happen by accident.

use std::sync::{Arc, Mutex};

use serde_json::json;
use stella_protocol::pull_request::{
    Check, PullRequest, PullRequestDraft, PullRequestError, PullRequestKey, PullRequestPatch,
    PullRequestProvider, PullRequestSummary,
};

use super::*;
use crate::forge::ForgeSlots;

/// Every write this forge was asked to make, in order.
#[derive(Default)]
struct Written {
    opened: Vec<PullRequestDraft>,
    updated: Vec<(String, PullRequestPatch)>,
    comments: Vec<(String, String)>,
    edited_comments: Vec<(String, String)>,
    merged: Vec<String>,
    closed: Vec<String>,
}

/// A forge that writes down each call instead of reaching anything.
#[derive(Default)]
struct Recorder {
    written: Mutex<Written>,
}

impl Recorder {
    fn written(&self) -> std::sync::MutexGuard<'_, Written> {
        self.written.lock().expect("fixture lock")
    }
}

impl PullRequestProvider for Recorder {
    fn id(&self) -> &str {
        "recorder"
    }

    fn open(&self, draft: &PullRequestDraft) -> Result<PullRequestKey, PullRequestError> {
        self.written().opened.push(draft.clone());
        Ok(PullRequestKey::from("31"))
    }

    fn get(&self, _key: &PullRequestKey) -> Result<PullRequest, PullRequestError> {
        unimplemented!("no test here reads a pull request back")
    }

    fn list_open(
        &self,
        _search: Option<&str>,
    ) -> Result<Vec<PullRequestSummary>, PullRequestError> {
        Ok(Vec::new())
    }

    fn checks_for_ref(&self, _git_ref: &str) -> Result<Vec<Check>, PullRequestError> {
        Ok(Vec::new())
    }

    fn required_checks(&self, _branch: &str) -> Result<Vec<String>, PullRequestError> {
        Ok(Vec::new())
    }

    fn recent_commits(
        &self,
        _branch: &str,
        _depth: usize,
    ) -> Result<Vec<String>, PullRequestError> {
        Ok(Vec::new())
    }

    fn mark_ready(&self, _key: &PullRequestKey) -> Result<(), PullRequestError> {
        Ok(())
    }

    fn add_label(&self, _key: &PullRequestKey, _label: &str) -> Result<(), PullRequestError> {
        Ok(())
    }

    fn merge(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.written().merged.push(key.as_str().to_owned());
        Ok(())
    }

    fn sync_with_base(&self, _key: &PullRequestKey) -> Result<(), PullRequestError> {
        Ok(())
    }

    fn rerun_failed_checks(&self, _key: &PullRequestKey) -> Result<(), PullRequestError> {
        Ok(())
    }

    fn comment(&self, key: &PullRequestKey, body: &str) -> Result<CommentId, PullRequestError> {
        self.written()
            .comments
            .push((key.as_str().to_owned(), body.to_owned()));
        Ok(CommentId::from("5150"))
    }

    fn edit_comment(
        &self,
        _key: &PullRequestKey,
        comment: &CommentId,
        body: &str,
    ) -> Result<(), PullRequestError> {
        self.written()
            .edited_comments
            .push((comment.as_str().to_owned(), body.to_owned()));
        Ok(())
    }

    fn update(
        &self,
        key: &PullRequestKey,
        patch: &PullRequestPatch,
    ) -> Result<(), PullRequestError> {
        self.written()
            .updated
            .push((key.as_str().to_owned(), patch.clone()));
        Ok(())
    }

    fn close(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.written().closed.push(key.as_str().to_owned());
        Ok(())
    }
}

/// A tool over a forge that writes down each call, plus the log to read after.
fn tool() -> (PullRequestTool, Arc<Recorder>) {
    tool_under(crate::policy::ToolPolicy::allow_all())
}

/// The same, with the operator's switches set.
fn tool_under(policy: crate::policy::ToolPolicy) -> (PullRequestTool, Arc<Recorder>) {
    build(policy, stella_autonomy::Attribution::default())
}

/// The same, signing with the attribution given.
fn tool_signing_as(attribution: stella_autonomy::Attribution) -> (PullRequestTool, Arc<Recorder>) {
    build(crate::policy::ToolPolicy::allow_all(), attribution)
}

/// An attribution whose surfaces are five different strings.
///
/// [`stella_autonomy::Attribution::default`] gives all five the same text.
/// Under it, a body signed with the wrong field reads like one signed with
/// the right field. Every assertion still passes. These five differ, so a
/// test can say which surface a footer came from.
fn distinguishable() -> stella_autonomy::Attribution {
    stella_autonomy::Attribution {
        commit: "by-the-commit-field".into(),
        pull_request: "by-the-pull-request-field".into(),
        issue: "by-the-issue-field".into(),
        issue_comment: "by-the-issue-comment-field".into(),
        pull_request_comment: "by-the-pull-request-comment-field".into(),
        ..stella_autonomy::Attribution::default()
    }
}

fn build(
    policy: crate::policy::ToolPolicy,
    attribution: stella_autonomy::Attribution,
) -> (PullRequestTool, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let slots = ForgeSlots::default();
    *slots.pull_requests.write().unwrap() = Some(recorder.clone() as Arc<dyn PullRequestProvider>);
    *slots.policy.write().unwrap() = policy;
    *slots.attribution.write().unwrap() = attribution;
    (PullRequestTool::new(slots), recorder)
}

/// Run one call against a bare context.
async fn call(tool: &PullRequestTool, input: serde_json::Value) -> ToolOutput {
    tool.execute(
        &input,
        &crate::ctx::ToolCtx::bare(std::path::PathBuf::from(".")),
    )
    .await
}

/// An opened pull request is signed, and its title says what opened it.
#[tokio::test]
async fn an_opened_pull_request_is_signed_and_prefixed() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({
            "action": "create",
            "head": "fix/the-thing",
            "title": "fix(stella-cli): the thing",
            "body": "what changed and why"
        }),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    let opened = &written.opened[0];
    assert_eq!(opened.head_ref, "fix/the-thing");
    assert_eq!(
        opened.title,
        "stella self-driving: fix(stella-cli): the thing"
    );
    assert_eq!(
        opened.body,
        format!(
            "what changed and why\n\n---\n{}",
            stella_autonomy::SIGNATURE
        )
    );
}

/// Merging without `confirm` does not merge, and says what is missing.
///
/// This is the one verb that writes to a shared branch. It is also the one
/// that cannot be walked back. A model that retries a failed call must not
/// reach a merge by changing some other field.
#[tokio::test]
async fn a_merge_without_confirmation_does_not_merge() {
    let (tool, recorder) = tool();
    let output = call(&tool, json!({"action": "merge", "key": "31"})).await;
    let ToolOutput::Error { message, .. } = output else {
        panic!("an unconfirmed merge is an error");
    };
    assert!(message.contains("confirm: true"), "{message}");
    assert!(recorder.written().merged.is_empty(), "nothing was merged");
}

/// With `confirm`, it merges.
#[tokio::test]
async fn a_confirmed_merge_merges() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "merge", "key": "#7", "confirm": true}),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");
    // The leading `#` a model writes is stripped on the way to the forge.
    assert_eq!(recorder.written().merged, vec!["7".to_owned()]);
}

/// `draft: false` on an update is a change, and not an absent field.
///
/// Read `false` as "unset" and marking a pull request ready for review does
/// nothing, and says nothing. That is the shape of every boolean-option bug.
#[tokio::test]
async fn marking_ready_is_an_update_with_nothing_else() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "update", "key": "31", "draft": false}),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    let (key, patch) = &written.updated[0];
    assert_eq!(key, "31");
    assert_eq!(patch.draft, Some(false));
    assert_eq!(patch.title, None);
    assert_eq!(patch.body, None);
}

/// An update that changes nothing is refused rather than sent.
#[tokio::test]
async fn an_empty_update_is_refused() {
    let (tool, recorder) = tool();
    let output = call(&tool, json!({"action": "update", "key": "31"})).await;
    assert!(output.is_error(), "{output:?}");
    assert!(recorder.written().updated.is_empty());
}

/// A pull request comment is signed with the pull request comment footer.
#[tokio::test]
async fn a_pull_request_comment_is_signed() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "comment", "key": "31", "body": "rebased onto main"}),
    )
    .await;
    let ToolOutput::Ok { data, .. } = &output else {
        panic!("expected a success, got {output:?}");
    };
    assert_eq!(data.as_ref().unwrap()["comment_id"], json!("5150"));
    assert!(
        recorder.written().comments[0]
            .1
            .ends_with(stella_autonomy::SIGNATURE)
    );
}

/// Closing a pull request reaches `close`, and never `merge`.
#[tokio::test]
async fn closing_does_not_merge() {
    let (tool, recorder) = tool();
    let output = call(&tool, json!({"action": "close", "key": "31"})).await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    assert_eq!(written.closed, vec!["31".to_owned()]);
    assert!(written.merged.is_empty());
}

/// An operator can withhold `merge` and keep the rest of the tool.
///
/// `AGENTS.md invariant 9`'s second reason asks this of a tool that carries
/// several verbs. The session's own gate cannot do it. That gate answers for
/// a whole tool, so withholding `merge` there would take `comment` with it.
///
/// The refusal reaches the forge as nothing at all. That is the half worth
/// asserting. A refusal the provider still sees is not a refusal.
#[tokio::test]
async fn a_switched_off_merge_refuses_while_comment_still_runs() {
    let (tool, recorder) = tool_under(crate::policy::ToolPolicy::from_switches([(
        "pull_request.merge".into(),
        false,
    )]));

    let refused = call(&tool, json!({"action": "merge", "key": "31"})).await;
    let ToolOutput::Error { message, class, .. } = &refused else {
        panic!("a withheld action must be an error, got {refused:?}");
    };
    assert_eq!(*class, Some(ErrorClass::RefusedByPolicy));
    assert!(
        message.contains("pull_request.merge"),
        "the refusal names the key that did it: {message}"
    );
    assert!(
        recorder.written().merged.is_empty(),
        "a refused merge must not reach the forge"
    );

    let allowed = call(
        &tool,
        json!({"action": "comment", "key": "31", "body": "rebased onto main"}),
    )
    .await;
    assert!(!allowed.is_error(), "{allowed:?}");
    assert_eq!(recorder.written().comments.len(), 1);
}

/// A retitle keeps the prefix an open put there.
///
/// `create` prefixed the title and `update` did not. A model that opened a
/// pull request and then corrected its title stripped the mark off it. That
/// is the one surface a maintainer triages from. Nothing failed visibly: the
/// pull request still carried its footer, and a footer is read only once you
/// open it.
#[tokio::test]
async fn an_updated_title_keeps_the_prefix() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "update", "key": "31", "title": "fix(stella-cli): the other thing"}),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    assert_eq!(
        recorder.written().updated[0].1.title.as_deref(),
        Some("stella self-driving: fix(stella-cli): the other thing")
    );
}

/// A retitle that already carries the prefix keeps exactly one.
///
/// A model reading a pull request back and sending the title it saw is the
/// ordinary case here, not a pathological one.
#[tokio::test]
async fn a_retitle_does_not_stack_prefixes() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({
            "action": "update",
            "key": "31",
            "title": "stella self-driving: fix(stella-cli): the thing"
        }),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    assert_eq!(
        recorder.written().updated[0].1.title.as_deref(),
        Some("stella self-driving: fix(stella-cli): the thing")
    );
}

/// Each surface of this tool carries the footer configured for that surface.
///
/// The text is per surface because an operator will want different words on a
/// description and on a comment. Nothing held the routing before this. Under
/// the default attribution all five fields read the same string. So a
/// description signed with the comment field passed every other assertion in
/// this file.
///
/// Each surface is read against a string only its own field holds. A swapped
/// field names itself in the failure.
#[tokio::test]
async fn each_pull_request_surface_carries_its_own_footer() {
    let (tool, recorder) = tool_signing_as(distinguishable());

    for input in [
        json!({
            "action": "create",
            "head": "fix/the-thing",
            "title": "fix(stella-cli): the thing",
            "body": "what changed and why"
        }),
        json!({"action": "update", "key": "31", "body": "what changed, reworded"}),
        json!({"action": "comment", "key": "31", "body": "rebased onto main"}),
        json!({
            "action": "edit_comment",
            "key": "31",
            "comment_id": "5150",
            "body": "rebased onto main, again"
        }),
    ] {
        let output = call(&tool, input.clone()).await;
        assert!(!output.is_error(), "{input}: {output:?}");
    }

    let written = recorder.written();
    assert!(
        written.opened[0]
            .body
            .ends_with("by-the-pull-request-field"),
        "a description takes `pull_request`: {}",
        written.opened[0].body
    );
    let updated = written.updated[0]
        .1
        .body
        .as_deref()
        .expect("the update sent a body");
    assert!(
        updated.ends_with("by-the-pull-request-field"),
        "an edited description takes `pull_request`: {updated}"
    );
    assert!(
        written.comments[0]
            .1
            .ends_with("by-the-pull-request-comment-field"),
        "a comment takes `pull_request_comment`: {}",
        written.comments[0].1
    );
    assert!(
        written.edited_comments[0]
            .1
            .ends_with("by-the-pull-request-comment-field"),
        "an edited comment takes `pull_request_comment`: {}",
        written.edited_comments[0].1
    );
}
