// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive recovery from a provider refusing the requested output ceiling.
//!
//! This module is `pub` for exactly one item — [`SessionOutputCeilings`], the
//! handle a host names to attach a session's learned ceilings to
//! [`crate::EngineConfig`]. The ladder itself is `pub(crate)`: it is engine
//! machinery, not a seam.
//!
//! The output-side mirror of `super::overflow_recovery`, and it exists for
//! the same reason: a rejection the engine can repair was being surfaced as
//! a terminal error that ended the turn.
//!
//! A gateway prices a request against the account balance using the ceiling
//! the caller *asks for*, not the tokens it will actually spend. So a
//! session that asks for `max_output_tokens: 128000` and typically emits a
//! few thousand is refused the moment the balance drops below the price of
//! the ask — while still holding credit enough for dozens of real calls.
//! OpenRouter says so in as many words:
//!
//! ```text
//! This request requires more credits, or fewer max_tokens. You requested
//! up to 128000 tokens, but can only afford 117676.
//! ```
//!
//! That is an instruction, not a wall. Before this module it arrived as
//! `ProviderError::Terminal` and killed the turn: three benchmark runs were
//! lost or maimed that way — every trial dead against a balance that could
//! have funded them all at a ceiling the provider itself named.
//!
//! # The ladder
//!
//! When a call fails with
//! [`stella_protocol::ProviderError::OutputBudgetExceeded`], the engine
//! clamps `max_output_tokens` for the rest of the turn and re-runs the step.
//! The clamp is the provider's own stated affordable ceiling less
//! `SAFETY_MARGIN_PERCENT` — a margin rather than the bare figure because
//! the balance is falling as the turn spends, so the number that was exactly
//! affordable when the provider computed it is not affordable by the time
//! the retry lands. When the provider names no figure, the ask is halved:
//! the engine reduces its own request rather than inventing a number the
//! provider never stated.
//!
//! # Bounds and the death-spiral guard
//!
//! Identical in shape to the overflow ladder, deliberately: a **per-turn
//! down-only latch**, at most `MAX_RECOVERY_RUNGS` rungs, the counter never
//! resetting within the turn, and each clamp monotonically tighter than the
//! last. The clamp never loosens once set, even after a later call commits,
//! so a configured ceiling above what the account can fund cannot oscillate
//! ask → refuse → recover for the rest of the turn. `FLOOR_TOKENS` stops
//! the ladder before the ceiling gets too small to hold an answer: below
//! that the account genuinely cannot fund the call, and the rejection
//! surfaces exactly as an unrecovered terminal failure does.
//!
//! Like the overflow latch, this is not checkpointed: a resumed turn starts
//! the allowance over, which only re-permits a bounded amount of work.
//!
//! # Why the clamp also outlives the turn
//!
//! The ladder above is per-turn because its *rungs* are: three refusals in one
//! turn is a death spiral, and the counter has to reset or a long session
//! eventually runs out of recoveries. But the fact the ladder learns — "this
//! account cannot fund a ceiling this large" — is a property of the balance,
//! not of the turn. Resetting it with the turn meant every turn re-sent the
//! original ceiling, ate a 402, and only then retried reduced: on a bench
//! trial with dozens of turns, dozens of wasted request/reject round-trips,
//! each one pure latency (a 402 bills nothing) and each one more load against
//! the gateway's rate limiter (#3307).
//!
//! [`SessionOutputCeilings`] is the carry that fixes it — the clamp survives
//! the turn while the rung allowance still resets with it. It is keyed by
//! provider id, because affordability is a fact about one account at one
//! gateway: a clamp learned from a gateway that prices the ask must not
//! shorten answers on a direct vendor that bills what is spent.
//!
//! It is **optimistically forgotten** every [`REPROBE_TURNS`] turns rather
//! than held forever. Nothing observable tells the engine a balance was topped
//! up — the only way to find out is to ask for the full ceiling again — so the
//! carry decays into one re-probe, which either passes (the session is back to
//! its configured ceiling for free) or is refused (the ladder re-learns, at
//! the cost of the single round-trip this module exists to stop paying every
//! turn).

/// How many turns a learned ceiling survives before the session optimistically
/// forgets it and re-asks for the caller's full ceiling.
///
/// This is a judgement between two costs, not a measured optimum. Too short
/// and the carry stops paying for itself: at one it *is* the per-turn
/// re-ask #3307 is about. Too long and a mid-session top-up leaves the session
/// answering under a ceiling it could now afford — which binds for real, since
/// a low balance yields a low clamp and every answer is cut to it. Eight
/// removes seven of every eight wasted round-trips while honouring a top-up
/// within a few minutes of ordinary work.
pub const REPROBE_TURNS: u32 = 8;

/// How many clamp rungs may fire per turn. Three, one more than the overflow
/// ladder's two, because each rung here answers a *different* stated figure
/// rather than a fixed schedule — the balance falls as the turn spends, so
/// the provider's second answer is legitimately smaller than its first, and
/// stopping at two would abandon a turn the third rung would have carried.
pub(crate) const MAX_RECOVERY_RUNGS: u8 = 3;

/// How far below the provider's stated affordable ceiling to clamp, in
/// percent. The stated figure is computed against a balance that keeps
/// falling while the turn runs, so asking for exactly it is asking to be
/// refused again one rung later — spending a rung to learn nothing.
pub(crate) const SAFETY_MARGIN_PERCENT: u32 = 10;

/// The ceiling below which recovery stops. An answer that must fit in fewer
/// tokens than this is not an answer worth buying a rung for, and a balance
/// that cannot fund this much output is out of credit in every sense that
/// matters — which is the terminal failure, correctly surfaced.
pub(crate) const FLOOR_TOKENS: u32 = 1024;

/// Per-turn latch and output-ceiling clamp. Pure data, no I/O (invariant 2);
/// lives on [`crate::step::TurnState`] and dies with the turn.
#[derive(Debug, Default)]
pub(crate) struct OutputBudgetRecovery {
    /// Rungs fired this turn. Monotone — never reset, even by a committed
    /// call, so the ladder cannot re-arm (the death-spiral guard above).
    rungs_fired: u8,
    /// The standing output ceiling, once any rung has fired. Monotone
    /// downward for the rest of the turn.
    clamp_tokens: Option<u32>,
}

impl OutputBudgetRecovery {
    /// Apply the standing clamp to a configured ceiling. The identity until
    /// a rung fires, and a `min` after — never a raise, so a config value
    /// below the clamp keeps its own smaller number.
    ///
    /// `None` (no configured ceiling — let the provider decide) becomes the
    /// clamp itself once armed: the whole point of the rung is that the
    /// provider's own default is what it just refused to fund.
    pub(crate) fn apply(&self, configured: Option<u32>) -> Option<u32> {
        match (self.clamp_tokens, configured) {
            (None, configured) => configured,
            (Some(clamp), Some(configured)) => Some(clamp.min(configured)),
            (Some(clamp), None) => Some(clamp),
        }
    }

    /// Arm the next rung against what the provider said it could afford, or
    /// `None` when the ladder is spent — the caller then surfaces the
    /// rejection terminally.
    ///
    /// `affordable` is the provider's stated ceiling when it named one.
    /// `asked` is what this turn actually requested, so a provider that
    /// named nothing can still be answered with half of the ask.
    pub(crate) fn arm(&mut self, affordable: Option<u32>, asked: Option<u32>) -> Option<u32> {
        if self.rungs_fired >= MAX_RECOVERY_RUNGS {
            return None;
        }
        let proposed = match affordable {
            Some(stated) => stated.saturating_sub(stated / 100 * SAFETY_MARGIN_PERCENT),
            // No figure from the provider: halve whatever ceiling is
            // currently in force. `asked` is the configured value, already
            // narrowed by any standing clamp, so successive rungs keep
            // halving rather than re-halving the original.
            None => self.apply(asked).map(|current| current / 2)?,
        };
        // Monotonically tighter, always: a provider that re-states a figure
        // *larger* than the standing clamp is answering about a balance that
        // has since changed, and loosening on it is the oscillation the
        // death-spiral guard exists to prevent.
        let clamped = match self.clamp_tokens {
            Some(standing) => proposed.min(standing.saturating_sub(1)),
            None => proposed,
        };
        if clamped < FLOOR_TOKENS {
            return None;
        }
        self.rungs_fired += 1;
        self.clamp_tokens = Some(clamped);
        Some(clamped)
    }

    /// Rungs fired so far — the `n` in the "recovery n of MAX" notice.
    pub(crate) fn fired(&self) -> u8 {
        self.rungs_fired
    }
}

/// The session-scoped carry for what the per-turn ladder above learned: one
/// standing output ceiling per provider id, surviving the turn that learned it
/// (module docs, #3307).
///
/// Shared by every engine a session builds — a handle cloned into each
/// per-turn `EngineConfig`, the same shape `stella-cli`'s `SessionDurability`
/// uses to reach a per-turn config from session-scoped state. Interior
/// mutability rather than `&mut` for the same reason
/// `stella_model::stream_recovery::StreamRecovery` uses it: the engine holds
/// its config by shared reference, and this is read on the request path and
/// written on the failure path of the same turn.
///
/// Pure data, no I/O (invariant 2). A session touches a handful of providers,
/// so a `Vec` scanned linearly beats a map it would never fill.
///
/// # Byte-stability
///
/// Until a rung arms, every method is the identity: [`Self::narrow`] hands
/// back the configured ceiling unchanged, so a session that never meets a 402
/// sends byte-identical requests to one that has never heard of this type
/// (invariant 7). `EngineConfig` leaves the handle unattached by default, so
/// an unwired host is unchanged twice over.
#[derive(Debug, Default)]
pub struct SessionOutputCeilings {
    entries: std::sync::Mutex<Vec<Entry>>,
}

/// One provider's standing ceiling and how long it has stood.
#[derive(Debug)]
struct Entry {
    provider: String,
    clamp: u32,
    /// Turns elapsed since this ceiling was learned or re-learned. At
    /// [`REPROBE_TURNS`] the entry is forgotten and the next call re-asks for
    /// the caller's full ceiling.
    turns_stood: u32,
}

impl SessionOutputCeilings {
    /// A turn is starting: age every standing ceiling and forget the ones that
    /// have stood [`REPROBE_TURNS`] turns.
    ///
    /// Called from `TurnState::new`/`from_checkpoint`, which is every path
    /// that mints a turn — the borrowed-transcript driver, a resume, and a
    /// durable host owning its own state — so no driver can opt out of the
    /// decay and leave a session capped for good.
    ///
    /// Deliberately ages *every* entry rather than only the provider about to
    /// run: lanes sharing one handle then age it faster than one lane would,
    /// which spends re-probes sooner. That is the safe direction — a re-probe
    /// costs at most the one round-trip, while a stale ceiling silently
    /// shortens answers.
    pub fn begin_turn(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.retain_mut(|entry| {
            entry.turns_stood += 1;
            entry.turns_stood < REPROBE_TURNS
        });
    }

    /// Narrow a configured ceiling by `provider`'s standing one. The identity
    /// when nothing is standing, and a `min` otherwise — never a raise, so a
    /// caller already asking for less keeps its own smaller number, exactly as
    /// `OutputBudgetRecovery::apply` does within the turn.
    ///
    /// `None` (no configured ceiling — let the provider decide) becomes the
    /// standing ceiling: the provider's own default is what it refused.
    #[must_use]
    pub fn narrow(&self, provider: &str, configured: Option<u32>) -> Option<u32> {
        let Some(standing) = self.standing(provider) else {
            return configured;
        };
        Some(match configured {
            Some(configured) => standing.min(configured),
            None => standing,
        })
    }

    /// Record what a rung just settled on for `provider`, and restart its
    /// re-probe clock.
    ///
    /// Monotonically tighter within the standing entry, mirroring the ladder:
    /// a later, larger figure describes a balance that has since changed, and
    /// honouring it here is the oscillation the per-turn latch already
    /// refuses. Raising happens only through [`Self::begin_turn`] forgetting
    /// the entry outright, which is the one path that re-asks the provider
    /// instead of guessing.
    pub fn learn(&self, provider: &str, clamp: u32) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        match entries.iter_mut().find(|e| e.provider == provider) {
            Some(entry) => {
                entry.clamp = entry.clamp.min(clamp);
                entry.turns_stood = 0;
            }
            None => entries.push(Entry {
                provider: provider.to_string(),
                clamp,
                turns_stood: 0,
            }),
        }
    }

    /// `provider`'s standing ceiling, if one is.
    #[must_use]
    pub fn standing(&self, provider: &str) -> Option<u32> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|e| e.provider == provider)
            .map(|e| e.clamp)
    }
}

impl super::Engine<'_> {
    /// The output ceiling this engine's next call may ask for *before* the
    /// turn's own ladder narrows it further: the configured value, cut by
    /// whatever this session already learned the active provider will fund
    /// (#3307).
    ///
    /// This is what makes the carry pay: without it a turn's first call
    /// re-sends the configured ceiling, is refused, and only the *retry* goes
    /// out reduced — one wasted round-trip per turn for as long as the balance
    /// stays low.
    ///
    /// Keyed on [`Engine::active_provider`] rather than the constructor-time
    /// primary, so a turn that failed over mid-flight (`driver::model_fallback`)
    /// is narrowed by what the *replacement* refused, not by what the dead
    /// primary did.
    ///
    /// [`Engine::active_provider`]: super::Engine::active_provider
    pub(crate) fn configured_output_ceiling(&self) -> Option<u32> {
        let configured = self.config.max_output_tokens;
        match self.config.session_output_ceilings.as_ref() {
            Some(ceilings) => ceilings.narrow(self.active_provider().id(), configured),
            None => configured,
        }
    }

    /// Carry a rung's clamp into the session, so the next turn's first call
    /// goes out already reduced instead of buying the same 402 again.
    pub(crate) fn carry_output_ceiling(&self, clamp: u32) {
        if let Some(ceilings) = self.config.session_output_ceilings.as_ref() {
            ceilings.learn(self.active_provider().id(), clamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded bench failure, replayed: a 128K ask against a balance
    /// that could afford 117676 must become a smaller ask, not a dead turn.
    #[test]
    fn the_stated_ceiling_becomes_a_clamp_below_it() {
        let mut recovery = OutputBudgetRecovery::default();
        let clamp = recovery.arm(Some(117_676), Some(128_000)).unwrap();
        assert!(clamp < 117_676, "must sit below the stated figure: {clamp}");
        assert_eq!(recovery.apply(Some(128_000)), Some(clamp));
        assert_eq!(recovery.fired(), 1);
    }

    /// A margin, not the bare figure: the balance keeps falling while the
    /// turn runs, so asking for exactly what was affordable a moment ago
    /// spends a rung to be refused again.
    #[test]
    fn the_clamp_keeps_a_margin_under_the_stated_figure() {
        let mut recovery = OutputBudgetRecovery::default();
        let clamp = recovery.arm(Some(100_000), Some(128_000)).unwrap();
        assert_eq!(clamp, 90_000);
    }

    /// A provider that names no figure still gets a smaller ask — the engine
    /// halves its own, rather than inventing one the provider never stated.
    #[test]
    fn an_unnamed_ceiling_halves_the_ask() {
        let mut recovery = OutputBudgetRecovery::default();
        assert_eq!(recovery.arm(None, Some(64_000)), Some(32_000));
        assert_eq!(recovery.arm(None, Some(64_000)), Some(16_000));
    }

    /// Down-only: a later, larger stated figure is about a balance that has
    /// since changed, and honouring it is the ask → refuse → recover
    /// oscillation the latch exists to prevent.
    #[test]
    fn the_clamp_never_loosens() {
        let mut recovery = OutputBudgetRecovery::default();
        let first = recovery.arm(Some(50_000), Some(128_000)).unwrap();
        let second = recovery.arm(Some(120_000), Some(128_000)).unwrap();
        assert!(second < first, "{second} must be under {first}");
    }

    /// The ladder is bounded, and a spent ladder surfaces terminally.
    #[test]
    fn the_ladder_is_spent_after_max_rungs() {
        let mut recovery = OutputBudgetRecovery::default();
        for _ in 0..MAX_RECOVERY_RUNGS {
            assert!(recovery.arm(Some(100_000), Some(128_000)).is_some());
        }
        assert_eq!(recovery.arm(Some(100_000), Some(128_000)), None);
    }

    /// A balance that cannot fund a usable answer is out of credit in the
    /// sense that matters, and must surface as the terminal failure it is
    /// rather than buying rungs down to nothing.
    #[test]
    fn a_ceiling_under_the_floor_does_not_arm() {
        let mut recovery = OutputBudgetRecovery::default();
        assert_eq!(recovery.arm(Some(200), Some(128_000)), None);
        assert_eq!(recovery.fired(), 0);
    }

    /// The clamp narrows a configured ceiling and never raises one: a
    /// caller already asking for less than the clamp keeps its own figure.
    #[test]
    fn apply_narrows_but_never_raises() {
        let mut recovery = OutputBudgetRecovery::default();
        recovery.arm(Some(100_000), Some(128_000)).unwrap();
        assert_eq!(recovery.apply(Some(4_096)), Some(4_096));
        assert_eq!(recovery.apply(None), Some(90_000));
    }

    /// Until something is learned the carry is the identity, on every
    /// provider — the byte-stability contract (invariant 7).
    #[test]
    fn an_empty_carry_narrows_nothing() {
        let ceilings = SessionOutputCeilings::default();
        assert_eq!(ceilings.narrow("openrouter", Some(128_000)), Some(128_000));
        assert_eq!(ceilings.narrow("openrouter", None), None);
        ceilings.begin_turn();
        assert_eq!(ceilings.narrow("openrouter", Some(128_000)), Some(128_000));
    }

    /// What one turn learned narrows the next, and — as with the per-turn
    /// ladder — never raises a caller already asking for less.
    #[test]
    fn a_learned_ceiling_narrows_later_turns_but_never_raises_them() {
        let ceilings = SessionOutputCeilings::default();
        ceilings.learn("openrouter", 90_000);
        assert_eq!(ceilings.narrow("openrouter", Some(128_000)), Some(90_000));
        assert_eq!(ceilings.narrow("openrouter", Some(4_096)), Some(4_096));
        // No configured ceiling means "let the provider decide", and the
        // provider's own default is precisely what it refused to fund.
        assert_eq!(ceilings.narrow("openrouter", None), Some(90_000));
    }

    /// Affordability is a fact about one account at one gateway. A clamp
    /// learned from a gateway that prices the ask must not shorten answers on
    /// a direct vendor that bills what is spent.
    #[test]
    fn a_ceiling_is_keyed_to_the_provider_that_refused_it() {
        let ceilings = SessionOutputCeilings::default();
        ceilings.learn("openrouter", 90_000);
        assert_eq!(ceilings.narrow("openrouter", Some(128_000)), Some(90_000));
        assert_eq!(ceilings.narrow("anthropic", Some(128_000)), Some(128_000));
    }

    /// Down-only while it stands, mirroring the ladder: a later, larger
    /// figure describes a balance that has since changed, and honouring it
    /// here is the oscillation the per-turn latch already refuses.
    #[test]
    fn learning_again_only_ever_tightens_a_standing_ceiling() {
        let ceilings = SessionOutputCeilings::default();
        ceilings.learn("openrouter", 90_000);
        ceilings.learn("openrouter", 120_000);
        assert_eq!(ceilings.standing("openrouter"), Some(90_000));
        ceilings.learn("openrouter", 45_000);
        assert_eq!(ceilings.standing("openrouter"), Some(45_000));
    }

    /// The decay. Nothing observable announces a top-up, so a standing
    /// ceiling is optimistically forgotten after `REPROBE_TURNS` turns and
    /// the next call re-asks the caller's full ceiling.
    #[test]
    fn a_standing_ceiling_is_forgotten_after_the_reprobe_period() {
        let ceilings = SessionOutputCeilings::default();
        ceilings.learn("openrouter", 90_000);
        for turn in 1..REPROBE_TURNS {
            ceilings.begin_turn();
            assert_eq!(
                ceilings.standing("openrouter"),
                Some(90_000),
                "turn {turn} is still inside the period"
            );
        }
        ceilings.begin_turn();
        assert_eq!(ceilings.standing("openrouter"), None, "the period is up");
    }

    /// Re-learning restarts the clock, so a balance that stays low keeps its
    /// ceiling for a fresh period rather than re-probing on a fixed cadence
    /// counted from the first refusal.
    #[test]
    fn re_learning_restarts_the_reprobe_clock() {
        let ceilings = SessionOutputCeilings::default();
        ceilings.learn("openrouter", 90_000);
        for _ in 0..REPROBE_TURNS - 1 {
            ceilings.begin_turn();
        }
        ceilings.learn("openrouter", 80_000);
        for _ in 0..REPROBE_TURNS - 1 {
            ceilings.begin_turn();
        }
        assert_eq!(ceilings.standing("openrouter"), Some(80_000));
    }
}
