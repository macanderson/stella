//! Registry unit tests: construction and gating of the tool set, the
//! read-only/speculation-safe schema claims, and per-tool dispatch.
//!
//! Split out of `registry.rs` so the module that ships the tools is not
//! dominated by the module that checks them. The submodules below cover one
//! seam each and share this module's fixtures through `use super::*`.

use super::*;

use crate::issues::IssueBackend;

/// A registry rooted in a fresh empty tempdir. Rooting tests at a shared
/// path like `/tmp` is not hermetic: a stray `.stella/private/codegraph.db` left
/// there by a real session conditionally registers `graph_query` and
/// skews every tool-set assertion. The `TempDir` is returned so the root
/// outlives the registry.
fn bare_registry(issue_backend: Option<IssueBackend>) -> (tempfile::TempDir, ToolRegistry) {
    let root = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(root.path().to_path_buf(), issue_backend);
    (root, reg)
}

/// `names` with `web_search` folded in when the *host running the tests*
/// happens to export a BYOK search key. `web_search` is the one remaining
/// environment-dependent row in the default surface — the other three web
/// tools need no key — so an exact-set pin has to account for it rather
/// than fail on a developer's machine.
fn with_ambient_search(mut names: Vec<&'static str>) -> Vec<&'static str> {
    if crate::web::detect_search_backend().is_some() {
        names.push("web_search");
        names.sort_unstable();
    }
    names
}

/// A coverage hint must fire only on whole path tokens — a substring hit
/// (`lib.rs` inside `mylib.rs`) would permanently burn the map's
/// once-per-session hint on a file it doesn't cover.
#[test]
fn mentions_path_requires_token_boundaries() {
    assert!(mentions_path("src/lib.rs:12: pub fn x()", "src/lib.rs"));
    assert!(mentions_path("lib.rs", "lib.rs"));
    // An absolute spelling of a covered relative path still matches.
    assert!(mentions_path("/tmp/ws/src/lib.rs:3: fn y()", "src/lib.rs"));
    assert!(mentions_path(
        "see `core/driver.rs` for the loop",
        "core/driver.rs"
    ));
    assert!(!mentions_path("mylib.rs:3: struct Y", "lib.rs"));
    assert!(!mentions_path("graphlib.rs/index.js", "lib.rs"));
    assert!(!mentions_path("src/lib.rs.bak", "src/lib.rs"));
    // A miss on one occurrence must not mask a clean later occurrence.
    assert!(mentions_path("mylib.rs and also lib.rs here", "lib.rs"));
    assert!(!mentions_path("anything", ""));
}

#[tokio::test]
async fn unknown_tool_returns_error_not_panic() {
    let (_root, reg) = bare_registry(None);
    let result = reg.execute("nonexistent", &Value::Null).await;
    assert!(result.is_error());
}

/// The measured defect: over a 20-task Terminal-Bench run, 757 of 1,063
/// tool calls were `bash` and the ledger recorded none of them, because
/// `classify_file_op` can only read a tool's *input* and a shell command
/// is an opaque string. With the diff probe dark on a non-git task
/// directory, `file_change_events` is the only channel left that can
/// prove the tree changed — so a blind ledger there is a blind ladder.
#[tokio::test]
async fn a_file_written_by_the_shell_reaches_the_ledger() {
    let (root, reg) = bare_registry(None);
    assert_eq!(reg.mutations_recorded(), 0, "clean before");

    reg.begin_workspace_probe();
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({"command": "printf 'a\\nb\\n' > made_by_shell.txt"}),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    assert!(root.path().join("made_by_shell.txt").exists());
    assert_eq!(
        reg.mutations_recorded(),
        0,
        "nothing is attributed until the turn settles"
    );
    reg.settle_workspace_probe();

    let touched = reg.files_touched();
    assert!(
        touched
            .iter()
            .any(|(path, ops)| path == "made_by_shell.txt" && ops.contains('C')),
        "shell-created file missing from the ledger: {touched:?}"
    );
    // The count the verification ladder reads.
    assert!(reg.mutations_recorded() >= 1, "no mutation recorded");
}

/// A shell command that fails can still have written something, so
/// attribution must not hang off the exit code.
#[tokio::test]
async fn a_failing_shell_command_still_reports_what_it_wrote() {
    let (root, reg) = bare_registry(None);

    reg.begin_workspace_probe();
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({"command": "echo partial > half_done.txt; exit 3"}),
        )
        .await;
    // Whatever the tool reports, the tree changed and the ledger says so.
    let _ = out;
    reg.settle_workspace_probe();
    assert!(root.path().join("half_done.txt").exists());
    assert!(
        reg.files_touched()
            .iter()
            .any(|(path, _)| path == "half_done.txt"),
        "a non-zero exit erased the attribution"
    );
}

/// Attribution needs a cause (#1553). A bracket that dispatched no opaque
/// call has nothing the probe could attribute a delta TO — so a file that
/// appears mid-bracket (a human editing beside the run in a shared worktree)
/// must not be recorded as this session's work. Before the gate, settle
/// attributed any delta unconditionally, and the verification ladder read
/// someone else's edit as the run's.
#[tokio::test]
async fn a_bracket_with_no_opaque_call_disowns_foreign_motion() {
    let (root, reg) = bare_registry(None);
    reg.begin_workspace_probe();
    // The tree moves, but through no call this registry dispatched.
    std::fs::write(root.path().join("foreign.txt"), "not ours\n").unwrap();
    reg.settle_workspace_probe();
    assert_eq!(
        reg.mutations_recorded(),
        0,
        "foreign motion attributed to a turn that dispatched nothing: {:?}",
        reg.files_touched()
    );

    // The disowned delta is dropped, never deferred: the settle re-armed
    // from the after-image, so a NEXT bracket that does run an opaque call
    // must not inherit the foreign file as its own work.
    reg.begin_workspace_probe();
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({"command": "echo mine > ours.txt"}),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    reg.settle_workspace_probe();
    let touched = reg.files_touched();
    assert!(
        touched.iter().any(|(path, _)| path == "ours.txt"),
        "the opaque call's own write is still attributed: {touched:?}"
    );
    assert!(
        !touched.iter().any(|(path, _)| path == "foreign.txt"),
        "the earlier foreign edit must not resurface under a later bracket: {touched:?}"
    );
}

/// Reads must not be inflated into mutations: a shell command that only
/// looks at the tree leaves the ledger where it found it.
#[tokio::test]
async fn a_read_only_shell_command_records_no_mutation() {
    let (root, reg) = bare_registry(None);
    std::fs::write(root.path().join("existing.txt"), "unchanged\n").unwrap();

    reg.begin_workspace_probe();
    let out = reg
        .execute("bash", &serde_json::json!({"command": "cat existing.txt"}))
        .await;
    assert!(!out.is_error(), "{out:?}");
    reg.settle_workspace_probe();
    assert_eq!(
        reg.mutations_recorded(),
        0,
        "inspecting the tree is not changing it: {:?}",
        reg.files_touched()
    );
}

/// The per-call probe is a fallback, not dead code. A host that never
/// brackets a turn — every embedder predating the pair, and every test
/// double — must keep exactly the attribution it already had, or gating
/// the fast path would have silently deleted the slow one.
#[tokio::test]
async fn an_unbracketed_session_still_attributes_the_shell_per_call() {
    let (root, reg) = bare_registry(None);
    // Deliberately no begin_workspace_probe(): this host does not bracket.
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({"command": "echo hi > unbracketed.txt"}),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    assert!(root.path().join("unbracketed.txt").exists());
    assert!(
        reg.files_touched()
            .iter()
            .any(|(path, _)| path == "unbracketed.txt"),
        "gating the turn probe cost an unbracketed host its attribution"
    );
}

/// The two granularities are mutually exclusive, and this is the property
/// that says so. Running both walks the tree twice per shell call *and*
/// twice per turn to learn one fact — which on a 900s trial with 757 shell
/// calls costs more than the trial it exists to observe.
#[tokio::test]
async fn a_bracketed_session_attributes_each_shell_write_exactly_once() {
    let (_root, reg) = bare_registry(None);

    reg.begin_workspace_probe();
    for i in 0..3 {
        let out = reg
            .execute(
                "bash",
                &serde_json::json!({"command": format!("echo {i} > f{i}.txt")}),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
    }
    assert_eq!(
        reg.mutations_recorded(),
        0,
        "the per-call probe fired inside a bracket: {:?}",
        reg.files_touched()
    );

    reg.settle_workspace_probe();
    assert_eq!(
        reg.mutations_recorded(),
        3,
        "expected one mutation per created file, no double-attribution: {:?}",
        reg.files_touched()
    );
}

/// The probe holds a real pre-image for every file it can fit in budget,
/// and the counts must come from *that*. Re-reading the file at settle
/// time yields its POST-image, which renders every shell rewrite as
/// 0 added / 0 removed — the same blindness this module exists to remove,
/// one level further down.
#[tokio::test]
async fn a_shell_rewrite_reports_the_line_counts_it_measured() {
    let (root, reg) = bare_registry(None);
    std::fs::write(root.path().join("rewritten.txt"), "one\ntwo\nthree\n").unwrap();

    reg.begin_workspace_probe();
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({
                "command": "printf 'one\\ntwo\\nthree\\nfour\\n' > rewritten.txt"
            }),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    reg.settle_workspace_probe();

    let telemetry = reg.file_touch_telemetry();
    let record = telemetry
        .files_touched
        .iter()
        .find(|r| r.path == "rewritten.txt")
        .expect("the rewritten file never reached the ledger");
    // One line appended: LCS trims the shared prefix, leaving 1 added.
    assert_eq!(
        (record.lines_added, record.lines_removed),
        (1, 0),
        "counts came from the post-image, not the captured pre-image"
    );
}

#[tokio::test]
async fn session_reads_flow_into_saved_exploration_manifest() {
    let (root, reg) = bare_registry(None);
    std::fs::write(root.path().join("evidence.rs"), "fn seen() {}").unwrap();

    // Read through the registry so the file-touch ledger records it.
    let read = reg
        .execute(
            "read_file",
            &serde_json::json!({"path": "evidence.rs", "reason": "test"}),
        )
        .await;
    assert!(!read.is_error(), "{read:?}");

    // Save WITHOUT declaring the file — the ledger must supply it.
    let saved = reg
        .execute(
            "save_exploration",
            &serde_json::json!({
                "slice": "auto", "title": "Auto", "summary": "s", "content": "map"
            }),
        )
        .await;
    match &saved {
        ToolOutput::Ok { content } => {
            assert!(content.contains("1 files tracked"), "{content}")
        }
        other => panic!("{other:?}"),
    }
}

/// The ledger key is the registry's channel, never the model's: with
/// zero session reads a model-authored `_session_read_files` used to
/// pass through untouched and hash fabricated evidence into the
/// manifest.
#[tokio::test]
async fn model_authored_ledger_key_is_dropped_when_the_session_read_nothing() {
    let (root, reg) = bare_registry(None);
    std::fs::write(root.path().join("sneaky.rs"), "fn planted() {}").unwrap();

    let saved = reg
        .execute(
            "save_exploration",
            &serde_json::json!({
                "slice": "fab", "title": "Fab", "summary": "s", "content": "map",
                crate::exploration::LEDGER_FILES_KEY: ["sneaky.rs"]
            }),
        )
        .await;
    match &saved {
        ToolOutput::Ok { content } => {
            assert!(
                content.contains("0 files tracked"),
                "no session reads means no ledger evidence: {content}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn fresh_map_hints_once_on_covering_search_results() {
    let (root, reg) = bare_registry(None);
    std::fs::write(root.path().join("covered.rs"), "fn mapped() {}").unwrap();
    let saved = reg
        .execute(
            "save_exploration",
            &serde_json::json!({
                "slice": "zone", "title": "Zone", "summary": "s", "content": "map",
                "files": ["covered.rs"]
            }),
        )
        .await;
    assert!(!saved.is_error(), "{saved:?}");
    // The author never needs a hint about its own map — simulate a new
    // session (fresh registry, same workspace) which does.
    let reg2 = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    let hit = reg2
        .execute("grep", &serde_json::json!({"pattern": "mapped"}))
        .await;
    match &hit {
        ToolOutput::Ok { content } => {
            assert!(
                content.contains("saved exploration `zone`"),
                "first covering search must carry the hint: {content}"
            );
        }
        other => panic!("{other:?}"),
    }

    // Once per session: the second covering search stays clean.
    let again = reg2
        .execute("grep", &serde_json::json!({"pattern": "mapped"}))
        .await;
    match &again {
        ToolOutput::Ok { content } => {
            assert!(
                !content.contains("saved exploration `zone`"),
                "hint must not repeat: {content}"
            );
        }
        other => panic!("{other:?}"),
    }

    // And the author's own registry was seeded as already-hinted.
    let author_search = reg
        .execute("grep", &serde_json::json!({"pattern": "mapped"}))
        .await;
    match &author_search {
        ToolOutput::Ok { content } => {
            assert!(
                !content.contains("saved exploration `zone`"),
                "author must not be hinted about its own map: {content}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_registry_tool_is_reserved_against_custom_shadowing() {
    // RESERVED_NAMES is now an alias for catalog::ALL_NAMES rather than a
    // hand-mirrored copy (#450), so declaring a tool reserves it. What
    // still needs proving is the *other* direction: a tool that registers
    // but was never declared is shadowable by a custom manifest.
    let (_root, reg) = bare_registry(Some(IssueBackend::GitHub));
    for schema in reg.schemas() {
        assert!(
            crate::custom::RESERVED_NAMES.contains(&schema.name.as_str()),
            "built-in tool `{}` is missing from custom::RESERVED_NAMES — \
             declare it in stella-tools/src/catalog.rs, or a custom \
             manifest could shadow it",
            schema.name
        );
    }

    // Conditionally-registered tools may not show up in a bare registry's
    // schemas (the media tools need a capable key, `web_search` a search
    // key), so the registry-driven loop above can't reach them. The
    // catalog declares them, and the alias reserves them — assert the
    // conditional half of the table is genuinely covered.
    for name in
        crate::catalog::names_where(|a| a != crate::catalog::Availability::Always && a.is_native())
    {
        assert!(
            crate::custom::RESERVED_NAMES.contains(&name),
            "conditionally-registered tool `{name}` is not reserved"
        );
    }
    // The CLI's session layer is reserved too — it is not in any registry.
    for name in ["ask_user", "tool_search", "install_skill"] {
        assert!(crate::custom::RESERVED_NAMES.contains(&name), "{name}");
    }
}

/// Witness for #450: the advertised set is pinned as an exact **name set**
/// against the canonical catalog, not as a count. A magic-number pin
/// (`names.len() == 50`) merges to a plausible-but-wrong integer when two
/// parallel PRs each bump it off the same base — this fails by name
/// instead, naming exactly what was added or dropped.
#[test]
fn registry_advertises_exactly_the_catalog_tool_set() {
    let (_root, reg) = bare_registry(Some(IssueBackend::GitHub));
    let schemas = reg.schemas();
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        with_ambient_search(crate::catalog::always_on_with_issues()),
        "registry disagrees with catalog::CATALOG — register the tool, or \
         add/remove its line in stella-tools/src/catalog.rs"
    );
    // `bash` IS in the default surface. Switching it off is settings work,
    // and it happens above this registry (crate::policy::ToolPolicy).
    assert!(names.contains(&"bash"), "{names:?}");
}

/// Witness for #450: a tool that registers but was never declared in the
/// catalog is caught by *name*. Simulated here by dropping a name from the
/// expectation — the same failure a real unregistered tool produces.
#[test]
fn an_undeclared_tool_fails_the_catalog_pin_by_name() {
    let (_root, reg) = bare_registry(Some(IssueBackend::GitHub));
    let schemas = reg.schemas();
    let live: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    let mut catalog_missing_one = with_ambient_search(crate::catalog::always_on_with_issues());
    let dropped = catalog_missing_one.pop().expect("catalog is non-empty");
    assert_ne!(
        live, catalog_missing_one,
        "a catalog missing `{dropped}` must not compare equal to the live \
         registry — otherwise the pin cannot catch an undeclared tool"
    );
    // And the difference is reported as a name, not an arity.
    assert!(!catalog_missing_one.contains(&dropped));
    assert!(live.contains(&dropped));
}

// bash and the web family ship ON (the default flip)

/// **Witness for the default flip.** With no options at all — no
/// settings, no opt-in, nothing — `bash` is advertised AND dispatchable.
/// This test fails on the old code, where `RegistryOptions::bash`
/// defaulted to `false` and the shell was absent until a settings key
/// said otherwise.
#[tokio::test]
async fn bash_is_registered_with_no_options_at_all() {
    let (_root, reg) = bare_registry(None);
    assert!(
        reg.schemas().iter().any(|s| s.name == "bash"),
        "bash must be advertised with no configuration"
    );
    let out = reg
        .execute(
            "bash",
            &serde_json::json!({"command": "echo bash_default_on"}),
        )
        .await;
    match out {
        ToolOutput::Ok { content } => assert!(content.contains("bash_default_on")),
        ToolOutput::Error { message } => panic!("default bash must run: {message}"),
    }
    // The default options carry no policy at all any more — the only
    // switch left is the host attestation.
    assert!(
        RegistryOptions::default()
            .media_host_data_isolation
            .is_none()
    );
}

/// **Witness for the default flip, web half.** The three key-free web
/// tools register with no options; `web_search` still needs a search key,
/// so its presence is environment-dependent and not pinned here.
#[test]
fn the_key_free_web_family_is_registered_with_no_options_at_all() {
    let (_root, reg) = bare_registry(None);
    let schemas = reg.schemas();
    for (expected, read_only) in [
        ("web_fetch", true),
        ("web_extract_assets", true),
        ("web_download", false),
    ] {
        let schema = schemas
            .iter()
            .find(|s| s.name == expected)
            .unwrap_or_else(|| panic!("{expected} must register with no configuration"));
        assert_eq!(schema.read_only, read_only, "{expected}");
    }
}

/// The schema list is serialized verbatim into the prompt prefix; a
/// nondeterministic order (HashMap iteration) breaks byte-level
/// prompt-cache prefix matching across processes.
#[test]
fn schemas_are_sorted_by_name_for_prompt_cache_stability() {
    let (_root, reg) = bare_registry(Some(IssueBackend::GitHub));
    let names: Vec<String> = reg.schemas().iter().map(|s| s.name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "schemas must be name-sorted");
}

#[test]
fn issue_tools_absent_without_a_configured_backend() {
    let (_root, reg) = bare_registry(None);
    let schemas = reg.schemas();
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    // The no-backend surface is a *filtered view* of the same canonical
    // table, not a second count to keep in sync (#450).
    assert_eq!(
        names,
        with_ambient_search(crate::catalog::always_on()),
        "the no-backend registry must be exactly the always-on catalog"
    );
    for absent in crate::catalog::names_where(|a| a == crate::catalog::Availability::Issue) {
        assert!(!names.contains(&absent), "{absent} must be absent");
    }
}

#[test]
fn video_tools_register_only_with_a_video_capable_media_backend() {
    // A provider that satisfies the port but is never called — tool
    // registration is what's under test here, not generation.
    struct NullMedia;
    #[async_trait]
    impl stella_media::MediaProvider for NullMedia {
        fn id(&self) -> &str {
            "null"
        }
        fn capabilities(&self) -> stella_media::MediaCapabilities {
            stella_media::MediaCapabilities::default()
        }
        async fn generate_image(
            &self,
            _req: stella_media::ImageRequest,
        ) -> Result<stella_media::MediaArtifact, stella_media::MediaError> {
            Err(stella_media::MediaError::Terminal("not under test".into()))
        }
        async fn generate_video(
            &self,
            _req: stella_media::VideoRequest,
        ) -> Result<stella_media::MediaJob, stella_media::MediaError> {
            Err(stella_media::MediaError::Terminal("not under test".into()))
        }
        async fn poll_video(
            &self,
            _job: &stella_media::MediaJob,
        ) -> Result<stella_media::MediaJobStatus, stella_media::MediaError> {
            Err(stella_media::MediaError::Terminal("not under test".into()))
        }
    }
    let provider: Arc<dyn stella_media::MediaProvider> = Arc::new(NullMedia);
    struct FixedIds;
    impl crate::media::MediaOperationIdSource for FixedIds {
        fn operation_id(&self) -> crate::media::HostMediaOperation {
            crate::media::HostMediaOperation {
                opaque_id: "op-registry-test".into(),
                expires_at: u64::MAX,
            }
        }
    }
    // A full approving host context: the paid tools register only under
    // one (#785).
    let host_options = || RegistryOptions {
        media_spend_gate: Some(Arc::new(stella_media::DenyMediaSpendGate)),
        media_operation_ids: Some(Arc::new(FixedIds)),
        media_operation_journal: Some(Arc::new(
            stella_media::SqliteMediaOperationJournal::open_in_memory(Default::default()).unwrap(),
        )),
        media_requires_host_approval: true,
        media_host_data_isolation: Some(crate::media::HostDataIsolation::ProcessFree),
    };
    let names = |backend, options: RegistryOptions| {
        let root = tempfile::tempdir().unwrap();
        ToolRegistry::with_backends_and_options(
            root.path().to_path_buf(),
            None,
            Some(backend),
            options,
        )
        .schemas()
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>()
    };

    let with_video = names(
        crate::media::MediaBackend {
            image: provider.clone(),
            video: Some(provider.clone()),
        },
        host_options(),
    );
    for expected in ["generate_image", "generate_video", "poll_video"] {
        assert!(
            with_video.contains(&expected.to_string()),
            "missing {expected}: {with_video:?}"
        );
    }

    let image_only = names(
        crate::media::MediaBackend {
            image: provider.clone(),
            video: None,
        },
        host_options(),
    );
    assert!(image_only.contains(&"generate_image".to_string()));
    for absent in ["generate_video", "poll_video"] {
        assert!(
            !image_only.contains(&absent.to_string()),
            "{absent} must be absent without a video adapter"
        );
    }

    // The #785 ruling: with credentials but NO approving host context,
    // the paid tools would deny every call — so none of them surface.
    // The free, client-side generate_svg still does.
    let deny_only = names(
        crate::media::MediaBackend {
            image: provider,
            video: Some(Arc::new(NullMedia)),
        },
        RegistryOptions::default(),
    );
    for absent in ["generate_image", "generate_video", "poll_video"] {
        assert!(
            !deny_only.contains(&absent.to_string()),
            "{absent} must not register without a host context"
        );
    }
    assert!(deny_only.contains(&"generate_svg".to_string()));
}

#[tokio::test]
async fn graph_query_is_advertised_without_an_index_and_builds_one_on_first_use() {
    // `graph_query` is advertised from the start, even
    // in a workspace with no `.stella/private/codegraph.db`. Hiding it
    // until an index existed hid the very tool that builds one, so the
    // agent grepped instead. The first call builds the index it needs.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let reg = ToolRegistry::with_issue_backend(root.clone(), None);
    let has_graph_query = |r: &ToolRegistry| r.schemas().iter().any(|s| s.name == "graph_query");

    assert!(
        has_graph_query(&reg),
        "advertised with no index present yet"
    );
    assert!(
        !crate::graph::graph_db_path(&root).exists(),
        "and no index exists at this point"
    );

    std::fs::write(root.join("lib.rs"), "pub fn f() {}\n").unwrap();
    // The first query builds the index and answers — no `stella init`,
    // no prior `enable_code_graph_if_available`.
    let out = reg
        .execute(
            "graph_query",
            &serde_json::json!({"op": "definitions", "target": "f"}),
        )
        .await;
    assert!(!out.is_error(), "first-use build must dispatch: {out:?}");
    assert!(
        crate::graph::graph_db_path(&root).exists(),
        "the call built the index on first use"
    );
}

/// Issue #330 witness: `read_symbol` dispatches through the registry,
/// returns exactly the graph-resolved span, lands the same `R`
/// file-touch event a `read_file` of that file would, and shares one
/// per-file read tally with `read_file`.
#[tokio::test]
async fn read_symbol_reads_the_exact_span_and_lands_a_file_touch_read_event() {
    let (root, reg) = bare_registry(None);
    std::fs::write(
        root.path().join("lib.rs"),
        "fn before() {}\n\nfn span_me() {\n    let x = 1;\n}\n",
    )
    .unwrap();

    let out = reg
        .execute(
            "read_symbol",
            &serde_json::json!({"name": "span_me", "reason": "witness"}),
        )
        .await;
    match &out {
        ToolOutput::Ok { content } => {
            assert!(content.contains("fn span_me (lib.rs:3-5)"), "{content}");
            assert!(content.contains("3\tfn span_me() {"), "{content}");
            assert!(!content.contains("before"), "exactly the span: {content}");
        }
        other => panic!("read_symbol must dispatch: {other:?}"),
    }

    // The read landed in the file-touch ledger as an `R` on the resolved
    // path — audit parity with `read_file`.
    let touched = reg.files_touched();
    assert!(
        touched
            .iter()
            .any(|(path, ops)| path == "lib.rs" && ops.contains('R')),
        "read_symbol must land an R event: {touched:?}"
    );

    // And the per-file read tally is shared: a `read_file` of the same
    // file counts as the second read this session.
    let second = reg
        .execute("read_file", &serde_json::json!({"path": "lib.rs"}))
        .await;
    match &second {
        ToolOutput::Ok { content } => {
            assert!(
                content.contains("read 2× this session"),
                "one tally across both read surfaces: {content}"
            );
        }
        other => panic!("read_file must read: {other:?}"),
    }
}

#[test]
fn read_only_flags_partition_the_registry_correctly() {
    // The engine parallelizes on this flag — a mutating tool marked
    // read-only would race writes; a read-only tool marked mutating
    // just loses concurrency. Pin the partition explicitly.
    // The expectation is the canonical catalog's flag, so the partition
    // lives in one place rather than being restated here (#450).
    let (_root, reg) = bare_registry(Some(IssueBackend::GitHub));
    for schema in reg.schemas() {
        let entry = crate::catalog::get(&schema.name).unwrap_or_else(|| {
            panic!(
                "`{}` registers but is not declared in catalog::CATALOG",
                schema.name
            )
        });
        assert_eq!(
            schema.read_only, entry.read_only,
            "read_only flag wrong for {}",
            schema.name
        );
        assert_eq!(
            schema.speculation_safe, entry.speculation_safe,
            "speculation_safe flag wrong for {} — the engine runs \
             exactly this partition before a step commits, so a schema \
             drifting from the catalog either double-bills a metered \
             read or silently stops speculating a pure one (#923)",
            schema.name
        );
    }
}

/// A baseline snapshot with the given tables in the implicit `sql`
/// layer, `default` namespace — the shape `stella init` would seed.
/// `pub(super)` so the batch-gate witnesses (`gate_batch`) reuse
/// the same fixture instead of growing a drifting copy.
pub(super) fn seeded_snapshot(tables: &[&str]) -> stella_graph::StorageSnapshot {
    stella_graph::StorageSnapshot {
        layers: vec![],
        relations: tables
            .iter()
            .map(|name| stella_graph::storage::RelationEntry {
                address: stella_graph::storage::relation_address("sql", "default", name),
                layer: "sql".into(),
                namespace: "default".into(),
                name: name.to_string(),
                kind: "table".into(),
                fields: vec![stella_graph::storage::FieldEntry {
                    name: "id".into(),
                    data_type: Some("INT".into()),
                    nullable: false,
                    default_value: None,
                    constraints: vec!["PRIMARY KEY".into()],
                    references: None,
                    intent: None,
                    line: 1,
                }],
                enum_values: vec![],
                intent: None,
                boundary: None,
                redirects: vec![],
                source: Some("migrations/001.sql:1".into()),
            })
            .collect(),
        ..Default::default()
    }
}

/// Fresh registry over a fresh tempdir, no optional backends. Lives here
/// rather than in `touch`, because `chain` builds its fixtures the same way
/// and sibling modules cannot see each other's helpers.
fn telemetry_fixture() -> (tempfile::TempDir, ToolRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    (dir, reg)
}

async fn exec_ok(reg: &ToolRegistry, name: &str, input: serde_json::Value) {
    let out = reg.execute(name, &input).await;
    assert!(!out.is_error(), "{name} {input} failed: {out:?}");
}

mod chain;
mod fence;
mod file_change;
mod gate_batch;
mod partial_belief;
#[cfg(unix)]
mod private_state;
mod schema_gate;
mod touch;

/// The no-clobber guarantee, end to end and across two independent sessions.
///
/// Two registries over one workspace stand in for two agents: they share no
/// state, no lock and no channel, exactly as two processes would. The claim is
/// that A cannot silently overwrite B's edit — not that A is made to wait for
/// it.
#[tokio::test]
async fn one_agent_cannot_clobber_another_agents_edit() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("shared.txt");
    std::fs::write(&file, "original\n").unwrap();

    let agent_a = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let agent_b = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    // A reads the file — this is the belief its later write will act on.
    let read = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "shared.txt", "reason": "planning an edit" }),
        )
        .await;
    assert!(!read.is_error(), "A can read the file: {read:?}");

    // B edits it. B has never heard of A and takes no lock.
    let b_wrote = agent_b
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "shared.txt",
                "content": "B's careful work\n",
                "reason": "B does its job",
            }),
        )
        .await;
    assert!(!b_wrote.is_error(), "B's write lands: {b_wrote:?}");

    // A now writes against content that no longer exists. This is the exact
    // moment work gets destroyed in a lock-based design — B has released, so
    // A proceeds and B's edit is gone.
    let a_wrote = agent_a
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "shared.txt",
                "content": "A's stale work\n",
                "reason": "A does its job",
            }),
        )
        .await;
    match &a_wrote {
        ToolOutput::Error { message } => {
            assert!(
                message.contains("shared.txt"),
                "the refusal names the file so the model can act on it: {message}"
            );
            assert!(
                message.contains("nothing was written"),
                "the refusal says the edit was not half-applied: {message}"
            );
        }
        other => panic!("A's stale write must be refused, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "B's careful work\n",
        "B's work survives untouched — this is the whole guarantee"
    );

    // And the recovery is one cheap step, not a failed turn: re-read, then
    // write. This is why staleness beats a lock for an agent — being told to
    // look again costs a step; blocking costs a turn.
    let reread = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "shared.txt", "reason": "re-reading after the refusal" }),
        )
        .await;
    assert!(!reread.is_error(), "A re-reads: {reread:?}");
    let retry = agent_a
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "shared.txt",
                "content": "A's work, rebased on B's\n",
                "reason": "redone against current content",
            }),
        )
        .await;
    assert!(
        !retry.is_error(),
        "after re-reading, A writes normally: {retry:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "A's work, rebased on B's\n"
    );
}

/// A ranged read must not launder an unseen edit into belief.
///
/// The remembered digest is of the WHOLE file, but a read only shows what it
/// was asked for. So a peek at a region someone else did not touch used to
/// overwrite the belief with the current hash — telling the guard the agent
/// had seen an edit that was never on its screen — and the next write sailed
/// through clean. That is the silent case the guard exists to prevent: no
/// refusal, no warning, and the other agent's work simply gone.
///
/// `read_file` windows at 2000 lines by default and `read_symbol` reads a
/// span, so the partial read is the ordinary case, not an exotic one.
#[tokio::test]
async fn a_ranged_read_does_not_launder_an_unseen_edit_into_belief() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("shared.txt");
    // Long enough that a limited read is genuinely a window onto part of it.
    let original: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&file, &original).unwrap();

    let agent_a = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let agent_b = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    // A reads the whole file. This belief is honest — A has seen every line.
    let read = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "shared.txt", "reason": "planning an edit" }),
        )
        .await;
    assert!(!read.is_error(), "A reads the whole file: {read:?}");

    // B rewrites the tail. B has never heard of A and takes no lock.
    let mut edited: String = (1..=39).map(|n| format!("line {n}\n")).collect();
    edited.push_str("line 40 — B's careful work\n");
    let b_wrote = agent_b
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "shared.txt",
                "content": edited,
                "reason": "B does its job",
            }),
        )
        .await;
    assert!(!b_wrote.is_error(), "B's write lands: {b_wrote:?}");

    // A peeks at the HEAD of the file — a region B did not touch, so this read
    // tells A nothing about B's edit. It must not count as having seen it.
    let peek = agent_a
        .execute(
            "read_file",
            &serde_json::json!({
                "path": "shared.txt",
                "offset": 1,
                "limit": 3,
                "reason": "checking the header",
            }),
        )
        .await;
    assert!(!peek.is_error(), "the ranged read succeeds: {peek:?}");

    let a_wrote = agent_a
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "shared.txt",
                "content": original,
                "reason": "A does its job",
            }),
        )
        .await;
    match &a_wrote {
        ToolOutput::Error { message } => assert!(
            message.contains("shared.txt"),
            "the refusal names the file so the model can act on it: {message}"
        ),
        other => panic!("A's write over an edit it never saw must be refused, got {other:?}"),
    }
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("B's careful work"),
        "B's work survives — a peek at another part of the file is not consent"
    );
}

/// A session writing its own file repeatedly must never trip the guard — the
/// digest it remembers after each write is the one it finds on the next.
/// Without this, the guarantee would make ordinary single-agent work fail.
#[tokio::test]
async fn a_session_never_conflicts_with_itself() {
    let root = tempfile::tempdir().unwrap();
    let agent = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    for i in 0..3 {
        let out = agent
            .execute(
                "write_file",
                &serde_json::json!({
                    "path": "mine.txt",
                    "content": format!("revision {i}\n"),
                    "reason": "iterating",
                }),
            )
            .await;
        assert!(!out.is_error(), "write {i} succeeds: {out:?}");
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("mine.txt")).unwrap(),
        "revision 2\n"
    );
}

/// Every mutation is durably recorded without the agent deciding to commit.
///
/// This is the answer to work lost when a turn dies before its commit: the
/// hook is the tool, not the model's judgement, so there is no window in which
/// a write exists only in the working tree.
#[tokio::test]
async fn every_mutation_is_committed_without_the_agent_choosing_to() {
    let root = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let journal = stella_store::work_journal::WorkJournal::open_in(
        store.path(),
        root.path(),
        "ses-autocommit",
    )
    .unwrap();
    reg.attach_work_journal(journal.clone(), "lead");

    // One ordinary write. The agent never asks for a commit.
    let out = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "work.txt",
                "content": "the work\n",
                "reason": "doing the job",
            }),
        )
        .await;
    assert!(!out.is_error(), "the write succeeds: {out:?}");

    journal.mark_turn(1, &journal_tip(&journal)).unwrap();
    assert_eq!(
        journal.read_at_turn(1, "work.txt").unwrap(),
        "the work\n",
        "the mutation is durable the instant it lands, with no commit step"
    );

    // A second write supersedes it in the record rather than appending noise.
    let out = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "work.txt",
                "content": "revised\n",
                "reason": "revising",
            }),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    journal.mark_turn(2, &journal_tip(&journal)).unwrap();
    assert_eq!(journal.read_at_turn(2, "work.txt").unwrap(), "revised\n");
    assert_eq!(
        journal.read_at_turn(1, "work.txt").unwrap(),
        "the work\n",
        "and turn 1 still replays its own version — history, not a snapshot"
    );
}

/// The session ref's current commit. A test affordance: production callers
/// mark turns from the commit `record` returns.
fn journal_tip(journal: &stella_store::work_journal::WorkJournal) -> String {
    journal.session_tip().expect("a mutation was recorded")
}

/// Switching sessions must not inherit the previous session's beliefs.
///
/// The deck reuses one registry across a session switch, so a merging restore
/// left the departing session's observations in place — and the arriving
/// session was then refused writes to files it had never read. A guard built
/// to have no false positives cannot carry another session's memory.
#[test]
fn restoring_a_session_replaces_rather_than_inherits() {
    let root = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    // Session A saw two files, one of which B never touches.
    let a = serde_json::json!({ "shared.rs": "aaa", "only-a-saw-this.rs": "bbb" }).to_string();
    assert_eq!(reg.restore_observed(&a), 2);

    // Session B's record disagrees about the shared file and never mentions
    // the other one at all.
    let b = serde_json::json!({ "shared.rs": "ccc" }).to_string();
    assert_eq!(reg.restore_observed(&b), 1);

    let json = reg
        .observed_snapshot()
        .expect("a restored session has a snapshot");
    let observed: std::collections::HashMap<String, String> =
        serde_json::from_str(&json).expect("the snapshot round-trips");
    assert_eq!(
        observed.get("shared.rs").map(String::as_str),
        Some("ccc"),
        "the arriving session's digest wins — `or_insert` used to keep the stale one"
    );
    assert!(
        !observed.contains_key("only-a-saw-this.rs"),
        "a file only the departing session read must not guard the arriving one"
    );
}

/// A windowed read is still a read the guard watched, so the file it looked at
/// must stay guarded.
///
/// `read_file` renders at most 2000 lines, so on a file above that ceiling
/// EVERY read is partial. Grading a partial read as "no belief at all" left
/// exactly those files — the big ones, the ones several agents are most likely
/// to be in at once — outside the no-clobber guarantee entirely: no entry in
/// the staleness map means `clobbered_paths` has nothing to compare and the
/// overwrite lands with no refusal.
#[tokio::test]
async fn a_windowed_read_still_guards_against_a_concurrent_edit() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("big.txt");
    let original: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &original).unwrap();

    let agent_a = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let agent_b = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    // A reads a WINDOW — what every read of an over-ceiling file looks like.
    let read = agent_a
        .execute(
            "read_file",
            &serde_json::json!({
                "path": "big.txt", "offset": 1, "limit": 10, "reason": "reading the head",
            }),
        )
        .await;
    assert!(!read.is_error(), "A's windowed read: {read:?}");

    // B appends work A was never shown.
    let mut edited = original.clone();
    edited.push_str("B's careful work\n");
    let b_wrote = agent_b
        .execute(
            "write_file",
            &serde_json::json!({ "path": "big.txt", "content": edited, "reason": "B does its job" }),
        )
        .await;
    assert!(!b_wrote.is_error(), "B's write lands: {b_wrote:?}");

    let a_wrote = agent_a
        .execute(
            "write_file",
            &serde_json::json!({ "path": "big.txt", "content": "A's stale work\n", "reason": "A does its job" }),
        )
        .await;
    match &a_wrote {
        ToolOutput::Error { message } => assert!(
            message.contains("big.txt"),
            "the refusal names the file: {message}"
        ),
        other => panic!("a write over an unseen edit must be refused, got {other:?}"),
    }
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("B's careful work"),
        "B's work survives — a windowed reader is still a guarded one"
    );
}

/// The other direction: a partial read must not overwrite a whole-file belief.
///
/// A saw the file whole, B edited a region A never looked at again, and A's
/// `read_symbol` span learns nothing about B's edit. Refreshing the belief on
/// that span would compare B's content against itself and wave A's write
/// through — the laundering this grade exists to prevent.
#[tokio::test]
async fn a_span_read_cannot_launder_a_whole_file_belief() {
    // `target_fn` is lines 5-8 in both versions; only `alpha`'s body differs,
    // so the span A re-reads is byte-identical and tells it nothing.
    const BEFORE: &str = "fn alpha() {\n    let a = 1;\n}\n\nfn target_fn() {\n    let x = 1;\n    let y = 2;\n}\n\nfn omega() {}\n";
    const AFTER: &str = "fn alpha() {\n    let a = 999;\n}\n\nfn target_fn() {\n    let x = 1;\n    let y = 2;\n}\n\nfn omega() {}\n";

    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("lib.rs");
    std::fs::write(&file, BEFORE).unwrap();
    let db = crate::graph::graph_db_path(root.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let graph = stella_graph::CodeGraph::open(root.path(), &db).unwrap();
    graph.index_all().unwrap();
    graph.shutdown();

    let agent_a = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let agent_b = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    let read = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "lib.rs", "reason": "reading it whole" }),
        )
        .await;
    assert!(!read.is_error(), "A's whole-file read: {read:?}");

    let b_wrote = agent_b
        .execute(
            "write_file",
            &serde_json::json!({ "path": "lib.rs", "content": AFTER, "reason": "B edits alpha" }),
        )
        .await;
    assert!(!b_wrote.is_error(), "B's write lands: {b_wrote:?}");

    let span = agent_a
        .execute(
            "read_symbol",
            &serde_json::json!({ "name": "target_fn", "reason": "peeking at one symbol" }),
        )
        .await;
    assert!(!span.is_error(), "A's span read: {span:?}");

    let a_wrote = agent_a
        .execute(
            "write_file",
            &serde_json::json!({ "path": "lib.rs", "content": "A's stale work\n", "reason": "A does its job" }),
        )
        .await;
    assert!(
        a_wrote.is_error(),
        "a span read must not refresh a whole-file belief, got {a_wrote:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        AFTER,
        "B's edit survives the peek"
    );
}

/// The refusal has to be recoverable on a file too big to read whole.
///
/// Above `read_file`'s 2000-line ceiling a whole-file read is unreachable, so
/// a rule of "only a whole-file read may refresh" would refuse the write, then
/// refuse every re-read's attempt to clear it — the agent could never touch
/// the file again. A partial belief is refreshable by the partial reads that
/// are all such a file can offer.
///
/// What the re-read buys back is the right to *edit*, not the right to replace
/// the file whole: clearing the drift says the bytes have not moved, and says
/// nothing about the 100 lines past the window that A still has not read. Those
/// are two separate refusals and this pins both — see the `partial_belief`
/// submodule for the coverage rule itself.
#[tokio::test]
async fn a_windowed_re_read_recovers_a_file_too_big_to_read_whole() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("huge.txt");
    let original: String = (1..=2_100).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &original).unwrap();

    let agent_a = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    let agent_b = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    let read = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "huge.txt", "reason": "first look" }),
        )
        .await;
    assert!(!read.is_error(), "A's read: {read:?}");

    let mut edited = original.clone();
    edited.push_str("B's careful work\n");
    let b_wrote = agent_b
        .execute(
            "write_file",
            &serde_json::json!({ "path": "huge.txt", "content": edited, "reason": "B does its job" }),
        )
        .await;
    assert!(!b_wrote.is_error(), "B's write lands: {b_wrote:?}");

    let refused = agent_a
        .execute(
            "write_file",
            &serde_json::json!({ "path": "huge.txt", "content": "A's stale work\n", "reason": "A does its job" }),
        )
        .await;
    assert!(refused.is_error(), "the file is guarded: {refused:?}");

    // The recovery the refusal asks for — the only read this file can give.
    let reread = agent_a
        .execute(
            "read_file",
            &serde_json::json!({ "path": "huge.txt", "reason": "re-reading after the refusal" }),
        )
        .await;
    assert!(!reread.is_error(), "A re-reads: {reread:?}");

    // The drift is now cleared: the bytes A holds are the bytes on disk. What
    // the re-read could NOT buy back is the tail — a 2,100-line file windows at
    // 2,000, so 100 lines have still never been on A's screen and replacing the
    // file whole would take them out. Same call, different refusal.
    let still_refused = agent_a
        .execute(
            "write_file",
            &serde_json::json!({ "path": "huge.txt", "content": "A's work, rebased\n", "reason": "redone" }),
        )
        .await;
    match &still_refused {
        ToolOutput::Error { message } => assert!(
            message.contains("only seen part of"),
            "B's edit is no longer the objection; the unseen tail is: {message}"
        ),
        other => panic!("a whole-file rewrite over an unread tail must be refused, got {other:?}"),
    }

    // And this is the step that proves it is not a deadlock: a targeted edit
    // lands, because it leaves every line A never read exactly where it was.
    let retry = agent_a
        .execute(
            "edit_file",
            &serde_json::json!({
                "path": "huge.txt",
                "old_string": "line 7\n",
                "new_string": "line 7 — rebased on B\n",
                "reason": "redone against a region A actually read",
            }),
        )
        .await;
    assert!(
        !retry.is_error(),
        "after re-reading, A edits normally — no deadlock: {retry:?}"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("B's careful work"),
        "and B's work is still there — A changed only what it looked at"
    );
}

/// The coverage grade has to survive a crash, or a resumed session silently
/// regains the power to launder the belief it restored.
#[test]
fn a_beliefs_coverage_survives_the_snapshot_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    let stored = serde_json::json!({ "whole.rs": "aaa", "windowed.rs": "partial:bbb" }).to_string();
    assert_eq!(reg.restore_observed(&stored), 2);

    let json = reg.observed_snapshot().expect("a snapshot");
    let observed: std::collections::HashMap<String, String> = serde_json::from_str(&json).unwrap();
    assert_eq!(
        observed.get("whole.rs").map(String::as_str),
        Some("aaa"),
        "a whole-file belief stays a bare digest — the pre-coverage format, unchanged"
    );
    assert_eq!(
        observed.get("windowed.rs").map(String::as_str),
        Some("partial:bbb"),
        "a partial belief is still partial after a resume"
    );
}
