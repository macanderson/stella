use std::sync::Arc;

use super::Tool;

/// Built-ins that launch child processes or delegate to another executor.
/// Kept as one closed list so a process-free media registry can omit the
/// entire authority class rather than trying to protect one host-data path.
pub(super) fn builtins(
    task_board: crate::tasks::TaskBoardHandle,
    spawn_queue: crate::tasks::SpawnQueue,
) -> Vec<Arc<dyn Tool>> {
    let processes: crate::process::ProcessTableHandle = Arc::default();
    let repo: Arc<dyn crate::repo::RepoBackend> = Arc::new(crate::repo::GitCli);
    vec![
        // `bash` belongs to this list and to no switch. It is the same
        // authority class as `run_script` and the process group beside it —
        // an operator who wants it gone writes `"tools": {"bash": "off"}`,
        // which the policy layer above enforces for MCP and custom tools too.
        Arc::new(crate::bash::Bash),
        Arc::new(crate::verify::VerifyDone),
        Arc::new(crate::project::BuildProject),
        Arc::new(crate::project::RunTests),
        Arc::new(crate::diagnostics::Diagnostics),
        Arc::new(crate::project::RunLint),
        Arc::new(crate::project::FormatCode),
        Arc::new(crate::scripts::RunScript),
        Arc::new(crate::process::StartProcess(processes.clone())),
        Arc::new(crate::process::ReadOutput(processes.clone())),
        Arc::new(crate::process::SendStdin(processes.clone())),
        Arc::new(crate::process::StopProcess(processes)),
        Arc::new(crate::repo::RepoStatusTool(repo.clone())),
        Arc::new(crate::repo::RepoDiffTool(repo.clone())),
        Arc::new(crate::repo::RepoCommit(repo.clone())),
        Arc::new(crate::repo::RepoPush(repo.clone())),
        Arc::new(crate::repo::RepoPull(repo.clone())),
        Arc::new(crate::repo::RepoRollback(repo)),
        Arc::new(crate::ci::CiStatus),
        Arc::new(crate::screenshot::Screenshot),
        Arc::new(crate::tasks::TaskAssign(task_board, spawn_queue)),
    ]
}
