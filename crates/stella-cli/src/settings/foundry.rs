//! `[foundry]` — the tool foundry's gap-detection thresholds and its
//! autonomy controls (#2471, #5453).
//!
//! A top-level sibling of `tools`, never a child of it: `ToolsSettings` is a
//! flat map of tool name to toggle, and a nested object under `tools` is a
//! tested loud parse error (`a_non_toggle_tools_value_is_a_loud_parse_error`).
//!
//! Every field is optional and absent means the shipped default — the strict
//! post-#2378 floor for detection, `auto` for autonomy, and the 3-consecutive
//! / 50%-of-10 circuit breaker. [`FoundrySettings::resolve`] is the one
//! validation seam: a bad value is a named diagnostic at the read site, never
//! a silently-clamped number, because a threshold that quietly became a
//! different threshold is exactly the settings failure mode #2616 catalogued.

use serde::{Deserialize, Serialize};
use stella_core::tool_foundry::GapDetectionConfig;
use stella_tools::custom::BreakerPolicy;

/// The `foundry` block as a scope's document carries it. Whole-block
/// last-wins across scopes, like `reward`: the thresholds and the autonomy
/// mode are one policy, and a scope holding half of somebody else's is a
/// policy nobody wrote.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct FoundrySettings {
    /// Matching invocations required before a shape is worth proposing.
    /// Default `3`; `0` or `1` disable detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_occurrences: Option<u64>,
    /// Distinct argument sets required. Default `2` — one set is an exact
    /// repeat, which is loop detection's territory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_distinct_arguments: Option<u64>,
    /// Uses per distinct argument set required. Default `3.0`; at or below
    /// `1.0` disables the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_reuse_ratio: Option<f64>,
    /// Whether a cluster needs at least one successful invocation.
    /// Default `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_success: Option<bool>,
    /// Cap on example command lines per proposal and example values per
    /// parameter. Default `3`; must be at least `1`, because the authored
    /// tool's witness input is built from these examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_examples: Option<u64>,
    /// The autonomy kill switch: `"auto"` (detect → author → validate →
    /// adopt → enable, network denied), `"draft-only"` (author and validate,
    /// adopt nothing), or `"off"` (detect and ledger only). Default `"auto"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<FoundryAutonomy>,
    /// Consecutive failures that trip a tool's circuit breaker. Default `3`;
    /// must be at least `1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker_consecutive_failures: Option<u64>,
    /// How many recent invocations the failure-rate arm looks at.
    /// Default `10`; must be at least `1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker_window: Option<u64>,
    /// The failure share over that window that trips the breaker. Default
    /// `0.5`; must be finite, above `0`, and at most `1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker_failure_rate: Option<f64>,
    /// Foundry-built tools allowed to reach the network. Empty by default —
    /// network is denied for every foundry tool unless its name is listed
    /// here. A control, not a ceremony: the entry is a line in a settings
    /// file a reviewer can find.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_allowlist: Option<Vec<String>>,
}

/// The autonomy mode — how far the foundry may carry a detected gap without
/// a human. Spelled as strings in both documents; anything else is a loud
/// parse error, because a typo here silently picks how much code the agent
/// may ship itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoundryAutonomy {
    /// Detect → author → validate → adopt → enable, with network denied at
    /// spawn for every foundry-built tool. The default.
    #[default]
    Auto,
    /// Author and validate only. The staged pair is written under
    /// `.stella/tools/proposed/` and nothing is adopted — the mode the
    /// pipeline also degrades to when the platform has no real network
    /// isolation to offer.
    DraftOnly,
    /// Detect and ledger only; nothing is authored.
    Off,
}

impl FoundryAutonomy {
    /// The canonical spelling, as the settings document writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::DraftOnly => "draft-only",
            Self::Off => "off",
        }
    }
}

impl<'de> Deserialize<'de> for FoundryAutonomy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        match raw.trim() {
            "auto" => Ok(Self::Auto),
            "draft-only" => Ok(Self::DraftOnly),
            "off" => Ok(Self::Off),
            other => Err(serde::de::Error::custom(format!(
                "\"foundry.autonomy\": {other:?} is not one of \"auto\", \"draft-only\", \
                 \"off\""
            ))),
        }
    }
}

impl Serialize for FoundryAutonomy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The `foundry` block with every absence filled from the defaults — what
/// the end-of-turn hook and the autonomy pipeline actually consume.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundryConfig {
    /// The detector thresholds, ready to hand to `detect_tool_gaps`.
    pub detection: GapDetectionConfig,
    /// How far the foundry may carry a gap without a human.
    pub autonomy: FoundryAutonomy,
    /// The auto-disable thresholds — `stella_tools`' own enforcement type,
    /// not a restatement of it, so the knob and the breaker cannot drift.
    pub breaker: BreakerPolicy,
    /// Foundry-built tools allowed to reach the network.
    pub network_allowlist: Vec<String>,
}

impl Default for FoundryConfig {
    fn default() -> Self {
        Self {
            detection: GapDetectionConfig::default(),
            autonomy: FoundryAutonomy::default(),
            breaker: BreakerPolicy::default(),
            network_allowlist: Vec::new(),
        }
    }
}

/// `true` iff `name` matches `^[a-z][a-z0-9_]{1,63}$` — the custom-tool name
/// contract an allowlist entry has to meet to ever match a real tool.
fn is_valid_tool_name(name: &str) -> bool {
    let len = name.len();
    if !(2..=64).contains(&len) {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl FoundrySettings {
    /// Fill the absent fields from the defaults and validate the result.
    ///
    /// The error is the diagnostic a person sees, so it names the key they
    /// have to change and what the rule is — never just "invalid".
    pub fn resolve(&self) -> Result<FoundryConfig, String> {
        let defaults = FoundryConfig::default();
        let mut detection = defaults.detection;

        if let Some(n) = self.min_occurrences {
            detection.min_occurrences = usize::try_from(n).map_err(|_| {
                format!("settings `foundry.min_occurrences`: {n} does not fit this platform")
            })?;
        }
        if let Some(n) = self.min_distinct_arguments {
            detection.min_distinct_arguments = usize::try_from(n).map_err(|_| {
                format!("settings `foundry.min_distinct_arguments`: {n} does not fit this platform")
            })?;
        }
        if let Some(ratio) = self.min_reuse_ratio {
            if !ratio.is_finite() || ratio < 0.0 {
                return Err(format!(
                    "settings `foundry.min_reuse_ratio`: {ratio} — must be a finite number at \
                     or above 0 (at or below 1.0 disables the reuse gate)"
                ));
            }
            detection.min_reuse_ratio = ratio;
        }
        if let Some(require_success) = self.require_success {
            detection.require_success = require_success;
        }
        if let Some(n) = self.max_examples {
            if n == 0 {
                return Err(
                    "settings `foundry.max_examples`: 0 — must be at least 1, because the \
                     authored tool's witness input is built from the recorded examples"
                        .to_string(),
                );
            }
            detection.max_examples = usize::try_from(n).map_err(|_| {
                format!("settings `foundry.max_examples`: {n} does not fit this platform")
            })?;
        }

        let mut breaker = defaults.breaker;
        if let Some(n) = self.breaker_consecutive_failures {
            breaker.consecutive_failures = validate_breaker_count(
                "foundry.breaker_consecutive_failures",
                n,
            )?;
        }
        if let Some(n) = self.breaker_window {
            breaker.window = validate_breaker_count("foundry.breaker_window", n)?;
        }
        if let Some(rate) = self.breaker_failure_rate {
            if !rate.is_finite() || rate <= 0.0 || rate > 1.0 {
                return Err(format!(
                    "settings `foundry.breaker_failure_rate`: {rate} — must be a finite share \
                     above 0 and at most 1 (the default 0.5 disables a tool once half of the \
                     last `breaker_window` invocations failed)"
                ));
            }
            breaker.failure_rate = rate;
        }

        let mut network_allowlist = Vec::new();
        if let Some(entries) = &self.network_allowlist {
            for entry in entries {
                if !is_valid_tool_name(entry) {
                    return Err(format!(
                        "settings `foundry.network_allowlist`: {entry:?} is not a valid tool \
                         name (^[a-z][a-z0-9_]{{1,63}}$), so it could never match a \
                         foundry-built tool"
                    ));
                }
                network_allowlist.push(entry.clone());
            }
        }

        Ok(FoundryConfig {
            detection,
            autonomy: self.autonomy.unwrap_or_default(),
            breaker,
            network_allowlist,
        })
    }
}

/// A breaker count must be at least 1 and fit in a `u32` — zero would trip
/// the breaker on a tool that has never run.
fn validate_breaker_count(key: &str, n: u64) -> Result<u32, String> {
    if n == 0 {
        return Err(format!(
            "settings `{key}`: 0 — must be at least 1, or the breaker would trip on a tool \
             that has never failed"
        ));
    }
    u32::try_from(n).map_err(|_| format!("settings `{key}`: {n} — must fit in 32 bits"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_block_resolves_to_the_shipped_defaults() {
        let resolved = FoundrySettings::default().resolve().expect("defaults");
        assert_eq!(resolved, FoundryConfig::default());
        let d = GapDetectionConfig::default();
        assert_eq!(resolved.detection.min_occurrences, d.min_occurrences);
        assert_eq!(resolved.detection.min_reuse_ratio, d.min_reuse_ratio);
        assert_eq!(resolved.autonomy, FoundryAutonomy::Auto);
    }

    #[test]
    fn min_occurrences_is_read_from_config() {
        let settings = FoundrySettings {
            min_occurrences: Some(4),
            ..Default::default()
        };
        assert_eq!(settings.resolve().unwrap().detection.min_occurrences, 4);
    }

    #[test]
    fn min_distinct_arguments_is_read_from_config() {
        let settings = FoundrySettings {
            min_distinct_arguments: Some(3),
            ..Default::default()
        };
        assert_eq!(
            settings.resolve().unwrap().detection.min_distinct_arguments,
            3
        );
    }

    #[test]
    fn min_reuse_ratio_is_read_and_a_bad_one_is_named() {
        let settings = FoundrySettings {
            min_reuse_ratio: Some(2.5),
            ..Default::default()
        };
        assert_eq!(settings.resolve().unwrap().detection.min_reuse_ratio, 2.5);

        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            let settings = FoundrySettings {
                min_reuse_ratio: Some(bad),
                ..Default::default()
            };
            let err = settings.resolve().unwrap_err();
            assert!(
                err.contains("foundry.min_reuse_ratio"),
                "diagnostic must name the key: {err}"
            );
        }
    }

    #[test]
    fn require_success_is_read_from_config() {
        let settings = FoundrySettings {
            require_success: Some(false),
            ..Default::default()
        };
        assert!(!settings.resolve().unwrap().detection.require_success);
    }

    #[test]
    fn max_examples_is_read_and_zero_is_rejected() {
        let settings = FoundrySettings {
            max_examples: Some(5),
            ..Default::default()
        };
        assert_eq!(settings.resolve().unwrap().detection.max_examples, 5);

        let zero = FoundrySettings {
            max_examples: Some(0),
            ..Default::default()
        };
        let err = zero.resolve().unwrap_err();
        assert!(err.contains("foundry.max_examples"), "{err}");
    }

    #[test]
    fn autonomy_parses_its_three_modes_and_rejects_typos() {
        for (raw, want) in [
            ("\"auto\"", FoundryAutonomy::Auto),
            ("\"draft-only\"", FoundryAutonomy::DraftOnly),
            ("\"off\"", FoundryAutonomy::Off),
        ] {
            let parsed: FoundryAutonomy = serde_json::from_str(raw).expect(raw);
            assert_eq!(parsed, want);
        }
        let err = serde_json::from_str::<FoundryAutonomy>("\"on\"").unwrap_err();
        assert!(err.to_string().contains("foundry.autonomy"), "{err}");
    }

    #[test]
    fn breaker_consecutive_failures_is_read_and_zero_is_rejected() {
        let settings = FoundrySettings {
            breaker_consecutive_failures: Some(5),
            ..Default::default()
        };
        assert_eq!(
            settings.resolve().unwrap().breaker.consecutive_failures,
            5
        );
        let zero = FoundrySettings {
            breaker_consecutive_failures: Some(0),
            ..Default::default()
        };
        let err = zero.resolve().unwrap_err();
        assert!(err.contains("foundry.breaker_consecutive_failures"), "{err}");
    }

    #[test]
    fn breaker_window_is_read_and_zero_is_rejected() {
        let settings = FoundrySettings {
            breaker_window: Some(20),
            ..Default::default()
        };
        assert_eq!(settings.resolve().unwrap().breaker.window, 20);
        let zero = FoundrySettings {
            breaker_window: Some(0),
            ..Default::default()
        };
        let err = zero.resolve().unwrap_err();
        assert!(err.contains("foundry.breaker_window"), "{err}");
    }

    #[test]
    fn breaker_failure_rate_is_read_and_out_of_range_is_named() {
        let settings = FoundrySettings {
            breaker_failure_rate: Some(0.8),
            ..Default::default()
        };
        assert_eq!(settings.resolve().unwrap().breaker.failure_rate, 0.8);
        for bad in [0.0, -0.5, 1.5, f64::NAN] {
            let settings = FoundrySettings {
                breaker_failure_rate: Some(bad),
                ..Default::default()
            };
            let err = settings.resolve().unwrap_err();
            assert!(err.contains("foundry.breaker_failure_rate"), "{err}");
        }
    }

    #[test]
    fn network_allowlist_is_read_and_a_non_name_is_rejected() {
        let settings = FoundrySettings {
            network_allowlist: Some(vec!["fetch_quotes".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            settings.resolve().unwrap().network_allowlist,
            vec!["fetch_quotes".to_string()]
        );
        let bad = FoundrySettings {
            network_allowlist: Some(vec!["Not A Tool".to_string()]),
            ..Default::default()
        };
        let err = bad.resolve().unwrap_err();
        assert!(err.contains("foundry.network_allowlist"), "{err}");
    }

    #[test]
    fn the_block_deserializes_from_json_and_toml_alike() {
        let json: FoundrySettings = serde_json::from_str(
            r#"{ "min_reuse_ratio": 2.5, "min_occurrences": 4, "autonomy": "draft-only" }"#,
        )
        .expect("json");
        let toml: FoundrySettings = toml::from_str(
            "min_reuse_ratio = 2.5\nmin_occurrences = 4\nautonomy = \"draft-only\"\n",
        )
        .expect("toml");
        assert_eq!(json, toml);
        assert_eq!(json.resolve().unwrap().detection.min_reuse_ratio, 2.5);
    }
}
