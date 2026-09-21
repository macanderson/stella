//! Witnesses for the tracker tool.
//!
//! One point, made from several angles: the footer is not the model's to
//! supply. Every body that reaches a tracker through this tool carries one.
//! That holds whatever the model sent. No way of phrasing the call turns it
//! off.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use stella_protocol::issue::{
    CommentId, Issue, IssueDraft, IssueError, IssueKey, IssueProvider, IssueState,
};

use super::*;
use crate::forge::ForgeSlots;

/// Every write this tracker was asked to make, in order.
#[derive(Default)]
struct Written {
    filed: Vec<IssueDraft>,
    comments: Vec<(String, String)>,
    edits: Vec<(String, Option<String>, Option<String>)>,
    edited_comments: Vec<(String, String, String)>,
    closed: Vec<(String, String, String)>,
}

/// A tracker that writes down each call instead of reaching anything.
#[derive(Default)]
struct Recorder {
    written: Mutex<Written>,
}

impl Recorder {
    fn written(&self) -> std::sync::MutexGuard<'_, Written> {
        self.written.lock().expect("fixture lock")
    }
}

#[async_trait]
impl IssueProvider for Recorder {
    fn id(&self) -> &str {
        "recorder"
    }

    async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
        Ok(Vec::new())
    }

    async fn file(&self, draft: &IssueDraft) -> Result<IssueKey, IssueError> {
        self.written().filed.push(draft.clone());
        Ok(IssueKey::from("77"))
    }

    async fn close(&self, key: &IssueKey, receipt: &str, state: &str) -> Result<(), IssueError> {
        self.written().closed.push((
            key.as_str().to_owned(),
            receipt.to_owned(),
            state.to_owned(),
        ));
        Ok(())
    }

    async fn comment(&self, key: &IssueKey, body: &str) -> Result<CommentId, IssueError> {
        self.written()
            .comments
            .push((key.as_str().to_owned(), body.to_owned()));
        Ok(CommentId::from("9001"))
    }

    async fn edit_comment(
        &self,
        key: &IssueKey,
        comment: &CommentId,
        body: &str,
    ) -> Result<(), IssueError> {
        self.written().edited_comments.push((
            key.as_str().to_owned(),
            comment.as_str().to_owned(),
            body.to_owned(),
        ));
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
        key: &IssueKey,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), IssueError> {
        self.written().edits.push((
            key.as_str().to_owned(),
            title.map(str::to_owned),
            body.map(str::to_owned),
        ));
        Ok(())
    }

    async fn get(&self, key: &IssueKey) -> Result<Issue, IssueError> {
        Ok(Issue {
            key: key.clone(),
            title: String::new(),
            body: String::new(),
            state: IssueState::Open,
            class: stella_protocol::issue::IssueClass::Other,
            labels: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
            url: String::new(),
            parent: None,
        })
    }
}

/// A tool over a tracker that writes down each call, plus the log to read after.
fn tool() -> (IssueTool, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let slots = ForgeSlots::default();
    *slots.issues.write().unwrap() = Some(recorder.clone() as Arc<dyn IssueProvider>);
    (IssueTool::new(slots), recorder)
}

/// Run one call against a bare context.
async fn call(tool: &IssueTool, input: serde_json::Value) -> ToolOutput {
    tool.execute(
        &input,
        &crate::ctx::ToolCtx::bare(std::path::PathBuf::from(".")),
    )
    .await
}

/// A filed issue carries the footer, and the model never asked for it.
///
/// This is the defect the whole plane exists to close. Ask for the footer in
/// prose (`self_driving_cmd::work::prompt_for`) and you get one only when the
/// model plays along.
#[tokio::test]
async fn a_filed_issue_is_signed_without_the_model_asking() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "create", "title": "a title", "body": "what is wrong"}),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    let filed = &written.filed[0];
    assert_eq!(filed.title, "a title");
    assert_eq!(
        filed.body,
        format!("what is wrong\n\n---\n{}", stella_autonomy::SIGNATURE)
    );
}

/// A comment is signed too, and the id comes back so it can be edited.
#[tokio::test]
async fn a_comment_is_signed_and_names_itself() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({"action": "comment", "key": "#4", "body": "the run is green"}),
    )
    .await;
    let ToolOutput::Ok { data, .. } = &output else {
        panic!("expected a success, got {output:?}");
    };
    assert_eq!(data.as_ref().unwrap()["comment_id"], json!("9001"));

    let written = recorder.written();
    let (key, body) = &written.comments[0];
    // The leading `#` a model writes is stripped. Trackers take the bare
    // number, and the forge's error would not tell the model that.
    assert_eq!(key, "4");
    assert!(body.ends_with(stella_autonomy::SIGNATURE), "{body}");
}

/// Editing a body that is already signed leaves one footer, not two.
///
/// The failure looks like this. A model reads an issue, changes a sentence,
/// and sends the whole body back with the footer still on it.
#[tokio::test]
async fn editing_a_signed_body_does_not_stack_footers() {
    let (tool, recorder) = tool();
    let already = stella_autonomy::sign("the original", stella_autonomy::SIGNATURE);
    let output = call(
        &tool,
        json!({"action": "update", "key": "412", "body": already}),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    let (_, _, body) = &written.edits[0];
    let body = body.as_ref().expect("the update carried a body");
    assert_eq!(body.matches("\n\n---\n").count(), 1, "{body}");
}

/// An update that changes nothing is refused rather than sent.
///
/// Hand a provider a patch of all `None` and it does nothing, then answers
/// `Ok`. Without this check, the model is told its edit landed when no edit
/// was made.
#[tokio::test]
async fn an_update_with_nothing_to_change_is_refused() {
    let (tool, recorder) = tool();
    let output = call(&tool, json!({"action": "update", "key": "412"})).await;
    assert!(output.is_error(), "{output:?}");
    assert!(recorder.written().edits.is_empty());
}

/// A closing receipt is signed like every other body.
#[tokio::test]
async fn a_closing_receipt_is_signed() {
    let (tool, recorder) = tool();
    let output = call(
        &tool,
        json!({
            "action": "close",
            "key": "412",
            "resolution": "not_planned",
            "receipt": "the reporter withdrew it"
        }),
    )
    .await;
    assert!(!output.is_error(), "{output:?}");

    let written = recorder.written();
    let (key, receipt, state) = &written.closed[0];
    assert_eq!(key, "412");
    assert_eq!(state, "not_planned");
    assert!(receipt.ends_with(stella_autonomy::SIGNATURE), "{receipt}");
}

/// A close with no receipt attaches no comment, rather than an empty signed one.
///
/// The adapter reads an empty receipt as "attach nothing". Sign an absent
/// receipt and you get a comment with nothing in it but a footer.
#[tokio::test]
async fn a_close_with_no_receipt_stays_silent() {
    let (tool, recorder) = tool();
    call(&tool, json!({"action": "close", "key": "412"})).await;
    assert_eq!(recorder.written().closed[0].1, "");
}

/// An action this tool does not have is named, with the ones it does.
#[tokio::test]
async fn an_unknown_action_lists_the_real_ones() {
    let (tool, _) = tool();
    let output = call(&tool, json!({"action": "merge", "key": "1"})).await;
    let ToolOutput::Error { message, .. } = output else {
        panic!("an unknown action is an error");
    };
    assert!(message.contains("edit_comment"), "{message}");
}
