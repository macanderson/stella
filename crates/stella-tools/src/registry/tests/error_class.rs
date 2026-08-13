//! The class a refusal carries (#3145).
//!
//! `ToolOutput::Error`'s message is prose written for the model; the class
//! beside it is what a per-tool error-rate ceiling reads, and the whole point
//! is that it can be read WITHOUT matching on that prose. The registry's own
//! refusal — a tool the model asked for that does not exist — is the one
//! failure this module owns, so it is checked here.

use super::*;

/// The unknown-tool refusal carries `NotFound`, not an unclassified error:
/// "the model named a tool that is not on the menu" is model misuse, and
/// counting it against a tool's error rate would charge us for the prompt.
#[tokio::test]
async fn unknown_tool_refusal_is_classified_not_found() {
    use stella_protocol::ErrorClass;
    let (_root, reg) = bare_registry(None);
    let ToolOutput::Error { message, class } = reg.execute("nonexistent", &Value::Null).await
    else {
        panic!("expected an error for an unknown tool");
    };
    assert_eq!(class, Some(ErrorClass::NotFound));
    assert!(
        message.starts_with("unknown tool `nonexistent`"),
        "the class rides beside the prose, never replacing it: {message}"
    );
}
