use std::sync::Arc;

use super::Tool;

/// Built-ins that launch child processes or delegate to another executor.
/// Kept as one closed list so a process-free media registry can omit the
/// entire authority class rather than trying to protect one host-data path.
pub(super) fn builtins(
    task_board: crate::tasks::TaskBoardHandle,
    spawn_queue: crate::tasks::SpawnQueue,
    shadow_memo: crate::verify::ShadowMemoHandle,
    diagnostics: Option<Arc<stella_diag::Dx>>,
) -> Vec<Arc<dyn Tool>> {
    let processes: crate::process::ProcessTableHandle = Arc::default();
    let repo: Arc<dyn crate::repo::RepoBackend> = Arc::new(crate::repo::GitCli);
    // Same adapter, separate port: history is read-only and survives the
    // `ReadOnlyTools` filter that withholds `bash`, which is the whole
    // reason these two exist (see `repo::history`'s module doc).
    let history: Arc<dyn crate::repo::history::HistoryBackend> = Arc::new(crate::repo::GitCli);
    let scratch = crate::scratch::ScratchDir::new().map(Arc::new);
    let scratch_path = scratch.as_ref().ok().map(|s| s.path().to_path_buf());

    let mut tools = vec![
        // `bash` belongs to this list and to no switch. It is the same
        // authority class as `run_script` and the process group beside it —
        // an operator who wants it gone writes `"tools": {"bash": "off"}`,
        // which the policy layer above enforces for MCP and custom tools too.
        Arc::new(crate::bash::Bash::new(scratch_path.clone())) as Arc<dyn Tool>,
        Arc::new(crate::verify::VerifyDone::new(shadow_memo, diagnostics)),
        Arc::new(crate::project::BuildProject),
        Arc::new(crate::project::RunTests),
        Arc::new(crate::diagnostics::Diagnostics),
        Arc::new(crate::project::RunLint),
        Arc::new(crate::project::FormatCode),
        Arc::new(crate::scripts::RunScript {
            scratch: scratch_path.clone(),
        }),
        Arc::new(crate::process::StartProcess(processes.clone())),
        Arc::new(crate::process::ReadOutput(processes.clone())),
        Arc::new(crate::process::ClearOutput(processes.clone())),
        Arc::new(crate::process::SendStdin(processes.clone())),
        Arc::new(crate::process::StopProcess(processes)),
        Arc::new(crate::repo::RepoStatusTool(repo.clone())),
        Arc::new(crate::repo::RepoDiffTool(repo.clone())),
        Arc::new(crate::repo::history::RepoHistoryTool(history.clone())),
        Arc::new(crate::repo::history::RepoRecoverTool(history)),
        Arc::new(crate::repo::RepoCommit(repo.clone())),
        Arc::new(crate::repo::RepoPush(repo.clone())),
        Arc::new(crate::repo::RepoPull(repo.clone())),
        Arc::new(crate::repo::RepoRollback(repo)),
        Arc::new(crate::ci::CiStatus::default()),
        Arc::new(crate::screenshot::Screenshot),
        Arc::new(crate::tasks::TaskAssign(task_board, spawn_queue)),
    ];

    if let Ok(scratch) = scratch {
        tools.extend([
            Arc::new(crate::scratch::SaveState(scratch.clone())) as Arc<dyn Tool>,
            Arc::new(crate::scratch::GetState(scratch.clone())),
            Arc::new(crate::scratch::ListState(scratch.clone())),
            Arc::new(crate::scratch::DeleteState(scratch)),
        ]);
    } else {
        eprintln!("warning: session scratch directory failed to initialize");
    }

    tools
}
