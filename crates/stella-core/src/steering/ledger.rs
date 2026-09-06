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
//!
//! The two halves keep different clocks. The allowance is one turn's, so the
//! spend resets each turn ([`SteeringLedger::open_turn`]). The answer is cache
//! prefix, so it stands while the budget it was asked for stands.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::tools::ToolBudget;

/// What the open turn's block has spent, and the tool budget the session
/// settled.
///
/// Shared by handle (`Arc`). The recall path holds one end. The tool stack
/// holds the other. Every method is cheap. None holds a lock across an
/// await.
#[derive(Debug, Default)]
pub struct SteeringLedger {
    /// What the open turn's block has spent. [`Self::open_turn`] clears it.
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

    /// Start a turn: drop what earlier turns spent, keep what the session
    /// settled.
    ///
    /// The allowance is one turn's, so the figure [`Self::settle`] subtracts
    /// has to be one turn's too. Only the spend is cleared. The settled
    /// answer is cache prefix and outlives every turn that follows it, which
    /// is the byte-stable prompt rule and is what a `/reload` still finds
    /// waiting.
    ///
    /// A reset rather than a store, so that two charges inside one turn add
    /// up. A store would keep the second and lose the first, and the loss
    /// would be silent.
    pub fn open_turn(&self) {
        self.spent.store(0, Ordering::Relaxed);
    }

    /// Note `tokens` of volatile context for the open turn.
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

    /// What the open turn's block has spent so far. [`Self::open_turn`] puts
    /// it back to zero, so this is never a session total.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// The tool budget under `declared`: what the open turn's block left of
    /// it.
    ///
    /// Answered once, then kept. Ask again with the same `declared` and the
    /// answer is the same, however much was spent since. The list is cache
    /// prefix. Moving it mid-session bills the whole chat again. Ask with a
    /// different `declared` and it settles anew, against the turn that is
    /// open then — not against every turn the session has run.
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

    /// A turn starts with nothing spent. What it settled is still there:
    /// the list is cache prefix and a new turn is not a reason to move it.
    #[test]
    fn a_new_turn_clears_the_spend_and_keeps_the_settled_answer() {
        let ledger = SteeringLedger::new();
        ledger.spend(400);
        let settled = ledger.settle(declared(1_000));

        ledger.open_turn();

        assert_eq!(ledger.spent(), 0, "the new turn has spent nothing");
        assert_eq!(ledger.settle(declared(1_000)), settled);
    }

    /// Two charges inside one turn add up. This is why a turn opens with a
    /// reset and not with a store: a store would keep the last charge and
    /// lose the rest of the block, and say nothing.
    #[test]
    fn two_charges_in_one_turn_add_up() {
        let ledger = SteeringLedger::new();
        ledger.open_turn();
        ledger.spend(300);
        ledger.spend(200);

        assert_eq!(ledger.settle(declared(1_000)).max_tokens, 500);
    }

    /// **The witness.** An operator raises the allowance twenty
    /// turns in and gets more room, not less.
    ///
    /// Every turn recalls a distinct block of the same size, so a spend that
    /// carried across turns would read twenty times one block. Against the
    /// raised 12_000 that reads 2_000 — under the 8_000 they started with,
    /// and on a longer session zero, which advertises no tools at all. One
    /// turn's block is 500, so the answer is 11_500.
    #[test]
    fn a_raised_allowance_is_charged_one_turn_not_the_session() {
        const BLOCK: u64 = 500;
        let ledger = SteeringLedger::new();

        for _ in 0..20 {
            ledger.open_turn();
            ledger.spend(BLOCK);
        }
        assert_eq!(ledger.settle(declared(8_000)).max_tokens, 8_000 - BLOCK);

        // The operator edits `context.steering.max_tokens` and reloads.
        ledger.open_turn();
        ledger.spend(BLOCK);

        assert_eq!(ledger.settle(declared(12_000)).max_tokens, 12_000 - BLOCK);
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
