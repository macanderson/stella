//! Ingest: the seam where an untrusted server's *payloads* become the engine's
//! own shapes, and where every one of them is bounded.
//!
//! Two directions, one rule (#551 — cap at ingest, because `stella-core`'s
//! compaction only trims on a LATER turn, after the tokens are already paid
//! for):
//!
//! - **Discovery.** [`fetch_all_tools`] drives `tools/list` to exhaustion and
//!   returns [`McpToolInfo`]s whose description and schema are already within
//!   budget, plus how many tools were refused past [`MAX_TOOLS_PER_SERVER`].
//! - **Results.** [`decode_call_result`] maps a `tools/call` result into
//!   [`ToolOutput`], and [`render_content`] flattens an MCP content array into
//!   one model-visible string.
//!
//! Split out of `client.rs` verbatim (#629's 1500-line ratchet).

use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::Value;
use stella_protocol::ToolOutput;

use super::to_value;
use crate::error::McpError;
use crate::http::{truncate, truncate_middle_out};
use crate::protocol::{CallToolResult, ContentBlock, ListToolsParams, ListToolsResult};
use crate::transport::Transport;

/// A hard cap on `tools/list` pages, defending against a server that returns
/// a non-advancing cursor forever.
const MAX_TOOL_PAGES: usize = 1000;

// Context budgets (#551). An MCP server is *untrusted input*: nothing on the
// wire stops it from answering a call with a gigabyte of text, advertising ten
// thousand tools, or attaching a megabyte description to each one. Without a
// bound here the tokens are already paid for by the time `stella-core`'s
// compaction runs — on a LATER turn — so the cap has to live at ingest, in
// this crate.
//
// The numbers below are RATIFIED as shipped (#616): sized to be invisible to
// every well-behaved server we know of while still bounding a hostile one.
// They are deliberately NOT configurable — no server has been observed to
// need more, and a config knob for a cap nobody hits is permanent public
// surface (entry-point signatures, settings schema, docs) bought for
// nothing. If a real server ever needs a higher cap, raise the const with
// that server named in the commit, or add the knob then, with the demand in
// hand. The same ruling covers `MAX_TOOL_PAGES` (1000) and the SSE
// `MAX_BUFFERED_BYTES` (8 MiB): existing deliberate limits, kept.

/// Byte budget for one rendered `tools/call` result, applied middle-out
/// (head and tail kept, with an explicit elision marker between them).
/// Matches the ceiling `stella-tools` already imposes on native
/// `bash`/custom-tool output, so an MCP tool cannot buy more of the model's
/// context than a local one.
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 100_000;

/// How many tools this client will accept from ONE server. Past this the
/// remaining tools are dropped and `tools/list` pagination stops; the count is
/// recorded as a non-fatal diagnostic ([`super::McpClient::dropped_tool_count`],
/// [`crate::McpToolSet::over_advertising_servers`]) — the server is connected
/// and working, it just advertises more surface than the model's context can
/// afford.
///
/// Public so a CLI/TUI can *name the cap it hit* when it renders that
/// diagnostic ("advertised 12 tools past the 256-tool cap") instead of
/// hard-coding a number that would silently drift from this one.
pub const MAX_TOOLS_PER_SERVER: usize = 256;

/// Character budget for one tool's advertised `description`, truncated
/// head-only (a description's contract is stated up front).
pub(crate) const MAX_TOOL_DESCRIPTION_CHARS: usize = 2_000;

/// Byte budget for one tool's serialized `inputSchema`. A JSON Schema cannot
/// be string-truncated and stay valid, so an over-budget schema is *replaced*
/// with the permissive `{"type": "object"}` (see [`normalize_schema`]) rather
/// than chopped into malformed JSON.
pub(crate) const MAX_TOOL_SCHEMA_BYTES: usize = 16_384;

/// Appended to a tool's description when its schema was over budget and
/// replaced, so the model is told the shape it can no longer see. Added
/// *after* the description is truncated, so the steer is never the part that
/// gets cut.
const SCHEMA_REPLACED_NOTE: &str = "\n\n[stella: this tool's advertised input schema exceeded the \
     size budget and was replaced with a permissive `{\"type\": \"object\"}` schema — pass the \
     arguments this description names.]";

/// One tool advertised by a server, in the client's own shape (the raw,
/// un-namespaced name plus its input schema).
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// The tool's name exactly as the server advertised it, before
    /// [`crate::toolset::McpToolSet`] prefixes it with the server namespace.
    pub name: String,
    /// The server's human-readable description of the tool, already bounded at
    /// ingest (`MAX_TOOL_DESCRIPTION_CHARS`) and carrying an appended note when
    /// this tool's schema was over budget and replaced.
    pub description: String,
    /// The tool's JSON Schema for its arguments (`inputSchema` on the wire),
    /// passed through verbatim unless it was absent or over budget — see
    /// `normalize_schema`, the only place this client substitutes a schema.
    pub input_schema: Value,
    /// The server advertised this tool as read-only or idempotent, so a
    /// duplicate delivery is harmless and the client may transparently
    /// re-send it after an ambiguous connection drop. `false` (the default
    /// for un-annotated tools) means a drop mid-call is surfaced as an
    /// error instead — the server may have already executed the call, and
    /// re-sending a mutating tool risks double execution.
    pub safe_to_retry: bool,
}

/// Drive `tools/list` to exhaustion over `transport`, following `nextCursor`,
/// returning the accepted tools plus how many were refused past
/// [`MAX_TOOLS_PER_SERVER`] (#551).
///
/// Every tool is bounded *at ingest* — description and schema included — so
/// each consumer (routing, `schemas()`, telemetry) sees the same already-safe
/// values instead of each having to remember to cap them.
///
/// A name the server advertises twice is kept once, FIRST occurrence wins —
/// mirroring the duplicate-server-name policy in [`crate::McpToolSet`]. A
/// second entry under the same name could never be routed to (both map to one
/// `mcp__server__tool` name), so keeping it would only duplicate its
/// `ToolSchema` into every model request.
pub(super) async fn fetch_all_tools(
    name: &str,
    transport: &dyn Transport,
) -> Result<(Vec<McpToolInfo>, usize), McpError> {
    let mut tools = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_TOOL_PAGES {
        let params = ListToolsParams {
            cursor: cursor.clone(),
        };
        let raw = transport.request("tools/list", to_value(&params)?).await?;
        let page: ListToolsResult = serde_json::from_value(raw)
            .map_err(|e| McpError::Protocol(format!("could not decode tools/list result: {e}")))?;
        let advertised = page.tools.len();
        for (index, tool) in page.tools.into_iter().enumerate() {
            // A re-advertised name is merged into its first occurrence, not
            // counted against the cap — it costs the model nothing.
            if seen.contains(&tool.name) {
                continue;
            }
            if tools.len() == MAX_TOOLS_PER_SERVER {
                // The cap bit mid-page. Stop paginating too — a server that
                // advertises more tools than the model's context can hold is
                // not one we want to keep fetching from, so the returned count
                // is the remainder of THIS page, a floor rather than a total.
                return Ok((tools, advertised - index));
            }
            seen.insert(tool.name.clone());
            let (input_schema, schema_replaced) = normalize_schema(tool.input_schema);
            let mut description = truncate(
                tool.description.as_deref().unwrap_or_default(),
                MAX_TOOL_DESCRIPTION_CHARS,
            );
            if schema_replaced {
                description.push_str(SCHEMA_REPLACED_NOTE);
            }
            tools.push(McpToolInfo {
                description,
                name: tool.name,
                input_schema,
                safe_to_retry: tool.annotations.read_only_hint || tool.annotations.idempotent_hint,
            });
        }
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => return Ok((tools, 0)),
        }
    }
    Err(McpError::Protocol(format!(
        "server `{name}` exceeded {MAX_TOOL_PAGES} tools/list pages — cursor never terminated"
    )))
}

/// Map a raw `tools/call` result value into the engine's [`ToolOutput`].
///
/// This is the seam where an untrusted *result* is bounded (#551): the
/// rendered string is capped at [`MAX_TOOL_RESULT_BYTES`] middle-out, on both
/// the `Ok` and the `isError: true` branch, so a server cannot evict the
/// conversation with one enormous answer. [`render_content`] itself stays pure
/// and zero-copy.
///
/// It bounds **only** the two branches below. A server that answers
/// `tools/call` with a JSON-RPC *error object* fails the request before it
/// gets here (`RequestOutcome::Protocol` → `Err`), and that route is bounded
/// by [`McpError::user_message`] at the same budget.
pub(super) fn decode_call_result(tool: &str, raw: Value) -> Result<ToolOutput, McpError> {
    let result: CallToolResult = serde_json::from_value(raw)
        .map_err(|e| McpError::Protocol(format!("could not decode tools/call result: {e}")))?;
    let rendered = render_content(&result.content);
    // Under budget stays byte-identical (and un-copied) — only an oversized
    // result pays for the middle-out rebuild.
    let rendered = if rendered.len() > MAX_TOOL_RESULT_BYTES {
        truncate_middle_out(&rendered, MAX_TOOL_RESULT_BYTES)
    } else {
        rendered
    };
    if result.is_error {
        Ok(ToolOutput::Error {
            message: if rendered.is_empty() {
                format!("tool `{tool}` reported an error with no detail")
            } else {
                rendered
            },
        })
    } else {
        Ok(ToolOutput::Ok { content: rendered })
    }
}

/// A tool with a null/missing input schema still needs *a* schema; default to
/// the permissive empty-object schema so the model always sees valid JSON
/// Schema.
///
/// An **over-budget** schema (past [`MAX_TOOL_SCHEMA_BYTES`] serialized) gets
/// that same fallback, and the `true` in the returned pair tells the caller to
/// say so in the tool's description. A schema is the one field that cannot be
/// string-truncated: chopping it would hand the model malformed JSON Schema,
/// which is strictly worse than a permissive one. Losing the shape costs an
/// argument round-trip; losing the *validity* costs every call to that tool.
fn normalize_schema(schema: Value) -> (Value, bool) {
    let permissive = || serde_json::json!({ "type": "object" });
    if schema.is_null() {
        return (permissive(), false);
    }
    let over_budget = match serde_json::to_vec(&schema) {
        Ok(bytes) => bytes.len() > MAX_TOOL_SCHEMA_BYTES,
        // Unserializable is unusable either way, and the permissive fallback
        // is still valid JSON Schema — fall back rather than pass it through.
        Err(_) => true,
    };
    if over_budget {
        (permissive(), true)
    } else {
        (schema, false)
    }
}

/// Concatenate a `tools/call` content array into a single model-visible
/// string: text verbatim, everything else a compact placeholder.
///
/// Text blocks are *borrowed* rather than cloned into an intermediate `Vec`:
/// a tool that returns a large file reads back as one multi-megabyte text
/// block, and the old shape copied it twice (once per part, once by `join`)
/// on the hot path of every MCP call.
pub(crate) fn render_content(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        let piece: Cow<'_, str> = match block.kind.as_str() {
            "text" => Cow::Borrowed(block.text.as_deref().unwrap_or_default()),
            "image" => Cow::Borrowed("[image]"),
            "audio" => Cow::Borrowed("[audio]"),
            "resource" => match block.resource.as_ref().and_then(|r| r.uri.as_deref()) {
                Some(uri) => Cow::Owned(format!("[resource: {uri}]")),
                None => Cow::Borrowed("[resource]"),
            },
            "resource_link" => match block.uri.as_deref() {
                Some(uri) => Cow::Owned(format!("[resource: {uri}]")),
                None => Cow::Borrowed("[resource]"),
            },
            // A block with no `type` is malformed; fall back to any text it
            // carried, else a generic marker.
            "" => Cow::Borrowed(block.text.as_deref().unwrap_or("[unknown]")),
            // An unknown but named type: summarize by its name.
            other => Cow::Owned(format!("[{other}]")),
        };
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&piece);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ResourceContents;

    fn block(kind: &str) -> ContentBlock {
        ContentBlock {
            kind: kind.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn render_concatenates_text_and_summarizes_the_rest() {
        let blocks = vec![
            ContentBlock {
                kind: "text".into(),
                text: Some("first".into()),
                ..Default::default()
            },
            block("image"),
            ContentBlock {
                kind: "text".into(),
                text: Some("second".into()),
                ..Default::default()
            },
            ContentBlock {
                kind: "resource".into(),
                resource: Some(ResourceContents {
                    uri: Some("file:///a.txt".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ContentBlock {
                kind: "resource_link".into(),
                uri: Some("https://x/y".into()),
                ..Default::default()
            },
            block("audio"),
            block("brand_new_kind"),
        ];
        let rendered = render_content(&blocks);
        assert_eq!(
            rendered,
            "first\n[image]\nsecond\n[resource: file:///a.txt]\n[resource: https://x/y]\n[audio]\n[brand_new_kind]"
        );
    }

    #[test]
    fn empty_content_renders_empty() {
        assert_eq!(render_content(&[]), "");
    }
}
