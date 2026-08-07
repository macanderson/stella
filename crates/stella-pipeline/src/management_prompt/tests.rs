//! Cross-role witnesses for the management system-prefix structure
//! (#1434, #1855).
//!
//! The per-role halves live beside their builders — `verify/tests.rs` pins
//! the verdict and guidance instruction blocks byte-stable across calls,
//! `triage/tests.rs` keeps volatile evidence out of the triage block. What
//! no per-role test can see is the FAMILY property #1855 turns on: whether
//! the roles' system blocks open with one byte-identical preamble a
//! provider prompt cache could serve across roles. This module declares
//! that shared preamble and holds every management system block to it,
//! parity-style (declared, not assumed — the invariant-8 shape): the
//! declaration must equal what the blocks actually share, so a preamble
//! authored but adopted by only some roles, and accidental sharing grown
//! without being declared, both fail here by name.

use stella_protocol::{MessageRole, ModelCallRole};

use super::ManagementPrompt;
use crate::pipeline::CONVERSATIONAL_SYSTEM_PROMPT;
use crate::triage::triage_prompt;
use crate::verify::diff_render::DiffContext;
use crate::verify::{guidance_prompt, verifier_prompt};

/// The preamble every management system block is declared to open with,
/// byte-for-byte.
///
/// Empty is the measured truth today (#1855): no shared preamble has been
/// authored, the fixed blocks are disjoint, and none clears a provider's
/// minimum cacheable prefix on its own (see the `management_prompt` module
/// docs). When #1855 option 1 authors the shared preamble, updating this
/// constant is the whole adoption contract — the parity witness below then
/// refuses any role that does not open with it.
const SHARED_MANAGEMENT_PREAMBLE: &str = "";

/// Every [`ModelCallRole`] the crate can dispatch, for enumerating the
/// family. Completeness is not compiler-checked here — that job belongs to
/// the exhaustive match in [`management_system_block`], which forces a new
/// variant to declare its prefix posture before this array matters.
const ALL_ROLES: [ModelCallRole; 15] = [
    ModelCallRole::Unknown,
    ModelCallRole::Triage,
    ModelCallRole::Plan,
    ModelCallRole::PlanRepair,
    ModelCallRole::WitnessAuthor,
    ModelCallRole::WitnessRepair,
    ModelCallRole::Worker,
    ModelCallRole::DistressGuidance,
    ModelCallRole::Verdict,
    ModelCallRole::AgentAuthor,
    ModelCallRole::SkillAuthor,
    ModelCallRole::DomainInference,
    ModelCallRole::Reflection,
    ModelCallRole::Summarization,
    ModelCallRole::Research,
];

/// The system block a role dispatches through the management chokepoint
/// (`metered_raw_call`), or `None` where the role sends no system message.
///
/// Exhaustive over [`ModelCallRole`] for the same reason `management_bounds`
/// is: a new role routed through the chokepoint must declare its prefix
/// posture here, not escape the family witness by omission.
fn management_system_block(role: ModelCallRole) -> Option<String> {
    let ctx = DiffContext::default();
    match role {
        ModelCallRole::Triage => Some(triage_prompt("goal", "src/lib.rs").instructions.to_string()),
        ModelCallRole::Verdict => Some(
            verifier_prompt("goal", "+a\n", "no flip", &ctx)
                .instructions
                .to_string(),
        ),
        ModelCallRole::DistressGuidance => Some(
            guidance_prompt("goal", "+a\n", "red twice", &ctx)
                .instructions
                .to_string(),
        ),
        // The conversational fast path is the only raw `Worker` dispatch.
        ModelCallRole::Worker => Some(CONVERSATIONAL_SYSTEM_PROMPT.to_string()),
        // Plan and PlanRepair ride the chokepoint as a single user message
        // with no system block at all: their fixed instructions are
        // interpolated into the volatile payload — the pre-#1434 shape,
        // structurally uncacheable. Recorded as a gap on #1855; when they
        // adopt the split these arms move to `Some(...)` and the roles join
        // the parity witness automatically.
        ModelCallRole::Plan | ModelCallRole::PlanRepair => None,
        // Never dispatched through the management chokepoint. `Research`
        // (#1778) rides the sub-agent primitive — its system prompt travels
        // on the `SubAgentSpec`, not through `metered_raw_call`.
        ModelCallRole::Unknown
        | ModelCallRole::Research
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization
        | ModelCallRole::Research => None,
    }
}

/// The `(role, system block)` pairs for every role that carries one.
fn management_system_blocks() -> Vec<(ModelCallRole, String)> {
    ALL_ROLES
        .into_iter()
        .filter_map(|role| management_system_block(role).map(|block| (role, block)))
        .collect()
}

/// The longest common prefix of two strings, on `char` boundaries.
fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a
        .char_indices()
        .zip(b.chars())
        .find(|((_, ca), cb)| ca != cb)
        .map(|((i, _), _)| i)
        .unwrap_or_else(|| a.len().min(b.len()));
    &a[..end]
}

/// The wire shape every management prompt serializes to: exactly one system
/// block carrying the fixed instructions, then the volatile payload as the
/// user message. This order is what puts the stable bytes where a provider
/// adapter's cache plumbing can mark them (#1434).
#[test]
fn the_wire_shape_is_one_system_block_then_the_payload() {
    let msgs = ManagementPrompt {
        instructions: "fixed",
        payload: "volatile".to_string(),
    }
    .into_messages();
    assert_eq!(msgs.len(), 2);
    assert!(matches!(msgs[0].role, MessageRole::System));
    assert_eq!(msgs[0].content, "fixed");
    assert!(matches!(msgs[1].role, MessageRole::User));
    assert_eq!(msgs[1].content, "volatile");
}

/// Every management system block opens with the declared shared preamble.
/// Vacuously green while the preamble is empty; the moment #1855 option 1
/// authors it, this is the assertion that refuses a role left behind.
#[test]
fn every_management_system_block_opens_with_the_declared_preamble() {
    for (role, block) in management_system_blocks() {
        assert!(
            block.starts_with(SHARED_MANAGEMENT_PREAMBLE),
            "{role:?}'s system block must open with the declared shared preamble"
        );
    }
}

/// The declaration is exact, not a lower bound: the longest common prefix
/// the family actually shares equals [`SHARED_MANAGEMENT_PREAMBLE`]. Today
/// both sides are empty — the measured #1855 problem, stated as a test.
/// This fails in both interesting directions: an authored preamble the
/// constant does not record (the option-1 PR must update the declaration in
/// the same change), and sharing that grows by accident (cache-relevant
/// bytes appearing without being declared).
#[test]
fn the_declared_preamble_is_exactly_what_the_family_shares() {
    let blocks = management_system_blocks();
    assert!(
        blocks.len() >= 2,
        "the family witness needs at least two roles to compare"
    );
    let lcp = blocks[1..]
        .iter()
        .fold(blocks[0].1.as_str(), |acc, (_, block)| {
            common_prefix(acc, block)
        });
    assert_eq!(
        lcp, SHARED_MANAGEMENT_PREAMBLE,
        "the shared management preamble must be declared, not assumed: \
         update SHARED_MANAGEMENT_PREAMBLE to the bytes the roles actually share"
    );
}
