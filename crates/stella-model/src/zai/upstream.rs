//! Which vendor OpenRouter routes a call to, and how to ask for it again.
//!
//! OpenRouter fronts many vendors and picks one per call. Each vendor holds
//! its own prompt cache. A switch throws that cache away, so the whole
//! prompt bills at the full rate.
//!
//! The sticky `session_id` this adapter sends is a hint. It is not a pin.
//! One 174-call turn in this repo's own store (execution 327, one slug,
//! `moonshotai/kimi-k3`) was served by three vendors and the route moved
//! twelve times. Five calls in that turn read no cache at all. Three of the
//! five were the first call on a vendor new to the turn. Those five calls
//! cost $0.97 of the $4.65 the turn spent.
//!
//! So the adapter reads the vendor name off the first answer and asks for
//! that vendor again on every later call. Fallbacks stay on. If the vendor
//! is down the gateway may still serve the call: a dearer call beats a dead
//! one, and a warm cache is a saving, not a rule.
//!
//! An operator who sets `upstream_pin` asked for the stronger thing — one
//! vendor or nothing. That setting still wins, and it still bars fallbacks.
//!
//! One part of this is not proven. The gateway names a vendor by its display
//! name, and this code sends that same name back. Whether the gateway routes
//! on it is untested, and no mock can test it: a mock takes any string.
//! Fallbacks are on, so a name the gateway cannot place costs the saving and
//! not the call.

use std::sync::OnceLock;

use serde::Serialize;

use super::ZaiProvider;

/// OpenRouter's `provider` routing object, in the two shapes this adapter
/// sends.
///
/// An operator pin bars fallbacks. A measured run has to fail out loud
/// rather than swap vendors part way, because a swapped trial reads just
/// like a clean one.
///
/// A learned pin allows them. Nobody asked for that vendor by name; the
/// adapter only wants the cache the first call already paid to write.
/// Killing the call to protect a saving is a bad trade.
#[derive(Serialize)]
pub(super) struct OpenRouterProviderPin<'a> {
    pub(super) order: &'a [String],
    pub(super) allow_fallbacks: bool,
}

/// The vendor that served this session's first answer.
///
/// Set once, then never moved. A later call the gateway routes elsewhere
/// does not change it: the cache worth asking for is the one the first
/// answer wrote.
#[derive(Debug, Default)]
pub(super) struct ServedUpstream(OnceLock<Vec<String>>);

impl ServedUpstream {
    /// Keep `served` if nothing is kept yet.
    ///
    /// A missing or blank name is not an answer, so it is dropped. Every
    /// direct endpoint sends none, and a gateway may decline to say.
    pub(super) fn learn(&self, served: Option<&str>) {
        let Some(name) = served.map(str::trim).filter(|name| !name.is_empty()) else {
            return;
        };
        if self.0.get().is_none() {
            // Two threads can reach this at once. One wins and the other's
            // `set` fails. Both wanted a served name in the slot, so either
            // winner is right.
            let _ = self.0.set(vec![name.to_string()]);
        }
    }

    /// The order to send, or `None` while no answer has named a vendor.
    fn order(&self) -> Option<&[String]> {
        self.0.get().map(Vec::as_slice)
    }
}

impl ZaiProvider {
    /// The `provider` object for the next request body.
    ///
    /// It stays off the wire unless this really is OpenRouter. No other
    /// chat-completions server knows the key, and an unknown key risks a
    /// hard 400. It also stays off until an answer has named a vendor, so a
    /// session's first request keeps the bytes it has always had.
    pub(super) fn routing_pin(&self) -> Option<OpenRouterProviderPin<'_>> {
        if !self.serves_openrouter() {
            return None;
        }
        if !self.upstream_pin.is_empty() {
            return Some(OpenRouterProviderPin {
                order: &self.upstream_pin,
                allow_fallbacks: false,
            });
        }
        self.served_upstream
            .order()
            .map(|order| OpenRouterProviderPin {
                order,
                allow_fallbacks: true,
            })
    }
}
