// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One budget for the whole volatile block.
//!
//! # Two budgets, and neither could see the other
//!
//! Records, skills and frames were packed against one number. Tool schemas
//! were packed against a second one. Both used the same packer. Both used the
//! same order. But neither knew what the other took. So a rule and a tool
//! schema were never weighed against each other.
//!
//! # Why a shared cell
//!
//! The two costs land at different times. The recall block is written before
//! the turn starts. The tool list is built after, by the driver. No one call
//! sees both. So they meet here. The block spends. The tool layer takes what
//! is left.
//!
//! # The list is settled once
//!
//! The tool list sits ahead of the prompt in every cache. A total that shrank
//! as the session ran would move the list and make the model pay for the
//! whole chat again. So [`SteeringLedger::settle`] answers once per budget
//! and keeps that answer. A later spend is noted and changes nothing. A
//! `/reload` that moves the budget settles a new one. That is a cold write an
//! operator asked for.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::tools::ToolBudget;

/// What one session's block has spent, and the tool budget it settled.
///
/// Shared by handle (`Arc`). The recall path holds one end. The tool stack
/// holds the other. Every method is cheap. None holds a lock across an
/// await.
#[derive(Debug, Default)]
pub struct SteeringLedger {
    spent: AtomicU64,
    /// The budget this session answered for, and the answer. `None` until
    /// the first [`Self::settle`].
    settled: Mutex<Option<(ToolBudget, ToolBudget)>>,
}

impl SteeringLedger {
    /// A new ledger. It has spent nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Note `tokens` of volatile context.
    ///
    /// The sum saturates. A sum that wrapped would hand the tool layer room
    /// the turn had already used. Past the budget the answer is the same
    /// either way: nothing is left.
    pub fn spend(&self, tokens: u64) {
        // The closure answers `Some` for every input. So there is no
        // failing arm to handle. It retries until the swap wins, and the
        // stored total is the saturated sum.
        let _ = self
            .spent
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(held.saturating_add(tokens))
            });
    }

    /// What this session's block has spent so far.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// The tool budget under `declared`: what the block left of it.
    ///
    /// Answered once, then kept. Ask again with the same `declared` and the
    /// answer is the same, however much was spent since. The list is cache
    /// prefix. Moving it mid-session bills the whole chat again. Ask with a
    /// different `declared` and it settles anew.
    ///
    /// `mcp_max_tokens` shrinks too. It is a share of the budget. Left at its
    /// full size it would let one server take more than the whole
    /// remainder.
    #[must_use]
    pub fn settle(&self, declared: ToolBudget) -> ToolBudget {
        let mut held = self.settled.lock().unwrap_or_else(|poisoned| {
            // A poisoned lock means a panic during an earlier settle. What
            // is stored is two plain numbers, so there is no torn state to
            // distrust. Going on keeps the list stable instead of failing a
            // turn over it.
            poisoned.into_inner()
        });
        if let Some((for_allowance, answer)) = *held
            && for_allowance == declared
        {
            return answer;
        }
        let max_tokens = declared.max_tokens.saturating_sub(self.spent());
        let answer = ToolBudget {
            max_tokens,
            mcp_max_tokens: declared.mcp_max_tokens.min(max_tokens),
        };
        *held = Some((declared, answer));
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(max_tokens: u64) -> ToolBudget {
        ToolBudget {
            max_tokens,
            mcp_max_tokens: max_tokens / 2,
        }
    }

    /// **The witness.** What the block took is gone from the tool budget.
    /// One number, two users.
    #[test]
    fn what_the_block_spends_leaves_the_tool_allowance() {
        let ledger = SteeringLedger::new();
        ledger.spend(400);

        assert_eq!(ledger.settle(declared(1_000)).max_tokens, 600);
    }

    /// A block that spends nothing leaves the budget whole. A session with
    /// no records is the session that had no ledger.
    #[test]
    fn an_unspent_ledger_hands_over_the_whole_allowance() {
        assert_eq!(
            SteeringLedger::new().settle(declared(1_000)),
            declared(1_000)
        );
    }

    /// The sub-cap cannot go past what is left. A server share bigger than
    /// the rest would cap nothing.
    #[test]
    fn the_server_share_is_narrowed_with_the_allowance() {
        let ledger = SteeringLedger::new();
        ledger.spend(900);

        let settled = ledger.settle(declared(1_000));
        assert_eq!((settled.max_tokens, settled.mcp_max_tokens), (100, 100));
    }

    /// **The witness for the stable-prompt rule.** A spend after the answer
    /// changes nothing. The list is cache prefix. Moving it mid-session bills
    /// the whole chat again.
    #[test]
    fn a_later_spend_does_not_move_a_settled_allowance() {
        let ledger = SteeringLedger::new();
        let first = ledger.settle(declared(1_000));
        ledger.spend(900);

        assert_eq!(ledger.settle(declared(1_000)), first);
        assert_eq!(ledger.spent(), 900, "and the spend is still recorded");
    }

    /// A different budget is a reload. It settles anew. That is the one
    /// cold write `tool_lean`'s docs promise an operator who edits the key.
    #[test]
    fn a_changed_allowance_settles_again() {
        let ledger = SteeringLedger::new();
        assert_eq!(ledger.settle(declared(1_000)).max_tokens, 1_000);
        ledger.spend(400);

        assert_eq!(ledger.settle(declared(2_000)).max_tokens, 1_600);
    }

    /// A block bigger than the budget leaves nothing. It must not wrap to a
    /// number that would send the one schema that fits least.
    #[test]
    fn a_block_over_the_allowance_leaves_nothing() {
        let ledger = SteeringLedger::new();
        ledger.spend(u64::MAX);
        ledger.spend(1);

        assert_eq!(ledger.spent(), u64::MAX, "the spend saturates");
        assert_eq!(ledger.settle(declared(1_000)).max_tokens, 0);
    }
}
