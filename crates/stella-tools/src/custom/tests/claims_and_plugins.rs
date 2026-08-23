// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Manifest claims (#3287) and plugin-contributed tools (#3380): the two
//! subjects `tests.rs` split out to stay clear of the 1500-line ceiling.
//! A child of [`super`], so `super::*` still reaches the private surface —
//! see `custom/tests.rs`'s module doc for the pattern.

use super::*;

// #3287 — manifest claims are recorded, displayed, and buy nothing.

#[test]
fn manifest_claims_parse_into_the_declared_contract_and_buy_nothing() {
    let manifest = r#"
name = "peek"
description = "look at things"
command = ["./peek.sh"]
read_only = true
risk = "low"
idempotent = true
"#;
    let tool = parse_manifest(manifest, Path::new("peek.toml")).unwrap();
    assert!(tool.claimed_read_only);
    assert_eq!(tool.claimed_risk, Some(stella_protocol::RiskLevel::Low));
    assert!(tool.claimed_idempotent);

    // The advertised schema never carries the claim: dispatch trusts the
    // bit directly, and a self-report must not buy concurrency.
    assert!(!tool.schema().read_only);

    // The contract carries the claim verbatim, at untrusted provenance —
    // visible to policy, vouched for by nobody.
    let contract = tool.contract();
    assert_eq!(contract.provenance, stella_protocol::Provenance::Declared);
    assert!(contract.schema.read_only, "the claim is preserved");
    assert!(!contract.trusted_read_only(), "and buys no trust");
    assert!(contract.idempotent);
    assert_eq!(
        contract.risk,
        stella_protocol::RiskLevel::High,
        "a claimed `low` never lowers the declared grade"
    );
    assert_eq!(
        tool.claims_label(),
        "[claims read-only, risk: low, idempotent] "
    );
}

#[test]
fn a_claimed_destructive_risk_raises_the_grade_and_a_claimless_manifest_is_unchanged() {
    let destructive = r#"
name = "wipe"
description = "deletes things"
command = ["./wipe.sh"]
risk = "destructive"
"#;
    let tool = parse_manifest(destructive, Path::new("wipe.toml")).unwrap();
    assert_eq!(
        tool.contract().risk,
        stella_protocol::RiskLevel::Destructive,
        "a self-report may make a tool look more dangerous"
    );

    let plain = r#"
name = "plain"
description = "no claims"
command = ["./plain.sh"]
"#;
    let tool = parse_manifest(plain, Path::new("plain.toml")).unwrap();
    assert_eq!(tool.contract().risk, stella_protocol::RiskLevel::High);
    assert_eq!(tool.claims_label(), "");
}

/// The #3287 witness (manifest half): a `read_only = true` claim is carried
/// on the contract, refused by a `Medium` risk ceiling, and absent from the
/// read-only dispatch set — while a genuinely read-only *built-in* passes
/// all three the other way (that half lives in `contracts.rs`'s
/// `a_builtin_resolves_to_its_reviewed_row`).
#[tokio::test]
async fn a_claimed_read_only_custom_tool_is_ceiling_refused_and_outside_the_read_only_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(dir.path(), "peek.sh", "#!/bin/sh\necho ok\n");
    tool.name = "peek".into();
    tool.claimed_read_only = true;
    let set = CustomToolSet::new(&FakeInner, vec![tool], dir.path().to_path_buf());

    // Not in the read-only dispatch set: the claim never touches the
    // advertised schema, so `ReadOnlyTools` (which trusts the bit) excludes
    // the tool entirely.
    let read_only = stella_core::ports::ReadOnlyTools::new(&set);
    assert!(
        !stella_core::ports::ToolExecutor::schemas(&read_only)
            .iter()
            .any(|s| s.name == "peek"),
        "an unreviewed claim must not admit a tool to the read-only set"
    );

    // Refused by a Medium risk ceiling: the contract snapshot the gate takes
    // carries the declared High grade.
    let gated = crate::gated::GatedToolSet::new(
        &set,
        std::sync::Arc::new(stella_core::ports::RiskCeiling::new(
            stella_protocol::RiskLevel::Medium,
        )),
        stella_core::ports::Principal::User,
    );
    let out = gated.execute("peek", &serde_json::json!({})).await;
    assert!(out.is_error(), "a Medium ceiling must refuse it: {out:?}");

    // And the claim IS visible where policy looks: the composed contracts.
    let contract = stella_core::ports::ToolExecutor::contracts(&set)
        .into_iter()
        .find(|c| c.name() == "peek")
        .expect("advertised");
    assert!(contract.schema.read_only && !contract.trusted_read_only());
}

/// The `[output_schema]` promise (#3287): JSON stdout matching the schema
/// flows into the structured half; non-JSON stdout is the tool's own defect.
#[tokio::test]
async fn a_declared_output_schema_holds_a_script_to_its_promise() {
    let dir = tempfile::tempdir().unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    });

    let mut honest = script_tool(
        dir.path(),
        "honest.sh",
        "#!/bin/sh\necho '{\"count\": 3}'\n",
    );
    honest.output_schema = Some(schema.clone());
    match run_custom(&honest, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Ok { data, .. } => {
            assert_eq!(data, Some(serde_json::json!({ "count": 3 })));
        }
        other => panic!("a kept promise must pass: {other:?}"),
    }

    let mut liar = script_tool(dir.path(), "liar.sh", "#!/bin/sh\necho 'not json'\n");
    liar.output_schema = Some(schema.clone());
    match run_custom(&liar, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::Internal));
            assert!(message.contains("output contract"), "{message}");
        }
        other => panic!("a broken promise is a tool defect: {other:?}"),
    }

    let mut wrong = script_tool(
        dir.path(),
        "wrong.sh",
        "#!/bin/sh\necho '{\"count\": \"three\"}'\n",
    );
    wrong.output_schema = Some(schema);
    match run_custom(&wrong, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::Internal));
            assert!(
                message.contains("field `count`"),
                "names the field: {message}"
            );
        }
        other => panic!("a schema breach is a tool defect: {other:?}"),
    }
}

// Plugin-contributed tools (#3380)

/// Write a `<name>.toml` script-tool manifest into `dir`.
fn manifest_at(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).expect("tools dir");
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("name = \"{name}\"\ndescription = \"d\"\ncommand = [\"./{name}.sh\"]\n"),
    )
    .expect("manifest");
}

/// **Witness: a plugin's tool reaches the surface, and it is attributed.**
///
/// Before #3380 a plugin could ship a `tools/` directory and nothing read
/// it: the tool simply did not exist. The provenance is the half that has to
/// hold as hard as the presence — an unattributed third-party tool cannot be
/// authorized as its plugin and cannot be traced back to it.
#[test]
fn a_plugin_contributes_a_tool_and_it_carries_the_plugin_name() {
    let root = tempfile::tempdir().expect("a temp dir");
    let plugin_dir = root.path().join("plugins").join("vera").join("tools");
    manifest_at(&plugin_dir, "vera_review");

    let none = discover_in_scopes(root.path(), None, true);
    assert!(
        none.names().is_empty(),
        "anti-vacuity: without the plugin tier the tool is nowhere"
    );

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            package_dir: root.path().join("plugins").join("vera"),
            dir: plugin_dir,
        }],
    );
    assert_eq!(found.names(), vec!["vera_review"]);
    let (tools, _) = found.into_parts();
    assert_eq!(tools[0].contributed_by.as_deref(), Some("vera"));
}

/// **Witness: a contributed tool authorizes as its plugin, never as the
/// human.** The rule the whole package surface rests on.
#[test]
fn a_contributed_tool_is_authorized_as_the_plugin_and_a_users_own_is_not() {
    let root = tempfile::tempdir().expect("a temp dir");
    manifest_at(&root.path().join(".stella").join("tools"), "mine");
    let plugin_dir = root.path().join("pkg").join("tools");
    manifest_at(&plugin_dir, "theirs");

    let (tools, _) = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            package_dir: root.path().join("pkg"),
            dir: plugin_dir,
        }],
    )
    .into_parts();

    let mine = tools.iter().find(|t| t.name == "mine").expect("mine");
    let theirs = tools.iter().find(|t| t.name == "theirs").expect("theirs");
    assert_eq!(mine.principal(&Principal::User), Principal::User);
    assert_eq!(
        theirs.principal(&Principal::User),
        Principal::Plugin("vera".into()),
        "a plugin's script never runs under the operator's identity"
    );
    // And it does not widen either: a lane's principal is not inherited.
    assert_eq!(
        theirs.principal(&Principal::Role("worker".into())),
        Principal::Plugin("vera".into())
    );
    assert_eq!(
        mine.principal(&Principal::Role("worker".into())),
        Principal::Role("worker".into())
    );
}

/// **Witness: precedence.** A package may not capture a name the user's own
/// manifest already defines, and the collision is reported rather than
/// silently resolved.
#[test]
fn a_plugin_never_takes_a_name_the_user_already_defined() {
    let root = tempfile::tempdir().expect("a temp dir");
    manifest_at(&root.path().join(".stella").join("tools"), "deploy");
    let plugin_dir = root.path().join("pkg").join("tools");
    manifest_at(&plugin_dir, "deploy");

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            package_dir: root.path().join("pkg"),
            dir: plugin_dir,
        }],
    );
    let reasons: Vec<String> = found
        .diagnostics()
        .iter()
        .map(|d| d.reason.clone())
        .collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(
        reasons[0].contains("yours is the one that runs"),
        "{reasons:?}"
    );

    let (tools, _) = found.into_parts();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].contributed_by, None,
        "the surviving `deploy` is the user's own"
    );
}

/// Plugin-versus-plugin resolves deterministically by the order the host
/// supplies (roster order, i.e. plugin name), and the loser is reported.
#[test]
fn two_plugins_claiming_one_name_resolve_in_roster_order() {
    let root = tempfile::tempdir().expect("a temp dir");
    let first = root.path().join("alpha").join("tools");
    let second = root.path().join("zeta").join("tools");
    manifest_at(&first, "shared");
    manifest_at(&second, "shared");

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[
            PluginToolDir {
                plugin: "alpha".into(),
                package_dir: root.path().join("alpha"),
                dir: first,
            },
            PluginToolDir {
                plugin: "zeta".into(),
                package_dir: root.path().join("zeta"),
                dir: second,
            },
        ],
    );
    assert_eq!(found.diagnostics().len(), 1);
    let (tools, _) = found.into_parts();
    assert_eq!(tools[0].contributed_by.as_deref(), Some("alpha"));
}

/// A manifest cannot claim a provenance it does not have: `contributed_by`
/// comes from the directory discovery read, and `parse_manifest` — the only
/// path a manifest's own bytes take — always leaves it `None`.
#[test]
fn a_manifest_cannot_claim_to_have_been_shipped_by_a_plugin() {
    let tool = parse_manifest(
        "name = \"xx\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]\ncontributed_by = \"vera\"\n",
        Path::new("/x/xx.toml"),
    )
    .expect("unknown fields are ignored, as they always have been");
    assert_eq!(tool.contributed_by, None);
}

/// **Witness for #3579: a package's tool can run a script the package ships.**
///
/// The child's working directory stays the workspace root — a linter a plugin
/// contributes acts on the *user's* repository — so a package names its own
/// files with `${plugin_dir}` and discovery resolves it against the installed
/// package. Without that expansion `command[0]` resolved inside the user's
/// repo, where the script is not, and the only shapes that could ever run were
/// a program already on `PATH` or an absolute path a package cannot know when
/// it is written.
///
/// The user's own manifest is the other half: `${plugin_dir}` names nothing
/// under `.stella/tools/`, so it must survive verbatim rather than expand to
/// the empty string and become a different path.
#[cfg(unix)]
#[tokio::test]
async fn a_contributed_tool_runs_a_script_its_own_package_ships() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins").join("vera");
    let script = package.join("scripts").join("x.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "#!/bin/sh\necho shipped_script_ran\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    // One manifest text, written into both scopes — so the only difference
    // between the two tools below is the directory each was read from.
    let manifest = |name: &str| {
        format!(
            "name = \"{name}\"\n\
             description = \"d\"\n\
             command = [\"${{plugin_dir}}/scripts/x.sh\"]\n\
             \n\
             [env]\n\
             PKG_DATA = \"${{plugin_dir}}/data\"\n"
        )
    };
    let contributed = package.join("tools");
    std::fs::create_dir_all(&contributed).unwrap();
    std::fs::write(contributed.join("theirs.toml"), manifest("theirs")).unwrap();
    let own = root.path().join(".stella").join("tools");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("mine.toml"), manifest("mine")).unwrap();

    let (tools, _) = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            package_dir: package.clone(),
            dir: contributed,
        }],
    )
    .into_parts();

    let mine = tools.iter().find(|t| t.name == "mine").expect("mine");
    assert_eq!(
        mine.command[0], "${plugin_dir}/scripts/x.sh",
        "no package is in scope for a manifest the user wrote"
    );
    assert_eq!(mine.env["PKG_DATA"], "${plugin_dir}/data");

    let theirs = tools.iter().find(|t| t.name == "theirs").expect("theirs");
    assert_eq!(theirs.command[0], script.to_string_lossy());
    assert_eq!(
        theirs.env["PKG_DATA"],
        package.join("data").to_string_lossy()
    );

    // Run it from the workspace root, which is where it does NOT live.
    match run_custom(theirs, &serde_json::json!({}), root.path()).await {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("shipped_script_ran"), "{content}");
        }
        ToolOutput::Error { message, .. } => {
            panic!("a package's own script must run: {message}")
        }
    }
}

// the shared dispatch gate (#2793)

/// A registry whose `tool.call.requested` chain answers `decision` for
/// `gated_name` and allows everything else, plus a script tool of that name
/// that touches `ran.marker` when it runs. The marker is the assertion that
/// matters: a refusal has to stop the *process*, not merely relabel its
/// output.
fn gated_fixture(
    dir: &Path,
    gated_name: &str,
    decision: stella_core::bus::HookDecision,
) -> (crate::registry::ToolRegistry, CustomTool) {
    use stella_core::bus::{HookBus, names as hook_names};

    let mut tool = script_tool(dir, "s.sh", "#!/bin/sh\ntouch ./ran.marker\necho ran\n");
    tool.name = gated_name.to_string();

    let registry = crate::registry::ToolRegistry::new(dir.to_path_buf());
    let bus = HookBus::new("custom-gate-test");
    let gated_name = gated_name.to_string();
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, move |event| {
        if event.payload["tool"] == gated_name.as_str() {
            decision.clone()
        } else {
            stella_core::bus::HookDecision::Allow
        }
    })
    .detach();
    registry.attach_bus(bus);
    (registry, tool)
}

fn script_ran(dir: &Path) -> bool {
    dir.join("ran.marker").exists()
}

/// **The #2793 witness (custom half).** A custom tool is dispatched by
/// [`CustomToolSet`] itself and never reaches the registry, so before the
/// shared gate the registry's `tool.call.requested` chain never saw it: an
/// extension policy could deny a built-in and be silently ignored for a
/// `.stella/tools/*.toml` script with the same effect.
#[tokio::test]
async fn a_policy_deny_stops_a_custom_tool_before_its_script_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, tool) = gated_fixture(
        dir.path(),
        "my_tool",
        stella_core::bus::HookDecision::Deny("custom tools are off here".into()),
    );
    let set = CustomToolSet::new(&registry, vec![tool], dir.path().to_path_buf());

    match set.execute("my_tool", &serde_json::json!({})).await {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("custom tools are off here"), "{message}");
        }
        ToolOutput::Ok { content, .. } => panic!("a denied custom tool must not run: {content}"),
    }
    assert!(
        !script_ran(dir.path()),
        "the refusal must stop the process, not just relabel its output"
    );
}

/// The approval half of the same seam (#2676): a `RequireApproval` on a
/// custom tool reaches the session's responder — the asymmetry that made the
/// gap user-visible was a gated built-in asking a human while a script tool
/// with the same effect did not.
#[tokio::test]
async fn a_custom_tool_needing_approval_asks_the_sessions_responder() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Answering {
        answer: crate::registry::approval::ApprovalResponse,
        asked: AtomicUsize,
    }

    #[async_trait]
    impl crate::registry::approval::ApprovalResponder for Answering {
        async fn respond(
            &self,
            request: &crate::registry::approval::ApprovalRequest,
        ) -> crate::registry::approval::ApprovalResponse {
            assert_eq!(
                request.parked.tool(),
                Some("my_tool"),
                "the card names the custom tool"
            );
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    for (answer, expect_ran) in [
        (
            crate::registry::approval::ApprovalResponse::Deny {
                reason: "not this one".into(),
            },
            false,
        ),
        (crate::registry::approval::ApprovalResponse::Approve, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (registry, tool) = gated_fixture(
            dir.path(),
            "my_tool",
            stella_core::bus::HookDecision::RequireApproval {
                reason: "scripts need a human".into(),
            },
        );
        let responder = std::sync::Arc::new(Answering {
            answer,
            asked: AtomicUsize::new(0),
        });
        registry.attach_approval_responder(responder.clone(), Duration::from_secs(5));
        let set = CustomToolSet::new(&registry, vec![tool], dir.path().to_path_buf());

        let out = set.execute("my_tool", &serde_json::json!({})).await;
        assert_eq!(
            responder.asked.load(Ordering::SeqCst),
            1,
            "the human is asked exactly once per call"
        );
        assert_eq!(
            script_ran(dir.path()),
            expect_ran,
            "the human's answer decides whether the script runs: {out:?}"
        );
    }
}

/// A `modify` decision rewrites the input a custom tool actually runs on,
/// exactly as it does for a built-in — the gate returns the amended input,
/// and the dispatch must use it rather than the original.
#[tokio::test]
async fn a_modify_decision_rewrites_the_input_a_custom_tool_receives() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, _) = gated_fixture(
        dir.path(),
        "my_tool",
        stella_core::bus::HookDecision::Modify {
            payload: serde_json::json!({
                "tool": "my_tool",
                "input": { "path": "rewritten" },
            }),
        },
    );
    // Echo the scalar the harness exports, so the amended input is visible.
    let mut tool = script_tool(
        dir.path(),
        "s.sh",
        "#!/bin/sh\necho \"saw=$STELLA_INPUT_PATH\"\n",
    );
    tool.name = "my_tool".into();
    let set = CustomToolSet::new(&registry, vec![tool], dir.path().to_path_buf());

    match set
        .execute("my_tool", &serde_json::json!({ "path": "original" }))
        .await
    {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("saw=rewritten"), "{content}");
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

/// The other side of the contract: a name this set does NOT own falls
/// through ungated *here*, because the inner executor gates it itself. Gating
/// in both places would fire the chain twice for one call — and, once a
/// `RequireApproval` is in play, ask a human twice.
#[tokio::test]
async fn a_name_that_falls_through_is_gated_exactly_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use stella_core::bus::{HookBus, HookDecision, names as hook_names};

    let dir = tempfile::tempdir().unwrap();
    let registry = crate::registry::ToolRegistry::new(dir.path().to_path_buf());
    let bus = HookBus::new("once-test");
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        HookDecision::Allow
    })
    .detach();
    registry.attach_bus(bus);

    let tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho hi\n");
    let set = CustomToolSet::new(&registry, vec![tool], dir.path().to_path_buf());

    let out = set.execute("task_list", &serde_json::json!({})).await;
    assert!(!out.is_error(), "{out:?}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "a fall-through must not run the chain twice"
    );
}
