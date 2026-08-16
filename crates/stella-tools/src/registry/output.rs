// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Post-execution validation of a tool's structured output against its
//! contract's `output_schema` (#3285) — the other direction of
//! [`super::validate`], run by `ToolRegistry::execute` after the tool
//! returns.
//!
//! # A violation is a tool defect, never model misuse
//!
//! Input validation refuses the *model's* mistake
//! ([`stella_protocol::ErrorClass::InvalidInput`]). A tool returning
//! something its own contract forbids is *our* bug, so the classification is
//! [`stella_protocol::ErrorClass::Internal`] — the class an error-rate
//! ceiling is actually about — and the message names the tool and the
//! failing field. It is surfaced rather than silently passed: the model must
//! not build on data the contract says cannot exist, and a defect nobody
//! sees is a defect nobody fixes.
//!
//! # What is checked, exactly
//!
//! Only a successful output's structured half, and only when the tool's
//! contract promises one:
//!
//! - No declared `output_schema` (every tool until its author declares one):
//!   nothing is checked, whatever the tool returns.
//! - Declared, and the output is [`ToolOutput::Error`]: untouched — a
//!   failure has no structured half to keep a promise about.
//! - Declared, and `Ok.data` is `None`: a violation. A promise of structure
//!   with no structure attached is the drift this seam exists to catch.
//! - Declared, and `Ok.data` is present: validated over the same JSON-Schema
//!   subset as inputs ([`super::validate::check`] — one checker, so the two
//!   vocabularies cannot drift).
//!
//! The prose `content` is deliberately never validated: it is written for
//! the model, and no schema should constrain how a tool talks.

use stella_protocol::ErrorClass;
use stella_protocol::tool::ToolOutput;

use super::{Tool, validate};
use crate::contracts;

/// The post-execution gate: `Some(defect)` when `output` breaks the tool's
/// declared output contract, `None` to pass the output through (including
/// for an unknown tool — an unknown name never reached execution).
pub(super) fn defect(tool: Option<&dyn Tool>, output: &ToolOutput) -> Option<ToolOutput> {
    let schema = tool?.schema();
    let expected = contracts::contract_for(&schema).output_schema?;
    let ToolOutput::Ok { data, .. } = output else {
        return None;
    };
    let name = &schema.name;
    match data {
        None => Some(ToolOutput::classified_error(
            ErrorClass::Internal,
            format!(
                "tool `{name}` violated its output contract: it promises structured \
                 data and returned none"
            ),
        )),
        Some(value) => validate::check(&expected, value).map(|violation| {
            ToolOutput::classified_error(
                ErrorClass::Internal,
                format!("tool `{name}` violated its output contract: {violation}"),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use stella_protocol::tool::ToolSchema;

    use super::*;

    /// A stub wearing a catalog name whose contract declares an
    /// `output_schema`, returning whatever the test tells it to — the one
    /// way to witness the seam, since the real `list_state` keeps its
    /// contract.
    struct Defective(ToolOutput);

    #[async_trait]
    impl Tool for Defective {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "list_state".into(),
                description: "d".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                read_only: true,
                speculation_safe: true,
            }
        }

        async fn execute(&self, _input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
            self.0.clone()
        }
    }

    fn classified_internal(output: &ToolOutput) -> &str {
        match output {
            ToolOutput::Error {
                message,
                class: Some(ErrorClass::Internal),
            } => message,
            other => panic!("expected a classified Internal defect, got {other:?}"),
        }
    }

    /// The #3285 witness: a tool whose declared `output_schema` contradicts
    /// what it returned yields a classified `Internal` error naming the
    /// failing field — a tool defect, never model misuse.
    #[test]
    fn contradicting_data_is_an_internal_defect_naming_the_field() {
        let tool = Defective(ToolOutput::ok_with_data("ok", json!({ "entries": "oops" })));
        let out = defect(
            Some(&tool),
            &ToolOutput::ok_with_data("ok", json!({ "entries": "oops" })),
        );
        assert_eq!(
            classified_internal(&out.expect("the violation must surface")),
            "tool `list_state` violated its output contract: field `entries` \
             must be an array, got string"
        );
    }

    /// A promise of structure with nothing attached is the same defect.
    #[test]
    fn a_promised_schema_with_no_data_is_a_defect() {
        let tool = Defective(ToolOutput::ok("prose only"));
        let out = defect(Some(&tool), &ToolOutput::ok("prose only"));
        assert_eq!(
            classified_internal(&out.expect("the violation must surface")),
            "tool `list_state` violated its output contract: it promises \
             structured data and returned none"
        );
    }

    /// Satisfied promise: the output passes through untouched.
    #[test]
    fn conforming_data_passes() {
        let good = ToolOutput::ok_with_data("ok", json!({ "entries": [] }));
        let tool = Defective(good.clone());
        assert_eq!(defect(Some(&tool), &good), None);
    }

    /// A failure has no structured half to keep a promise about.
    #[test]
    fn an_error_output_is_never_reclassified() {
        let err = ToolOutput::error("the world failed");
        let tool = Defective(err.clone());
        assert_eq!(defect(Some(&tool), &err), None);
    }

    /// The live path, end to end: the real `list_state` — the first
    /// declarer — returns structured data that satisfies its own contract,
    /// so the seam passes it through with the prose intact.
    #[tokio::test]
    async fn the_first_declarer_keeps_its_own_contract() {
        let root = tempfile::tempdir().unwrap();
        let reg = super::super::ToolRegistry::new(root.path().to_path_buf());
        reg.execute("save_state", &json!({ "key": "seam", "content": "v" }))
            .await;
        let out = reg.execute("list_state", &json!({})).await;
        match out {
            ToolOutput::Ok {
                content,
                data: Some(data),
            } => {
                assert!(content.contains("seam"), "prose lists the key: {content}");
                let keys: Vec<&str> = data["entries"]
                    .as_array()
                    .expect("entries is an array")
                    .iter()
                    .filter_map(|entry| entry["key"].as_str())
                    .collect();
                assert!(keys.contains(&"seam"), "data lists the key: {data}");
            }
            other => panic!("list_state must succeed with structured data: {other:?}"),
        }
    }

    /// A tool with no declared `output_schema` — every tool until its author
    /// declares one — is untouched, whatever it returns.
    #[test]
    fn a_tool_with_no_declared_schema_is_untouched() {
        struct Undeclared;

        #[async_trait]
        impl Tool for Undeclared {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "get_environment".into(),
                    description: "d".into(),
                    input_schema: json!({"type": "object", "properties": {}}),
                    read_only: true,
                    speculation_safe: true,
                }
            }

            async fn execute(&self, _input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
                ToolOutput::ok("anything")
            }
        }

        let out = ToolOutput::ok_with_data("anything", json!({ "free": "form" }));
        assert_eq!(defect(Some(&Undeclared), &out), None);
    }
}
