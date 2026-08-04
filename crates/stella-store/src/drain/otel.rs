//! The OTLP (`format = "otel"`) drain encoding (#427): the same staged hub
//! rows the native payload ships, emitted as an OTLP/HTTP JSON `/v1/logs`
//! request — one log record per row, identity + telemetry as attributes — so
//! telemetry can flow into an existing OpenTelemetry collector instead of a
//! bespoke Stella intake.
//!
//! Only the encoding differs from `format = "stella"`: the pager → POST →
//! ack loop, retry classification, and cursor semantics are shared, because
//! the syncer encodes whichever format the drain config names and delivers
//! through the same [`super::CloudIntake`] port.
//!
//! # Content-free by construction
//!
//! The attribute set mirrors the native wire contract *exactly* — the
//! [`super::DrainRow`] field set and nothing else — and the encoder is
//! registered with the content-free harness
//! ([`crate::content_free::registered_encoders`]), so a content-bearing field
//! reaching an attribute fails the build, not a privacy review after the
//! fact.
//!
//! # At-least-once, and how a collector can dedup
//!
//! The Stella intake dedups on `(workspace_id, source_rowid)`; a generic OTel
//! backend does not, so a re-send after a lost ack lands as a duplicate log
//! record. Both halves of the dedup key ride as attributes
//! (`stella.workspace_id`, `stella.source_rowid`) so a collector *can* dedup
//! — and absent that, this format is at-least-once by contract.
//!
//! # Timestamps
//!
//! `timeUnixNano` is deliberately `"0"`: the hub records `recorded_at` as an
//! RFC 3339 string and this crate takes no date-parsing dependency to convert
//! it. The authoritative event time rides as the `stella.recorded_at`
//! attribute (exactly as it does on the native wire); collectors that need
//! the OTLP timestamp can derive it there.

use serde_json::{Value, json};

use super::{DrainBatch, OTEL_SCHEMA_VERSION_ATTR};

/// The OTLP/HTTP logs path the `otel` endpoint is expected to serve. The
/// drain config carries a full URL; this constant documents the convention.
pub const OTLP_LOGS_PATH: &str = "/v1/logs";

/// Every log record's body — a constant marker, never row content.
const LOG_BODY: &str = "stella.telemetry";

fn string_attr(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

/// proto3 JSON maps int64 onto a *string*, so `intValue` is stringified.
fn int_attr(key: &str, value: i64) -> Value {
    json!({ "key": key, "value": { "intValue": value.to_string() } })
}

fn double_attr(key: &str, value: f64) -> Value {
    json!({ "key": key, "value": { "doubleValue": value } })
}

fn bool_attr(key: &str, value: bool) -> Value {
    json!({ "key": key, "value": { "boolValue": value } })
}

/// Encode one native batch as an OTLP/HTTP JSON `/v1/logs` request body.
///
/// The batch's [`DrainBatch::schema_version`] rides as the scope attribute
/// [`OTEL_SCHEMA_VERSION_ATTR`], so a collector reads the client version the
/// same way regardless of format.
pub fn otlp_logs_payload(batch: &DrainBatch) -> Value {
    let records: Vec<Value> = batch
        .rows
        .iter()
        .map(|row| {
            let mut attributes = vec![
                string_attr("stella.org_id", &row.org_id),
                string_attr("stella.repo_id", &row.repo_id),
                string_attr("stella.project_id", &row.project_id),
                int_attr("stella.source_rowid", row.source_rowid),
                string_attr("stella.recorded_at", &row.recorded_at),
                string_attr("stella.provider", &row.provider),
                string_attr("stella.model", &row.model),
                int_attr("stella.input_tokens", row.input_tokens as i64),
                int_attr("stella.output_tokens", row.output_tokens as i64),
                int_attr("stella.cache_read_tokens", row.cache_read_tokens as i64),
                int_attr("stella.cache_miss_tokens", row.cache_miss_tokens as i64),
                int_attr("stella.cache_write_tokens", row.cache_write_tokens as i64),
                double_attr("stella.cost_usd", row.cost_usd),
                int_attr("stella.duration_ms", row.duration_ms as i64),
                int_attr("stella.retries", i64::from(row.retries)),
                int_attr("stella.tool_calls", row.tool_calls as i64),
                bool_attr("stella.usage_complete", row.usage_complete),
            ];
            if let Some(workspace_id) = &row.workspace_id {
                // One half of the (workspace_id, source_rowid) dedup key; a
                // row replicated before registration has none to carry.
                attributes.insert(1, string_attr("stella.workspace_id", workspace_id));
            }
            json!({
                "timeUnixNano": "0",
                "body": { "stringValue": LOG_BODY },
                "attributes": attributes,
            })
        })
        .collect();
    json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [string_attr("service.name", "stella")],
            },
            "scopeLogs": [{
                "scope": {
                    "name": "stella-drain",
                    "version": batch.schema_version.to_string(),
                    "attributes": [int_attr(OTEL_SCHEMA_VERSION_ATTR, i64::from(batch.schema_version))],
                },
                "logRecords": records,
            }],
        }],
    })
}

/// Every OTLP attribute name the encoder emits — collected by the content-free
/// guard into the key gate, so adding an attribute is a reviewed allowlist
/// edit exactly like a native wire key.
pub fn attribute_names(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_attribute_names(payload, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_attribute_names(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let (Some(Value::String(key)), true) = (map.get("key"), map.contains_key("value")) {
                out.push(key.clone());
            }
            for nested in map.values() {
                collect_attribute_names(nested, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_attribute_names(nested, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::DrainBatch;
    use super::*;
    use crate::content_free::poisoned_cloud_event;

    fn payload() -> Value {
        otlp_logs_payload(&DrainBatch::from_events(&[poisoned_cloud_event()]))
    }

    #[test]
    fn the_payload_is_one_log_record_per_row_with_the_dedup_key() {
        let payload = payload();
        let records = &payload["resourceLogs"][0]["scopeLogs"][0]["logRecords"];
        assert_eq!(records.as_array().map(Vec::len), Some(1));
        let names = attribute_names(&payload);
        assert!(names.iter().any(|n| n == "stella.workspace_id"));
        assert!(names.iter().any(|n| n == "stella.source_rowid"));
        assert!(names.iter().any(|n| n == "stella.recorded_at"));
    }

    #[test]
    fn the_schema_version_rides_the_scope_attribute() {
        let payload = payload();
        let scope = &payload["resourceLogs"][0]["scopeLogs"][0]["scope"];
        assert_eq!(
            scope["attributes"][0]["key"],
            super::super::OTEL_SCHEMA_VERSION_ATTR
        );
        assert_eq!(
            scope["attributes"][0]["value"]["intValue"],
            super::super::DRAIN_SCHEMA_VERSION.to_string()
        );
    }

    #[test]
    fn a_row_without_a_workspace_id_omits_the_attribute() {
        let mut event = poisoned_cloud_event();
        event.workspace_id = None;
        let payload = otlp_logs_payload(&DrainBatch::from_events(&[event]));
        assert!(
            !attribute_names(&payload)
                .iter()
                .any(|n| n == "stella.workspace_id")
        );
    }
}
