use std::path::Path;

use stella_protocol::tool::ToolOutput;

use super::super::exec;
use super::{ScriptIndex, compose_command};

/// Resolve and run one script — the single execution path shared by the
/// `run_script` tool and `stella scripts run`.
pub async fn run_by_name(
    root: &Path,
    script: &str,
    dir: Option<&str>,
    args: &[String],
    timeout_secs: u64,
    scratch_path: Option<&std::path::Path>,
) -> ToolOutput {
    let index = ScriptIndex::detect(root).await;
    let entry = match index.resolve(script, dir) {
        Ok(entry) => entry,
        Err(message) => return ToolOutput::Error { message },
    };
    let command = compose_command(entry, args);
    let cwd = if entry.dir == "." {
        root.to_path_buf()
    } else {
        root.join(&entry.dir)
    };
    exec::run_and_report(&command, &cwd, timeout_secs, scratch_path).await
}
