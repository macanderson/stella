//! Registry unit tests: construction and gating of the tool set, the
//! read-only/speculation-safe schema claims, and per-tool dispatch.
//!
//! Split out of `registry.rs` so the module that ships the tools is not
//! dominated by the module that checks them, joining the four sibling test
//! files already here. No test changed in the move.

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
/// `pub(super)` so the batch-gate witnesses (`gate_batch_tests`) reuse
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
