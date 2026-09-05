//! The deck's boot disclosures about steering: what this checkout withheld,
//! and what the learning holdout costs a turn (#2302, #3616, #4463).
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

/// Say what this session's steering is doing that the user did not ask for:
/// what a withheld checkout is not loading, and what the learning holdout
/// holds back.
///
/// One entry point because both are session facts settled before any turn
/// opens, and because the boot keeps one line whatever this grows to say.
pub(super) fn announce_session_steering(cfg: &Config, in_tx: &UnboundedSender<Inbound>) {
    announce_withheld(cfg, in_tx);
    announce_holdout(cfg, in_tx);
}

/// Tell the user what the per-artifact holdout costs.
///
/// A learning loop that pays for its evidence with the user's own turns has
/// to say so. Silent when the holdout is switched off, because a notice about
/// a cost nobody is paying is chrome.
///
/// An `Inbound::Notice` rather than a transcript event: this is the deck
/// telling the user about the session, which is what that channel is for, and
/// unlike the withheld-checkout refusal it names no remedy to scroll back to.
fn announce_holdout(cfg: &Config, in_tx: &UnboundedSender<Inbound>) {
    let rate = crate::memory::session_artifact_holdout_rate(&cfg.workspace_root);
    if let Some(line) = stella_learn::holdout::disclosure(rate, "skill") {
        let _ = in_tx.send(Inbound::Notice(line));
    }
}

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
fn announce_withheld(cfg: &Config, in_tx: &UnboundedSender<Inbound>) {
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

    /// Send the whole boot disclosure over a workspace whose settings pin
    /// `holdout`, and collect what reached the deck.
    fn drain_session(holdout: &str) -> Vec<Inbound> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".stella")).expect("dot stella");
        std::fs::write(
            dir.path().join(".stella/settings.json"),
            format!(r#"{{"context":{{"retrieval":{{"artifact_holdout_rate":{holdout}}}}}}}"#),
        )
        .expect("settings");

        let mut cfg = config();
        cfg.workspace_root = dir.path().to_path_buf();

        let _latch = crate::agent::latch_for_withheld_test();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        announce_session_steering(&cfg, &tx);
        drop(tx);
        let mut out = Vec::new();
        while let Ok(inbound) = rx.try_recv() {
            out.push(inbound);
        }
        out
    }

    /// **The disclosure witness.** A holdout spends the user's own turns, so
    /// the deck's boot names the fraction it spends.
    #[test]
    fn the_deck_boot_discloses_the_holdout_fraction() {
        match drain_session("4").as_slice() {
            [Inbound::Notice(line)] => {
                assert!(line.contains("1 turn in 4"), "{line}");
                assert!(line.contains("skill"), "{line}");
            }
            other => panic!("expected one notice, got {other:?}"),
        }
    }

    /// A workspace that switched the holdout off pays nothing, so it is told
    /// nothing.
    #[test]
    fn a_workspace_with_the_holdout_off_is_told_nothing() {
        assert!(drain_session("0").is_empty());
    }
}
