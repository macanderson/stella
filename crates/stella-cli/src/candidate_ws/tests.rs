use super::*;
use stella_protocol::FileChangeKind;

/// #2067: the seal identity is the vocabulary `verify_done`'s baseline
/// walk skips — the two crates must spell it identically, and a `const`
/// array cannot embed the other crate's constant, so this test is the
/// binding.
#[test]
fn snapshot_identity_matches_the_verify_done_walk_vocabulary() {
    assert_eq!(
        SNAPSHOT_IDENT[3],
        format!(
            "user.email={}",
            stella_tools::verify::CANDIDATE_SNAPSHOT_EMAIL
        )
    );
}

#[test]
fn candidate_port_keeps_the_exact_host_operation_journal() {
    let journal: Arc<dyn stella_media::MediaOperationJournal> = Arc::new(
        stella_media::SqliteMediaOperationJournal::open_in_memory(Default::default()).unwrap(),
    );
    let options = RegistryOptions {
        media_operation_journal: Some(journal.clone()),
        ..Default::default()
    };

    let port = GitCandidateWorkspaces::new(
        PathBuf::from("unused"),
        options,
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );

    assert!(Arc::ptr_eq(
        &journal,
        port.options.media_operation_journal.as_ref().unwrap()
    ));
}

/// Run `git <args>` in `root` with hook-exported `GIT_*` vars scrubbed
/// (the verify_done test discipline — without it, running the suite from
/// inside a git hook re-targets every command at the HOST repo) and
/// return stdout. Panics on failure: these are test fixtures.
fn scratch_git(root: &Path, args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("git");
    stella_tools::exec::scrub_sensitive_std_env(&mut cmd);
    cmd.args(args).current_dir(root);
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Build the canonical dirty repo every test starts from:
/// - committed: `tracked.txt` ("base\n"), `.gitignore` (ignores
///   `ignored.txt`)
/// - a pre-existing stash entry (fixture only — the production code must
///   never touch it)
/// - uncommitted tracked edit: `tracked.txt` = "base\ndirty\n"
/// - staged-but-uncommitted new file: `staged.txt`
/// - untracked: `untracked.txt`; ignored: `ignored.txt`
pub(super) fn scaffold(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stella_cwstest_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t.t"],
        &["config", "user.name", "t"],
        &["config", "commit.gpgsign", "false"],
    ] {
        scratch_git(&root, args);
    }
    std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
    std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    scratch_git(&root, &["add", "."]);
    scratch_git(&root, &["commit", "-q", "-m", "base"]);
    // The shared-stash fixture: other sessions' stashes must survive
    // best-of-N byte-identically.
    std::fs::write(root.join("tracked.txt"), "stash-fixture\n").unwrap();
    scratch_git(&root, &["stash", "push", "-q", "-m", "fixture"]);
    // The dirty state candidates must see.
    std::fs::write(root.join("tracked.txt"), "base\ndirty\n").unwrap();
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    scratch_git(&root, &["add", "staged.txt"]);
    std::fs::write(root.join("untracked.txt"), "hello\n").unwrap();
    std::fs::write(root.join("ignored.txt"), "secret\n").unwrap();
    root
}

/// The observable state of the real repo that candidate isolation must
/// leave byte-identical: worktree status, staged diff, stash list, HEAD.
fn tree_state(root: &Path) -> (String, String, String, String) {
    (
        scratch_git(root, &["status", "--porcelain"]),
        scratch_git(root, &["diff", "--cached"]),
        scratch_git(root, &["stash", "list"]),
        scratch_git(root, &["rev-parse", "HEAD"]),
    )
}

fn assert_no_candidate_worktrees(root: &Path) {
    let listing = scratch_git(root, &["worktree", "list", "--porcelain"]);
    assert!(
        !listing.contains("stella_candidate_"),
        "candidate worktrees must not stay registered: {listing}"
    );
}

pub(super) fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn snapshot_mirrors_dirty_staged_and_untracked_state_without_touching_the_real_tree() {
    let root = scaffold("snap");
    let before = tree_state(&root);
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );

    let ws = port.create_workspace().await.unwrap();
    // Uncommitted tracked edit, staged-but-uncommitted new file, and the
    // untracked file are all visible; the ignored file is not.
    assert_eq!(read(&ws.dir().join("tracked.txt")), "base\ndirty\n");
    assert_eq!(read(&ws.dir().join("staged.txt")), "staged\n");
    assert_eq!(read(&ws.dir().join("untracked.txt")), "hello\n");
    assert!(
        !ws.dir().join("ignored.txt").exists(),
        "ignored files are not part of the snapshot"
    );

    // Fingerprint parity: the snapshot reports the overlay untracked file
    // with the REAL tree's complete content hash, so the
    // witness tamper watchlist keeps working inside candidates.
    let real = GitRepoStatus { root: root.clone() }
        .untracked_fingerprints()
        .await;
    let snap = ws.repo_status().untracked_fingerprints().await;
    assert_eq!(
        snap.get("untracked.txt"),
        real.get("untracked.txt"),
        "overlay fingerprints must match the real tree's"
    );

    std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
    std::fs::write(
        ws.dir().join("tests/witness.rs"),
        "#[test] fn witness() {}\n",
    )
    .unwrap();
    let witness_fingerprint = ws
        .repo_status()
        .untracked_fingerprints()
        .await
        .remove("tests/witness.rs")
        .expect("new witness is visible in the candidate delta");
    let identity = ws
        .repo_status()
        .artifact_identity("tests/witness.rs")
        .await
        .expect("candidate status exposes the no-follow artifact identity");
    assert!(identity.is_regular_single_link());
    assert_eq!(identity.fingerprint, witness_fingerprint);

    // The real tree, index, stash, and HEAD are untouched by creation.
    assert_eq!(tree_state(&root), before);

    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    assert!(!ws.dir().exists(), "the shadow directory is removed");
    assert_eq!(
        tree_state(&root),
        before,
        "removal is also side-effect free"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn authored_witness_keeps_identity_through_seal_verification_and_exact_adoption() {
    let root = scaffold("witness-seal");
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();
    std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
    std::fs::write(
        ws.dir().join("tests/authority_witness.rs"),
        "#[test] fn authority_witness() {}\n",
    )
    .unwrap();

    let authored = ws
        .repo_status()
        .artifact_identity("tests/authority_witness.rs")
        .await
        .expect("authored witness has an identity");
    ws.seal().await.unwrap();
    assert!(
        !ws.repo_status()
            .untracked_fingerprints()
            .await
            .contains_key("tests/authority_witness.rs"),
        "the seal intentionally reclassifies the witness as tracked"
    );
    let verified = ws
        .repo_status()
        .artifact_identity("tests/authority_witness.rs")
        .await;
    assert!(stella_pipeline::witness_identity_matches(
        &authored,
        verified.as_ref()
    ));

    let adopted = ws.adopt(&[]).await.unwrap();
    assert_eq!(adopted.len(), 1);
    assert_eq!(
        read(&root.join("tests/authority_witness.rs")),
        "#[test] fn authority_witness() {}\n"
    );
    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

/// Withholding the witness must remove ONLY the witness. The whole risk of
/// filtering the adoption patch is that an over-broad pathspec silently
/// swallows real work, which would look like the model simply not doing
/// the task — so this asserts both halves: the witness is absent from the
/// real tree AND from the reported change list, while the production edit
/// beside it lands byte-exactly.
#[tokio::test]
async fn withholding_the_witness_still_adopts_the_work_it_verified() {
    let root = scaffold("witness-withhold");
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();
    std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
    std::fs::write(
        ws.dir().join("tests/authority_witness.rs"),
        "#[test] fn authority_witness() {}\n",
    )
    .unwrap();
    // The real work the witness exists to prove.
    std::fs::write(ws.dir().join("tracked.txt"), "the actual fix\n").unwrap();
    ws.seal().await.unwrap();

    let withhold = vec!["tests/authority_witness.rs".to_string()];
    let adopted = ws.adopt(&withhold).await.unwrap();

    assert_eq!(
        adopted.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec!["tracked.txt"],
        "the withheld witness must not be reported as adopted"
    );
    assert_eq!(read(&root.join("tracked.txt")), "the actual fix\n");
    assert!(
        !root.join("tests/authority_witness.rs").exists(),
        "the witness must never reach the real tree"
    );
    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

/// A witness whose filename contains glob metacharacters must exclude
/// exactly itself. `:(exclude)` without `literal` would treat `[` and `*`
/// as a pattern and could withhold production files that happen to match.
#[tokio::test]
async fn withholding_treats_the_path_literally_not_as_a_glob() {
    let root = scaffold("witness-glob");
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();
    std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
    // `t*.rs` as a glob would also match `tracked_rs.rs`; as a literal it
    // matches only the file actually named `t*.rs`.
    std::fs::write(ws.dir().join("tests/t*.rs"), "#[test] fn w() {}\n").unwrap();
    std::fs::write(ws.dir().join("tests/tracked_rs.rs"), "// real work\n").unwrap();
    ws.seal().await.unwrap();

    let adopted = ws.adopt(&["tests/t*.rs".to_string()]).await.unwrap();

    assert_eq!(
        adopted.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec!["tests/tracked_rs.rs"],
        "only the literally-named witness may be withheld"
    );
    assert!(root.join("tests/tracked_rs.rs").exists());
    assert!(!root.join("tests/t*.rs").exists());
    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

/// A candidate's tool surface includes the session's custom script tools,
/// and running one executes in the SNAPSHOT (cwd = shadow), never the real
/// tree — the isolation guarantee for the grown tool surface.
#[tokio::test]
async fn custom_tools_reach_the_candidate_and_run_in_the_snapshot() {
    let root = scaffold("customtools");
    // A custom tool that writes a file into its cwd. Discovered from the
    // real root exactly as the session discovers it.
    std::fs::create_dir_all(root.join(".stella/tools")).unwrap();
    std::fs::write(
        root.join(".stella/tools/writer.toml"),
        "name = \"writer\"\n\
         description = \"write a marker file into the cwd\"\n\
         command = [\"sh\", \"-c\", \"printf candidate > candidate_wrote.txt\"]\n\
         [input_schema]\n\
         type = \"object\"\n",
    )
    .unwrap();
    let found = stella_tools::custom::discover(&root);
    let custom_tools = crate::tool_foundry::adopt::gate_discovery(found, &root).tools;
    assert_eq!(custom_tools.len(), 1, "the writer tool must be discovered");

    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        custom_tools,
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();

    // The candidate model sees the custom tool in its schema…
    let names: Vec<String> = ws.tools().schemas().into_iter().map(|s| s.name).collect();
    assert!(
        names.iter().any(|n| n == "writer"),
        "candidate schemas must include the custom tool: {names:?}"
    );
    // …and it also still sees a built-in (the registry is the inner set).
    assert!(
        names.iter().any(|n| n == "read_file"),
        "candidate must still have the built-in registry"
    );

    // Executing it writes into the SNAPSHOT, not the real tree.
    let out = ws.tools().execute("writer", &serde_json::json!({})).await;
    assert!(!out.is_error(), "custom tool run failed: {out:?}");
    assert_eq!(
        read(&ws.dir().join("candidate_wrote.txt")),
        "candidate",
        "the custom tool must write inside the snapshot"
    );
    assert!(
        !root.join("candidate_wrote.txt").exists(),
        "the custom tool must NOT touch the real tree"
    );

    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

/// A minimal [`stella_mcp::Transport`] fake for the wiring test below —
/// no process, no socket; just enough of the MCP handshake plus one tool
/// to prove a schema makes it through `GitCandidateWorkspaces::create`
/// end to end, not just the isolated `CandidateMcpView` unit tests in
/// `stella-mcp`.
struct FakeMcpTransport;

#[async_trait]
impl stella_mcp::Transport for FakeMcpTransport {
    async fn request(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, stella_mcp::McpError> {
        Ok(match method {
            "initialize" => serde_json::json!({
                "protocolVersion": stella_mcp::protocol::PREFERRED_PROTOCOL_VERSION
            }),
            "tools/list" => serde_json::json!({
                "tools": [{ "name": "search", "inputSchema": { "type": "object" } }]
            }),
            "tools/call" => {
                serde_json::json!({ "content": [{ "type": "text", "text": "docs ok" }] })
            }
            _ => serde_json::Value::Null,
        })
    }
    async fn notify(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<(), stella_mcp::McpError> {
        Ok(())
    }
    async fn close(&self) -> Result<(), stella_mcp::McpError> {
        Ok(())
    }
}

/// Issue #248 Phase 1's wiring witness: a session that shares its MCP
/// toolset via `.with_candidate_mcp` must have the allowlisted server's
/// tools actually reach a REAL candidate workspace's advertised schema —
/// closing the gap `stella-mcp`'s `CandidateMcpView` tests can't (they
/// exercise the view in isolation, never through this port).
#[tokio::test]
async fn candidate_mcp_reaches_the_candidate_when_the_session_shares_it() {
    let root = scaffold("mcpwiring");
    let mut docs_client = stella_mcp::McpClient::new("docs", Box::new(FakeMcpTransport));
    docs_client.initialize().await.unwrap();
    let mcp = Arc::new(
        stella_mcp::McpToolSet::from_clients(vec![docs_client])
            .with_candidate_safe_servers(["docs"]),
    );
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    )
    .with_candidate_mcp(mcp);
    let ws = port.create_workspace().await.unwrap();

    let names: Vec<String> = ws.tools().schemas().into_iter().map(|s| s.name).collect();
    assert!(
        names.iter().any(|n| n == "mcp__docs__search"),
        "the allowlisted server's tool must reach the candidate: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "read_file"),
        "the candidate must still have its snapshot-rooted built-ins: {names:?}"
    );

    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn store_backed_guard_survives_candidate_snapshot_and_winner_adoption() {
    let root = scaffold("storeguard");
    std::fs::write(root.join(".gitignore"), "ignored.txt\n.stella/private/\n").unwrap();
    {
        let store = stella_store::Store::open(&root).unwrap();
        store
            .upsert_rule(
                "protect-store-path",
                "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nStore guard remains binding in candidates.",
                "ext:policy",
            )
            .unwrap();
    }
    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..crate::settings::AuthorityPolicy::default()
    };
    let active_rules = crate::rules::load_workspace_rules(&root, &authority);
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        active_rules,
    );
    let ws = port.create_workspace().await.unwrap();

    let output = ws
        .tools()
        .execute(
            "write_file",
            &serde_json::json!({"path": "protected/store.txt", "content": "no\n"}),
        )
        .await;
    ws.seal().await.unwrap();
    let adopted = ws.adopt(&[]).await.unwrap();
    let landed = root.join("protected/store.txt").exists();
    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();

    assert!(
        output.is_error(),
        "candidate bypassed store guard: {output:?}"
    );
    assert!(
        adopted.is_empty(),
        "prohibited change was adoptable: {adopted:?}"
    );
    assert!(!landed, "winner adopted a store-guarded change");
}

#[tokio::test]
async fn ignored_rule_guard_survives_candidate_snapshot_and_winner_adoption() {
    let root = scaffold("ignoredguard");
    std::fs::write(
        root.join(".gitignore"),
        "ignored.txt\n.stella/rules/protect-ignored.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".stella/rules")).unwrap();
    std::fs::write(
        root.join(".stella/rules/protect-ignored.md"),
        "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nIgnored file guard remains binding in candidates.",
    )
    .unwrap();
    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..crate::settings::AuthorityPolicy::default()
    };
    let active_rules = crate::rules::load_workspace_rules(&root, &authority);
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        active_rules,
    );
    let ws = port.create_workspace().await.unwrap();

    let output = ws
        .tools()
        .execute(
            "write_file",
            &serde_json::json!({"path": "protected/ignored.txt", "content": "no\n"}),
        )
        .await;
    ws.seal().await.unwrap();
    let adopted = ws.adopt(&[]).await.unwrap();
    let landed = root.join("protected/ignored.txt").exists();
    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();

    assert!(
        output.is_error(),
        "candidate bypassed ignored guard: {output:?}"
    );
    assert!(
        adopted.is_empty(),
        "prohibited change was adoptable: {adopted:?}"
    );
    assert!(!landed, "winner adopted an ignored-rule-guarded change");
}

#[tokio::test]
async fn winner_adoption_lands_only_the_winners_changes() {
    let root = scaffold("adopt");
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let loser = port.create_workspace().await.unwrap();
    let winner = port.create_workspace().await.unwrap();

    // The loser edits a tracked file and creates a new one.
    std::fs::write(loser.dir().join("tracked.txt"), "base\ndirty\nloser\n").unwrap();
    std::fs::write(loser.dir().join("loser.txt"), "residue\n").unwrap();
    // The winner edits the tracked file, creates a file, and deletes the
    // pre-existing untracked file.
    std::fs::write(winner.dir().join("tracked.txt"), "base\ndirty\nwinner\n").unwrap();
    std::fs::write(winner.dir().join("winner.txt"), "new\n").unwrap();
    std::fs::remove_file(winner.dir().join("untracked.txt")).unwrap();
    winner.seal().await.unwrap();

    let (_, before_cached, before_stash, before_head) = tree_state(&root);
    loser.remove().await;

    let mut adopted = winner.adopt(&[]).await.unwrap();
    adopted.sort_by(|a, b| a.path.cmp(&b.path));
    let described: Vec<(String, FileChangeKind)> =
        adopted.into_iter().map(|c| (c.path, c.kind)).collect();
    assert_eq!(
        described,
        vec![
            ("tracked.txt".to_string(), FileChangeKind::Modified),
            ("untracked.txt".to_string(), FileChangeKind::Deleted),
            ("winner.txt".to_string(), FileChangeKind::Created),
        ]
    );

    // Winner's changes landed; loser's never touched the real tree.
    assert_eq!(read(&root.join("tracked.txt")), "base\ndirty\nwinner\n");
    assert_eq!(read(&root.join("winner.txt")), "new\n");
    assert!(!root.join("untracked.txt").exists());
    assert!(!root.join("loser.txt").exists());

    // Index, stash, and HEAD are byte-identical: adoption writes only
    // the working tree.
    let (_, after_cached, after_stash, after_head) = tree_state(&root);
    assert_eq!(after_cached, before_cached, "the index is never touched");
    assert_eq!(after_stash, before_stash, "the stash is never touched");
    assert_eq!(after_head, before_head, "HEAD never moves");

    winner.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn post_verification_worktree_drift_is_rejected_and_never_adopted() {
    let root = scaffold("sealed-drift");
    let before = tree_state(&root);
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();
    std::fs::write(ws.dir().join("verified.txt"), "verified bytes\n").unwrap();

    ws.seal()
        .await
        .expect("candidate state seals before verification");
    assert!(ws.sealed_is_unchanged().await.unwrap());
    std::fs::write(
        ws.dir().join("verified.txt"),
        "mutated after verification\n",
    )
    .unwrap();

    let error = ws.adopt(&[]).await.expect_err("drift must reject adoption");
    assert!(
        error.to_string().contains("changed after verification"),
        "{error}"
    );
    assert!(!root.join("verified.txt").exists());
    assert_eq!(
        tree_state(&root),
        before,
        "real tree remains byte-identical"
    );

    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_mid_run_user_edit_fails_adoption_atomically_naming_the_path() {
    let root = scaffold("conflict");
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    let ws = port.create_workspace().await.unwrap();

    std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
    std::fs::write(ws.dir().join("second.txt"), "must not land\n").unwrap();
    ws.seal().await.unwrap();
    // The user edits the same file while the candidate runs.
    std::fs::write(root.join("tracked.txt"), "base\nuser-edit\n").unwrap();

    match ws.adopt(&[]).await.unwrap_err() {
        WorkspaceError::Adopt {
            paths, workspace, ..
        } => {
            assert!(
                paths.iter().any(|p| p.contains("tracked.txt")),
                "the conflict must name the path: {paths:?}"
            );
            assert_eq!(workspace, ws.dir().display().to_string());
        }
        other => panic!("expected an adoption conflict, got {other:?}"),
    }
    // Atomicity: NOTHING was applied — not even the conflict-free file.
    assert_eq!(read(&root.join("tracked.txt")), "base\nuser-edit\n");
    assert!(
        !root.join("second.txt").exists(),
        "a rejected adoption must not half-apply"
    );
    // The workspace is preserved for recovery until removed explicitly.
    assert!(ws.dir().exists());

    ws.remove().await;
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_repo_without_commits_is_a_clean_snapshot_error() {
    let root = std::env::temp_dir().join(format!(
        "stella_cwstest_nocommit_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    scratch_git(&root, &["init", "-q"]);
    let port = GitCandidateWorkspaces::new(
        root.clone(),
        RegistryOptions::default(),
        Default::default(),
        Vec::new(),
        crate::rules::ResolvedRules::default(),
    );
    match port.create_workspace().await {
        Err(WorkspaceError::Snapshot { reason }) => {
            assert!(reason.contains("at least one commit"), "{reason}")
        }
        Err(other) => panic!("expected a snapshot error, got {other:?}"),
        Ok(_) => panic!("expected a snapshot error, got a workspace"),
    }
    assert_no_candidate_worktrees(&root);
    std::fs::remove_dir_all(&root).ok();
}
