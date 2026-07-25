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
//! that only fires when the provider key *and* `STELLA_MEDIA_LIVE=1` are
//! present — so CI never calls a paid API, yet a keyed release run exercises
//! the real wire (L-V4).

pub mod openai_image;
pub mod zai_image;
pub mod zai_video;

/// Whether the paid live smokes are armed. `STELLA_MEDIA_LIVE=1` is the
/// spelling that matches every other `STELLA_*` toggle; the pre-rename
/// `OXAGEN_MEDIA_LIVE` is accepted as a deprecated alias for one release.
#[cfg(test)]
pub(crate) fn live_smokes_armed() -> bool {
    std::env::var_os("STELLA_MEDIA_LIVE")
        .or_else(|| std::env::var_os("OXAGEN_MEDIA_LIVE"))
        .is_some()
}

pub use openai_image::OpenAiImageProvider;
pub use zai_image::ZaiImageProvider;
pub use zai_video::ZaiVideoProvider;
