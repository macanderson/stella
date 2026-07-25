//! Vendor media adapters. Each implements
//! [`crate::provider::MediaProvider`] for one vendor endpoint and owns its
//! wire types; error classification routes through the shared
//! [`crate::http`] policy so a 401 means the same thing everywhere.
//!
//! Deliberate architecture deviation (documented at the crate root): these
//! vendor HTTP clients live in `stella-media`, not `stella-model`, so this
//! workstream stays self-contained. Folding them into `stella-model`'s
//! provider set is a recorded follow-up.
//!
//! Coverage is recorded-fixture-first (wiremock transcripts, including the
//! failure shapes: auth 401, rate-limit 429, content-policy refusal, and a
//! 404-gone video job), with one runtime-skipped **live smoke** per family
//! that only fires when the provider key *and* `OXAGEN_MEDIA_LIVE=1` are
//! present — so CI never calls a paid API, yet a keyed release run exercises
//! the real wire (L-V4).

pub mod openai_image;
pub mod zai_image;
pub mod zai_video;

pub use openai_image::OpenAiImageProvider;
pub use zai_image::ZaiImageProvider;
pub use zai_video::ZaiVideoProvider;

/// Whether the paid live smokes in this module tree are armed, read from the
/// process environment. The single gate every adapter's `live_smoke_*` test
/// consults, so the three families can never drift apart.
#[cfg(test)]
pub(crate) fn live_smoke_enabled() -> bool {
    live_smoke_armed_by(std::env::var("OXAGEN_MEDIA_LIVE").ok().as_deref())
}

/// The pure half of [`live_smoke_enabled`]: does this reading of
/// `OXAGEN_MEDIA_LIVE` arm the live smokes?
///
/// Requires the LITERAL value `1` — `0`, `false`, `true` and the empty string
/// all leave the smokes skipped. These tests submit real, billable provider
/// jobs (a CogVideoX submit is money), so the switch is deliberately narrower
/// than the repo's other boolean env gates, matching `live_smoke_enabled` in
/// `stella-model/tests/live_smoke.rs`. Split out from the environment read so
/// the gate can be witnessed without `set_var`, which would race every other
/// test thread reading the same variable.
#[cfg(test)]
pub(crate) fn live_smoke_armed_by(value: Option<&str>) -> bool {
    value == Some("1")
}

#[cfg(test)]
mod tests {
    use super::live_smoke_armed_by;

    #[test]
    fn only_the_literal_one_arms_the_paid_live_smokes() {
        assert!(
            live_smoke_armed_by(Some("1")),
            "OXAGEN_MEDIA_LIVE=1 is the documented opt-in and must arm the smokes"
        );
        for value in ["0", "false", "true", "yes", "", " 1", "1 ", "01"] {
            assert!(
                !live_smoke_armed_by(Some(value)),
                "OXAGEN_MEDIA_LIVE={value:?} must NOT arm the paid live smokes"
            );
        }
        assert!(
            !live_smoke_armed_by(None),
            "an unset OXAGEN_MEDIA_LIVE must leave the paid live smokes skipped"
        );
    }
}
