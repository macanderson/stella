//! `save_memory` — persist a lesson to `.stella/memories/` so future
//! sessions execute better. The self-improvement loop's write half.
//!
//! The read half is deliberately NOT a tool: memories are loaded once at
//! session start into the system prompt's stable prefix (see
//! `stella-cli`'s prompt assembly), where every model call considers them
//! at prompt-cache-hit prices. That placement is also why this tool's
//! contract says new memories take effect NEXT session — hot-injecting
//! them mid-session would mutate the cached prefix and re-bill it on
//! every subsequent call.
//!
//! `cite_memory` — the feedback half of the loop: when a piece of injected
//! context actually informs the model's work, the model cites it here with a
//! usefulness score and a truthfulness verdict. Two citable shapes, matching
//! the two things the prompt tags: a recalled memory's stable `nod_…` id
//! (`stella-cli`'s recall block) and a context record's `^handle` (the
//! rendered rule channels — both the cached "Workspace rules" block, whose
//! heading asks for exactly this citation, and the per-turn volatile block).
//! The citations accumulate in a session ledger ([`CitationLedger`], shared
//! with `ToolRegistry`) and are persisted per execution by the CLI at turn
//! end — memory citations feed `stella memory`'s inspection view and the
//! rule-promotion eligibility gate (`stella-store`); record citations feed
//! the context-use attribution ledger (`ContextUseKind::Cited`), joining the
//! `^handle` the receipts plane recorded for the rendered block.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Workspace-relative directory holding one markdown memory per file.
pub const MEMORIES_DIR: &str = ".stella/memories";

pub struct SaveMemory;

#[async_trait]
impl Tool for SaveMemory {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_memory".into(),
            description: "Persist a durable lesson (gotcha, convention, failed approach) to \
                          .stella/memories/. Loaded into every future session's prompt. \
                          Takes effect next session."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Filename slug (lowercase, -, _)" },
                    "memory": { "type": "string", "description": "The lesson, 1-6 lines of markdown" }
                },
                "required": ["slug", "memory"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let slug = match crate::input::required_str(input, "slug") {
            Ok(v) => v,
            Err(message) => return ToolOutput::Error { message },
        };
        let memory = match crate::input::required_str(input, "memory") {
            Ok(v) => v,
            Err(message) => return ToolOutput::Error { message },
        };
        if slug.is_empty()
            || slug.len() > 64
            || !slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return ToolOutput::Error {
                message: format!(
                    "invalid slug `{slug}` — use 1-64 lowercase letters, digits, - or _"
                ),
            };
        }
        if memory.trim().is_empty() || memory.chars().count() > 2_000 {
            return ToolOutput::Error {
                message: "memory must be non-empty and at most 2000 characters — memories are \
                          prompt-resident, keep them dense"
                    .into(),
            };
        }
        let dir = root.join(MEMORIES_DIR);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return ToolOutput::Error {
                message: format!("could not create {}: {e}", dir.display()),
            };
        }
        let path = dir.join(format!("{slug}.md"));
        let existed = path.exists();
        if let Err(e) =
            crate::durable_write::write_file_durably(path.clone(), memory.as_bytes().to_vec()).await
        {
            return ToolOutput::Error {
                message: format!("could not write {}: {e}", path.display()),
            };
        }
        ToolOutput::Ok {
            content: format!(
                "{} memory `{slug}` — it will be in the prompt from the next session on",
                if existed { "updated" } else { "saved" }
            ),
        }
    }
}

/// One recorded citation: which recalled memory informed the turn, how
/// useful it was (1–5), whether its content still held true, and a short
/// remark. Mirrors `stella-store`'s `MemoryCitationRow` (this crate stays
/// store-free; the CLI maps at persist time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCitation {
    pub memory_id: String,
    pub useful_score: i64,
    pub truthful: bool,
    pub remark: String,
}

/// The session's citation ledger, shared between the [`CiteMemory`] tool
/// instance and the `ToolRegistry` that drains it at turn end. Unlike the
/// file-touch ledger (session-cumulative), this one is drained per persist
/// so a citation lands in the store exactly once — re-persisting it under a
/// later execution would inflate the >10 promotion count.
pub type CitationLedger = Arc<Mutex<Vec<MemoryCitation>>>;

/// The exact id shape the recall block shows the model: `nod_` + 24 hex
/// chars (`stella-context`'s node public id). Anything else is rejected up
/// front — a fabricated id could never be inspected or promoted anyway.
fn is_memory_id(id: &str) -> bool {
    id.strip_prefix("nod_")
        .is_some_and(|hex| hex.len() == 24 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The `^handle` shape the rendered record channels show the model: `^` plus
/// a slugged record handle — exactly the characters
/// `stella-core::records::handle::slug` can emit (lowercase, digits, `-`).
/// The sigil is kept on the stored citation so a record citation can never be
/// mistaken for a memory id anywhere downstream: the two namespaces stay
/// disjoint in `memory_citations` the same way they are disjoint in the
/// prompt.
fn is_record_handle(id: &str) -> bool {
    id.strip_prefix('^').is_some_and(|handle| {
        // At least one letter, matching the receipts-plane parser
        // (`record_handle_of`): the two ends of the citation join must agree
        // on what a handle is, or a citation could name an id no block row
        // will ever carry.
        handle.chars().any(|c| c.is_ascii_lowercase())
            && handle.len() <= 128
            && handle
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

pub struct CiteMemory(pub CitationLedger);

#[async_trait]
impl Tool for CiteMemory {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "cite_memory".into(),
            description: "Cite a piece of injected context that actually informed your work \
                          this turn — a recalled memory (a `[nod_…]`-tagged line from the \
                          relevant-context block) or a workspace rule/record (a `^handle` as \
                          shown beside its statement): score how useful it was and say whether \
                          its content still holds. Cite only context you genuinely used."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_id": { "type": "string", "description": "The memory's [nod_…] id or the record's ^handle, exactly as shown in the injected context" },
                    "useful_score": { "type": "integer", "minimum": 1, "maximum": 5, "description": "How useful the memory was for the actual work (1 = misleading/wasted effort, 5 = decisive)" },
                    "truthful": { "type": "boolean", "description": "Does the memory's content still hold true, as verified against the workspace this turn?" },
                    "remark": { "type": "string", "description": "One short sentence: how the memory helped, or what no longer holds" }
                },
                "required": ["memory_id", "useful_score", "truthful", "remark"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let memory_id = match crate::input::required_str(input, "memory_id") {
            Ok(v) => v,
            Err(message) => return ToolOutput::Error { message },
        };
        if !is_memory_id(memory_id) && !is_record_handle(memory_id) {
            return ToolOutput::Error {
                message: format!(
                    "`{memory_id}` is neither a memory id nor a record handle — cite the \
                     [nod_…] id or the ^handle exactly as shown in the injected context"
                ),
            };
        }
        let Some(useful_score) = input.get("useful_score").and_then(|v| v.as_i64()) else {
            return ToolOutput::Error {
                message: "missing required integer field `useful_score`".into(),
            };
        };
        if !(1..=5).contains(&useful_score) {
            return ToolOutput::Error {
                message: format!("useful_score must be 1-5, got {useful_score}"),
            };
        }
        let Some(truthful) = input.get("truthful").and_then(|v| v.as_bool()) else {
            return ToolOutput::Error {
                message: "missing required boolean field `truthful`".into(),
            };
        };
        let remark = input
            .get("remark")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();
        if remark.is_empty() || remark.chars().count() > 300 {
            return ToolOutput::Error {
                message: "remark must be a non-empty sentence of at most 300 characters".into(),
            };
        }

        let citation = MemoryCitation {
            memory_id: memory_id.to_string(),
            useful_score,
            truthful,
            remark: remark.to_string(),
        };
        let mut ledger = self.0.lock().unwrap_or_else(|p| p.into_inner());
        // A re-cite of the same memory before the ledger is drained is an
        // updated judgment, not a second data point — keep the latest only
        // (the store's UNIQUE (execution_id, memory_id) key assumes this).
        ledger.retain(|c| c.memory_id != citation.memory_id);
        ledger.push(citation);
        ToolOutput::Ok {
            content: format!(
                "cited memory {memory_id} (score {useful_score}, truthful {truthful})"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saves_validates_and_overwrites() {
        let root = std::env::temp_dir().join(format!("stella_memory_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ok = SaveMemory
            .execute(
                &serde_json::json!({"slug": "cargo-workspace", "memory": "Always build with --workspace."}),
                &root,
            )
            .await;
        assert!(!ok.is_error(), "{ok:?}");
        let saved =
            std::fs::read_to_string(root.join(MEMORIES_DIR).join("cargo-workspace.md")).unwrap();
        assert_eq!(saved, "Always build with --workspace.");

        for bad in [
            serde_json::json!({"slug": "UPPER", "memory": "x"}),
            serde_json::json!({"slug": "ok", "memory": ""}),
            serde_json::json!({"slug": "ok", "memory": "y".repeat(2001)}),
            serde_json::json!({"memory": "no slug"}),
        ] {
            assert!(SaveMemory.execute(&bad, &root).await.is_error(), "{bad}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    const NOD_A: &str = "nod_0123456789abcdef01234567";

    fn cite_input(id: &str, score: i64, truthful: bool, remark: &str) -> Value {
        serde_json::json!({
            "memory_id": id, "useful_score": score, "truthful": truthful, "remark": remark
        })
    }

    #[tokio::test]
    async fn cite_memory_records_and_a_recite_replaces_the_earlier_judgment() {
        let ledger: CitationLedger = Arc::default();
        let tool = CiteMemory(ledger.clone());
        let root = std::env::temp_dir();

        let ok = tool
            .execute(
                &cite_input(NOD_A, 4, true, "named the exact failing module"),
                &root,
            )
            .await;
        assert!(!ok.is_error(), "{ok:?}");
        // Re-citing the same memory updates in place — never a second row.
        tool.execute(
            &cite_input(NOD_A, 2, false, "actually the path moved"),
            &root,
        )
        .await;
        let recorded = ledger.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![MemoryCitation {
                memory_id: NOD_A.into(),
                useful_score: 2,
                truthful: false,
                remark: "actually the path moved".into(),
            }]
        );
    }

    #[tokio::test]
    async fn cite_memory_accepts_a_record_handle_with_its_sigil() {
        let ledger: CitationLedger = Arc::default();
        let tool = CiteMemory(ledger.clone());
        let root = std::env::temp_dir();

        // The `^handle` exactly as the rendered rule channels show it — the
        // shape the cached heading ("cite the ^handle of any you apply") asks
        // the model to hand back.
        let ok = tool
            .execute(
                &cite_input("^license-allowlist", 5, true, "kept the dep out"),
                &root,
            )
            .await;
        assert!(!ok.is_error(), "{ok:?}");
        assert_eq!(
            ledger.lock().unwrap().clone(),
            vec![MemoryCitation {
                memory_id: "^license-allowlist".into(),
                useful_score: 5,
                truthful: true,
                remark: "kept the dep out".into(),
            }],
            "the sigil is stored with the citation, keeping the two namespaces disjoint"
        );
    }

    #[tokio::test]
    async fn cite_memory_rejects_malformed_input_without_recording() {
        let ledger: CitationLedger = Arc::default();
        let tool = CiteMemory(ledger.clone());
        let root = std::env::temp_dir();

        for bad in [
            // Not the id shape the recall block shows.
            cite_input("mem_0123456789abcdef01234567", 4, true, "r"),
            cite_input("nod_short", 4, true, "r"),
            cite_input("anything else", 4, true, "r"),
            // Not the handle shape the rule channels show: a bare sigil, an
            // unslugged name, and a handle missing its sigil (ambiguous with
            // prose — the model must hand back what it was shown).
            cite_input("^", 4, true, "r"),
            cite_input("^Not A Slug", 4, true, "r"),
            cite_input("license-allowlist", 4, true, "r"),
            // Score out of the 1-5 range.
            cite_input(NOD_A, 0, true, "r"),
            cite_input(NOD_A, 6, true, "r"),
            // Missing/empty remark and missing truthful flag.
            cite_input(NOD_A, 4, true, "  "),
            serde_json::json!({"memory_id": NOD_A, "useful_score": 4, "remark": "r"}),
            serde_json::json!({"useful_score": 4, "truthful": true, "remark": "r"}),
        ] {
            assert!(tool.execute(&bad, &root).await.is_error(), "{bad}");
        }
        assert!(
            ledger.lock().unwrap().is_empty(),
            "rejected input never lands"
        );
    }
}
