//! `run_script`'s `command.started` gate resolution, and the input-threading
//! seam that closes the gate/execute TOCTOU (#804): the line the fence
//! approves is the line that runs.

use std::path::Path;

use serde_json::Value;

use super::{ScriptIndex, compose_command, string_args};

/// Internal input key carrying `run_script`'s gate resolution into
/// execution. Authored ONLY by the registry — any model-authored value is
/// stripped before injection, like `exploration::LEDGER_FILES_KEY`. Once
/// the `command.started` chain approves the composed line, execute runs
/// exactly that line in exactly that dir instead of re-resolving the
/// index, closing the gate/execute TOCTOU where a Taskfile change between
/// the two resolutions swapped the approved command for another.
pub(crate) const GATE_RESOLVED_KEY: &str = "_gate_resolved_command";

/// `run_script`'s gate resolution: the composed command line and the
/// package dir (index-relative, `.` for the root) it runs in.
pub(crate) struct GateResolved {
    pub command: String,
    pub dir: String,
}

/// Resolve `run_script`'s command line for the registry's `command.started`
/// policy chain — best-effort: `None` (no gating) when the input or index
/// can't resolve, in which case the tool itself returns the named error.
pub(crate) async fn resolve_command_for_gate(root: &Path, input: &Value) -> Option<GateResolved> {
    let script = input.get("script")?.as_str()?;
    let dir = input.get("dir").and_then(|v| v.as_str());
    let args = string_args(input);
    let index = ScriptIndex::detect(root).await;
    let entry = index.resolve(script, dir).ok()?;
    Some(GateResolved {
        command: compose_command(entry, &args),
        dir: entry.dir.clone(),
    })
}

/// The registry's `run_script` seam: strip any model-authored
/// [`GATE_RESOLVED_KEY`] from `input` and — when a bus is attached
/// (`resolve`) — resolve once and inject `{command, dir}` under the key.
/// Returns the augmented input (`None` when nothing changed) and the
/// resolved line for the `command.started` chain.
pub(crate) async fn thread_gate_resolution(
    root: &Path,
    input: &Value,
    resolve: bool,
) -> (Option<Value>, Option<String>) {
    let Value::Object(map) = input else {
        return (None, None);
    };
    let mut map = map.clone();
    let authored = map.remove(GATE_RESOLVED_KEY).is_some();
    let resolved = if resolve {
        resolve_command_for_gate(root, input).await
    } else {
        None
    };
    if let Some(resolved) = &resolved {
        map.insert(
            GATE_RESOLVED_KEY.to_string(),
            serde_json::json!({ "command": resolved.command, "dir": resolved.dir }),
        );
    }
    let augmented = (authored || resolved.is_some()).then_some(Value::Object(map));
    (augmented, resolved.map(|r| r.command))
}
