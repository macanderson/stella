//! The deck's half of the withheld-steering notice (#2302, #3616, #4463).
//!
//! `agent::output::open_raw_turn` announces it on the raw door, where the
//! event lands beside the turn it applies to. The deck cannot do that: the
//! refusal is a **session** fact, established before any turn opens, and
//! sending it from `run_lead_turn` would repeat it on every prompt.
//!
//! So it is announced once, during session boot, and it goes on the
//! transcript rather than through `system_notice`: a startup dialog is
//! dismissed by the next keystroke, and "this repository's memories, rules,
//! skills, commands and agents were not loaded" is a fact a user needs to be
//! able to scroll back to when the session behaves unlike the one they
//! expected. Under the alternate screen the stderr line the notice otherwise
//! relies on is swallowed entirely, so without this the deck said nothing at
//! all.
//!
//! Lives here rather than at the call site because `command_deck.rs` is a god
//! file closed to growth (AGENTS.md § "God files"); the boot keeps one line.

use tokio::sync::mpsc::UnboundedSender;

use stella_tui::Inbound;

use super::LEAD;
use crate::config::Config;

/// Put this session's withheld-steering notice on the lead's transcript, if
/// there is one.
///
/// Silent on both of the `None` arms `settings::withheld` already
/// distinguishes: a checkout whose steering **was** loaded has nothing to be
/// told, and one with **nothing to withhold** must not warn about a
/// suppression that cost it nothing.
///
/// **Call it before the boot's `Status::WaitingInput`.** An `Inbound::Event`
/// folds the lead to `Running`, which is what that assertion exists to
/// correct for the rest of the startup chrome.
pub(super) fn announce_withheld(cfg: &Config, in_tx: &UnboundedSender<Inbound>) {
    let Some(withheld) = cfg.authority.withheld.as_ref() else {
        return;
    };
    // The same session latch the raw door spends (#4500), so a deck that
    // later drives a raw turn does not say this twice. Claimed after the
    // `None` arm above, never before: a session with nothing withheld must
    // not spend an announcement it was never owed.
    if !crate::agent::claim_withheld_announcement() {
        return;
    }
    let _ = in_tx.send(Inbound::Event {
        agent: LEAD.to_string(),
        event: withheld.event(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{AgentEvent, Withholder};

    fn config() -> Config {
        Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string())
    }

    /// The session latch [`announce_withheld`] spends is process state
    /// (`agent::output`), so a test that spends it takes it first — see
    /// `crate::agent::latch_for_withheld_test`.
    fn drain(cfg: &Config) -> Vec<Inbound> {
        let _latch = crate::agent::latch_for_withheld_test();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        announce_withheld(cfg, &tx);
        drop(tx);
        let mut out = Vec::new();
        while let Ok(inbound) = rx.try_recv() {
            out.push(inbound);
        }
        out
    }

    /// **The witness (#4463).** The deck's boot puts the refusal on the
    /// lead's transcript, carrying the authority the row's remedy is derived
    /// from.
    #[test]
    fn the_deck_boot_announces_a_withheld_checkout_on_the_transcript() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".stella/memories")).expect("memories");
        std::fs::write(dir.path().join(".stella/memories/a.md"), "lesson").expect("memory");

        let mut cfg = config();
        cfg.authority.withheld =
            crate::settings::withheld_notice(dir.path(), Some(Withholder::ManagedCeiling));
        assert!(cfg.authority.withheld.is_some(), "the fixture withholds");

        match drain(&cfg).as_slice() {
            [Inbound::Event { agent, event }] => {
                assert_eq!(agent, LEAD);
                assert!(
                    matches!(
                        event,
                        AgentEvent::SteeringWithheld {
                            withheld_by: Withholder::ManagedCeiling,
                            memories: 1,
                            ..
                        }
                    ),
                    "{event:?}"
                );
            }
            other => panic!("expected one transcript event, got {other:?}"),
        }
    }

    /// A session whose steering loaded normally is told nothing — a notice
    /// printed in every repository is one nobody reads.
    #[test]
    fn a_trusted_checkout_is_announced_nothing() {
        assert!(drain(&config()).is_empty());
    }
}
