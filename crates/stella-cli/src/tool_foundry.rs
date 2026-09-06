//! Tool Foundry wiring — the CLI half of the self-authored-tool protocol.
//!
//! A proposal is a manifest+script pair staged under `.stella/tools/proposed/`,
//! a directory discovery's non-recursive scan can never register from. The
//! manual verbs live in [`adopt`] (`--adopt` proves a staged tool with its
//! capability witness, `--enable` grants, `--foundry` reports) and [`ops`]
//! (`--draft` authors from the gap ledger, `--rollback` restores a recorded
//! version, `--status` shows per-tool health).
//!
//! The live loop runs on the end-of-turn seam beside skill
//! mining: [`end_of_turn`] mines the store's recent shell history for gaps
//! ([`gaps`]), ledgers the novel ones, and — under `foundry.autonomy =
//! "auto"` — carries them through author → validate → witness-adopt → enable
//! ([`autonomy`]), with network denial, telemetry, the circuit breaker, and
//! versioned rollback standing in place of the retired human ceremony.

pub(crate) mod adopt;
pub(crate) mod author;
pub(crate) mod autonomy;
// The pure detector [`gaps`] feeds: shell shapes in, proposals out, no I/O.
pub(crate) mod detect;
pub(crate) mod gaps;
pub(crate) mod ops;

use std::path::Path;

/// The end-of-turn hook: detect gaps from the store's recent shell
/// history, ledger the novel ones, and run whatever autonomy the workspace's
/// `[foundry]` settings allow. Returns user-visible notices; never fails the
/// turn it rides on.
pub(crate) async fn end_of_turn(root: &Path, store: Option<&stella_store::Store>) -> Vec<String> {
    let config = match crate::settings::Settings::load(root) {
        Ok(settings) => match settings.foundry_config() {
            Ok(config) => config,
            // A threshold the module refuses is a mistake someone has to
            // see, and a config that cannot be trusted runs nothing
            // autonomous — fail closed, loudly.
            Err(diagnostic) => {
                return vec![format!(
                    "{diagnostic} — tool-gap detection skipped this turn"
                )];
            }
        },
        Err(_) => crate::settings::FoundryConfig::default(),
    };
    end_of_turn_with(root, store, &config).await
}

/// [`end_of_turn`] with the config already resolved — the seam the witness
/// tests drive, so they exercise the live hook path without depending on
/// the test machine's own settings scopes.
pub(crate) async fn end_of_turn_with(
    root: &Path,
    store: Option<&stella_store::Store>,
    config: &crate::settings::FoundryConfig,
) -> Vec<String> {
    let (new_gaps, notice) = gaps::scan_and_ledger(root, store, config.detection);
    let mut notices: Vec<String> = notice.into_iter().collect();
    if !new_gaps.is_empty() {
        notices.extend(autonomy::run_autonomy(root, store, &new_gaps, config).await);
    }
    notices
}

/// Stamp the operator's `[foundry]` runtime policy onto every discovered
/// foundry-authored tool: whether it is on the network allowlist, and the
/// breaker thresholds its launches are held to. Hand-written tools are left
/// untouched. Fail-closed: an unreadable settings chain means the defaults —
/// empty allowlist, shipped breaker.
pub(crate) fn apply_foundry_runtime(tools: &mut [stella_tools::custom::CustomTool], root: &Path) {
    let config = crate::settings::Settings::load(root)
        .ok()
        .and_then(|settings| settings.foundry_config().ok())
        .unwrap_or_default();
    for tool in tools.iter_mut() {
        if tool
            .foundry
            .as_ref()
            .is_some_and(|p| p.is_foundry_authored())
        {
            tool.foundry_runtime = stella_tools::custom::FoundryRuntimePolicy {
                network_allowed: config.network_allowlist.iter().any(|n| n == &tool.name),
                breaker: Some(config.breaker),
            };
        }
    }
}

/// A tiny FNV-1a hash — enough to key dedup deterministically, and it keeps
/// this module free of a hashing dependency. `pub(crate)` because the ingest
/// staleness alerts (`ingest_cmd::lineage`) derive their notification ids
/// this way, and two copies of a hash function is how they drift apart.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::ToolExecutor;
    use stella_protocol::tool::ToolOutput;

    /// The hash keys durable notification ids (`ingest_cmd::lineage`), so its
    /// output for a given input is a compatibility surface: a changed value
    /// re-surfaces every already-delivered notification.
    #[test]
    fn fnv1a_is_stable_for_a_signature() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1} {p2}"));
        assert_ne!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1}"));
    }

    /// A staged `[foundry]` manifest naming a network-reachability probe.
    /// `witness_input` needs at least one value — an empty table is a
    /// declared-vacuous witness (`VacuousWitness::NoWitnessInput`) — so it
    /// carries an unused `p1`, ignored by the script, the same shape
    /// `author_cat` uses in `adopt/tests.rs`. Since
    /// `adopt::Builtins::execute` always errors, any candidate that runs at
    /// all proves a capability flip.
    fn author_netcheck(root: &Path, name: &str, body: &str) {
        let staged = root.join(stella_tools::foundry_gate::PROPOSED_DIR);
        std::fs::create_dir_all(&staged).expect("create proposed dir");
        let script_path = staged.join(format!("{name}.sh"));
        std::fs::write(&script_path, body).expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("mark executable");
        }
        std::fs::write(
            staged.join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\n\
                 description = \"Probe outbound network reachability.\"\n\
                 command = [\"./{dir}/{name}.sh\"]\n\
                 \n\
                 [foundry]\n\
                 authored_by = \"{authored_by}\"\n\
                 signature = \"netcheck\"\n\
                 occurrences = 3\n\
                 witness_input = {{ p1 = \"unused\" }}\n\
                 \n\
                 [input_schema]\n\
                 type = \"object\"\n\
                 [input_schema.properties.p1]\n\
                 type = \"string\"\n",
                dir = stella_tools::foundry_gate::PROPOSED_DIR,
                authored_by = stella_tools::foundry_gate::AUTHORED_BY,
            ),
        )
        .expect("write manifest");
    }

    /// `foundry.network_allowlist`'s **allow** direction, driven through
    /// the real chain a session actually runs — a project
    /// `.stella/settings.json`, real discovery
    /// (`stella_tools::custom::discover_in`), the adoption gate,
    /// [`apply_foundry_runtime`], and the real dispatch seam
    /// (`CustomToolSet::execute`). The **deny** direction already has this
    /// witness end to end
    /// (`a_governed_tool_cannot_reach_the_network_where_the_mechanism_is_live`
    /// in `stella-tools`'s `custom::tests::execution`, which hand-builds its
    /// `CustomTool` rather than discovering one) — nothing drove the allow
    /// side through a real settings file. On `main`, `apply_foundry_runtime`
    /// is exercised only by `fnv1a_is_stable_for_a_signature` above (which
    /// never calls it) — no test failed if `network_allowlist` were read
    /// under the wrong settings key, if the gate silently withheld an
    /// allowlisted tool, or if the spawn seam stopped reading
    /// `foundry_runtime` at all.
    #[tokio::test]
    async fn the_allowlist_reaches_from_settings_through_discovery_to_the_spawn() {
        // Skip with reason: nothing to observe without the OS-level
        // mechanism (the issue's own verify steps ask for exactly this).
        if !stella_tools::netdeny::available() {
            return;
        }
        // And nothing to conclude without real outbound reach on this
        // runner: watching for a connect to succeed cannot tell "allowed"
        // from "denied" if nothing can ever connect. Probed with a bare,
        // unwrapped process, independent of anything the foundry gate does,
        // so a network-isolated runner reports nothing rather than a false
        // reading in either direction.
        let probe = tokio::process::Command::new("bash")
            .args(["-c", "exec 3<>/dev/tcp/1.1.1.1/53 && echo REACHED"])
            .output()
            .await
            .expect("the bare probe itself must spawn");
        if !String::from_utf8_lossy(&probe.stdout).contains("REACHED") {
            return;
        }

        let ws = tempfile::tempdir().expect("tmp");
        let root = ws.path();
        let _home = crate::paths::test_user_home(root.to_path_buf());
        let store = stella_store::Store::open(root).expect("store");

        let body = "#!/bin/bash\nif exec 3<>/dev/tcp/1.1.1.1/53; then echo REACHED; \
                     else echo DENIED; fi\n";
        for name in ["allowed_net", "denied_net"] {
            author_netcheck(root, name, body);
            // The async form: this test already runs on a tokio runtime, and
            // `adopt_in` (the sync form) spins up its own to drive the
            // witness — nesting runtimes panics. Same steps, same gates.
            adopt::adopt_in_async(root, &store, name, "test")
                .await
                .unwrap_or_else(|e| panic!("adopt {name}: {e}"));
            adopt::set_enabled_in(
                root,
                &store,
                name,
                Some(stella_store::EnableAuthority::InteractiveHuman),
            )
            .unwrap_or_else(|e| panic!("enable {name}: {e}"));
        }

        std::fs::create_dir_all(root.join(".stella")).expect("create .stella");
        std::fs::write(
            root.join(".stella/settings.json"),
            r#"{"foundry": {"network_allowlist": ["allowed_net"]}}"#,
        )
        .expect("write settings");

        // Discovery, exactly as a real session runs it.
        let report = adopt::gate_discovery(stella_tools::custom::discover_in(root, None), root);
        let mut tools = report.tools;
        apply_foundry_runtime(&mut tools, root);

        let allowed = tools
            .iter()
            .find(|t| t.name == "allowed_net")
            .expect("the allowlisted tool must survive the gate");
        let denied = tools
            .iter()
            .find(|t| t.name == "denied_net")
            .expect("the unlisted tool must survive the gate too");
        assert!(
            allowed.foundry_runtime.network_allowed,
            "a listed name must be stamped allowed"
        );
        assert!(
            !denied.foundry_runtime.network_allowed,
            "an unlisted name must stay denied"
        );

        // The spawn, through the same seam the engine dispatches through.
        struct Empty;
        #[async_trait::async_trait]
        impl stella_core::ports::ToolExecutor for Empty {
            fn schemas(&self) -> Vec<stella_protocol::tool::ToolSchema> {
                Vec::new()
            }
            async fn execute(&self, name: &str, _input: &serde_json::Value) -> ToolOutput {
                ToolOutput::error(format!("no tool named `{name}`"))
            }
        }
        let set = stella_tools::custom::CustomToolSet::new_owned(
            std::sync::Arc::new(Empty),
            tools,
            root.to_path_buf(),
        );
        let rendered = |out: &ToolOutput| match out {
            ToolOutput::Ok { content, .. } => content.clone(),
            ToolOutput::Error { message, .. } => message.clone(),
        };
        let allowed_out = set.execute("allowed_net", &serde_json::json!({})).await;
        let denied_out = set.execute("denied_net", &serde_json::json!({})).await;
        assert!(
            rendered(&allowed_out).contains("REACHED"),
            "the allowlisted tool must spawn unwrapped and reach the network: {}",
            rendered(&allowed_out)
        );
        assert!(
            !rendered(&denied_out).contains("REACHED"),
            "the unlisted tool must still spawn under the netdeny wrapper: {}",
            rendered(&denied_out)
        );
    }
}
