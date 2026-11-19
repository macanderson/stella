//! The `_meta` a `tools/call` request carries.
//!
//! Two calls to one tool with the same input look the same on the wire. The
//! engine's id for the call tells them apart, and it is the id the
//! `PreToolUse` and `PostToolUse` hooks carry too. A server that does not
//! know the key ignores it.

use serde_json::{Map, Value};

/// The `_meta` key that names the engine's tool call.
///
/// Namespaced under a reverse-DNS prefix, as the MCP spec asks of a key
/// that is not the protocol's own.
pub const TOOL_USE_ID_META_KEY: &str = "dev.stella/tool_use_id";

/// The `_meta` object for the tool call this future runs for, or `None`
/// when it runs outside one.
pub(crate) fn for_current_call() -> Option<Value> {
    meta_for(stella_core::tool_use::current())
}

/// The `_meta` object naming `tool_use_id`, or `None` without one.
fn meta_for(tool_use_id: Option<String>) -> Option<Value> {
    let tool_use_id = tool_use_id?;
    let mut meta = Map::new();
    meta.insert(TOOL_USE_ID_META_KEY.into(), tool_use_id.into());
    Some(Value::Object(meta))
}

#[cfg(test)]
mod tests {
    use super::{TOOL_USE_ID_META_KEY, for_current_call, meta_for};

    #[test]
    fn a_call_id_is_sent_under_the_stella_key() {
        let meta = meta_for(Some("call_1".into())).expect("an id makes a meta object");
        assert_eq!(meta[TOOL_USE_ID_META_KEY], "call_1");
        assert_eq!(meta.as_object().map(serde_json::Map::len), Some(1));
    }

    #[test]
    fn no_call_id_sends_no_meta() {
        assert_eq!(meta_for(None), None);
    }

    /// With no id, the request leaves the `_meta` key out, rather than
    /// sending it as null.
    #[test]
    fn a_request_with_no_id_has_no_meta_key() {
        let params = crate::protocol::CallToolParams {
            name: "echo".into(),
            arguments: serde_json::json!({}),
            meta: meta_for(None),
        };
        let json = serde_json::to_value(&params).expect("the params serialize");
        assert!(json.get("_meta").is_none(), "{json}");
    }

    #[tokio::test]
    async fn the_meta_names_the_call_the_engine_is_running() {
        assert_eq!(for_current_call(), None, "no call is running here");
        let meta = stella_core::tool_use::scope("call_7".into(), async { for_current_call() })
            .await
            .expect("a running call is named");
        assert_eq!(meta[TOOL_USE_ID_META_KEY], "call_7");
    }
}
