//! Tool Foundry wiring — the I/O half of the capability-gap detector
//! (issue #830, first slice).
//!
//! The decision logic is a pure function in `stella-core`
//! ([`stella_core::detect_tool_gaps`]); this module is its adapter: it reads
//! the durable `bash` receipt log from the store, hands the commands to the
//! detector, and surfaces each newly-proposed tool to the user's `/inbox` as a
//! [`stella_store::Notification`]. Nothing is installed and no manifest is
//! written — the first slice *proposes only*, so a human decides whether a
//! suggestion ever becomes a tool. It runs best-effort at end of execution
//! (`agent::persistence::record_execution_end`); a failed pass never fails a
//! turn.

use std::collections::HashSet;
use std::path::Path;

use colored::Colorize;
use stella_core::{GapDetectionConfig, ProposedTool, ShellInvocation, detect_tool_gaps};
use stella_store::{Notification, NotificationStore, Store, ToolCallRow};
use stella_tools::foundry_author::{self, PROPOSED_DIR};

/// How many recent `bash` receipts to mine per pass. A few hundred is plenty:
/// the detector only needs enough history to see a shape recur, and the read
/// is index-backed and cheap.
const HISTORY_WINDOW: usize = 200;

/// The `source` stamped on every foundry inbox item (shown dimmed in the
/// inbox) — also the human-readable marker for where a proposal came from.
const SOURCE: &str = "tool-foundry";

/// Mine recent `bash` history, detect capability gaps, and push a deduped
/// `/inbox` notification for each newly-proposed tool. Best-effort and
/// side-effect-only: returns the number of *new* proposals surfaced, and never
/// propagates an error.
pub(crate) fn surface_tool_gaps(store: &Store) -> usize {
    surface_tool_gaps_into(
        store,
        &NotificationStore::open_default(),
        GapDetectionConfig::default(),
    )
}

/// The testable core of [`surface_tool_gaps`], with the inbox and detector
/// config injected so a test can point at a temp directory and an in-memory
/// store.
pub(crate) fn surface_tool_gaps_into(
    store: &Store,
    inbox: &NotificationStore,
    config: GapDetectionConfig,
) -> usize {
    let Ok(rows) = store.recent_tool_calls("bash", HISTORY_WINDOW) else {
        return 0;
    };
    let invocations: Vec<ShellInvocation> =
        rows.into_iter().filter_map(invocation_from_row).collect();
    let proposals = detect_tool_gaps(&invocations, config);
    if proposals.is_empty() {
        return 0;
    }

    // Dedup against everything already in the inbox (read or unread) so the
    // same gap is surfaced at most once, however many turns keep the shape in
    // the mining window.
    let existing: HashSet<String> = inbox.list().into_iter().map(|n| n.id).collect();
    let mut pushed = 0;
    for proposal in &proposals {
        let id = notification_id(&proposal.signature);
        if existing.contains(&id) {
            continue;
        }
        let mut note = Notification::new(
            format!("Proposed tool: `{}`", proposal.name),
            proposal_body(proposal),
            SOURCE,
        );
        // A deterministic id from the shape is what makes dedup survive across
        // sessions — `Notification::new` would otherwise mint a fresh id each
        // pass and re-surface the same proposal forever.
        note.id = id;
        if inbox.push(&note).is_ok() {
            pushed += 1;
        }
    }
    pushed
}

/// Parse a `bash` `tool_calls` row into a [`ShellInvocation`]: the `command`
/// field of `args_json`, plus whether the call succeeded. Rows without a
/// string `command` (a malformed payload, or a non-bash row that somehow
/// slipped the name filter) are dropped rather than guessed at.
fn invocation_from_row(row: ToolCallRow) -> Option<ShellInvocation> {
    let value: serde_json::Value = serde_json::from_str(&row.args_json).ok()?;
    let command = value.get("command")?.as_str()?.to_string();
    if command.trim().is_empty() {
        return None;
    }
    Some(ShellInvocation {
        command,
        succeeded: row.ok(),
    })
}

/// A stable, filesystem-safe notification id derived from the proposal
/// signature, so one gap maps to exactly one inbox item forever.
fn notification_id(signature: &str) -> String {
    format!("ntf-toolfoundry-{:016x}", fnv1a(signature))
}

/// A tiny FNV-1a hash — enough to key dedup deterministically, and it keeps
/// this module free of a hashing dependency.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The inbox body: what the gap is, the proposed shape, the parameters with a
/// sample value each, the commands actually seen, and the guardrail that
/// nothing was installed.
fn proposal_body(p: &ProposedTool) -> String {
    let params = if p.parameters.is_empty() {
        "  (none)".to_string()
    } else {
        p.parameters
            .iter()
            .map(|param| {
                let sample = param
                    .examples
                    .first()
                    .map(|e| format!(" (e.g. {e})"))
                    .unwrap_or_default();
                format!("  {} : {}{}", param.name, param.kind.placeholder(), sample)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let examples = p
        .examples
        .iter()
        .map(|e| format!("  $ {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{desc}\n\nProposed command:\n  {template}\n\nParameters:\n{params}\n\nSeen:\n{examples}\n\n\
         This is a suggestion only — no tool was installed. Stage it for review with \
         `stella tools --author {name}`.",
        desc = p.description,
        template = p.command_template,
        name = p.name,
    )
}

/// `stella tools --author [NAME]` — the authorship slice of #830: turn one
/// mined proposal into a staged, reviewable custom tool, or (with no name)
/// list the proposals the detector currently mines from recent `bash`
/// receipts. Authoring installs nothing: the manifest+script pair lands in
/// `.stella/tools/proposed/`, a directory discovery's non-recursive scan can
/// never register from, and enabling remains an explicit human move of the
/// manifest into `.stella/tools/`.
pub(crate) fn run_tools_author(name: Option<&str>) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    run_tools_author_in(&root, &store, name, GapDetectionConfig::default())
}

/// The testable core of [`run_tools_author`]: store and workspace root
/// injected, so a test can drive it with an in-memory store and a tempdir.
fn run_tools_author_in(
    root: &Path,
    store: &Store,
    name: Option<&str>,
    config: GapDetectionConfig,
) -> Result<(), String> {
    let rows = store
        .recent_tool_calls("bash", HISTORY_WINDOW)
        .map_err(|e| format!("cannot read the bash receipt log: {e}"))?;
    let invocations: Vec<ShellInvocation> =
        rows.into_iter().filter_map(invocation_from_row).collect();
    let proposals = detect_tool_gaps(&invocations, config);

    let Some(name) = name else {
        crate::tui::section_header("Tool-foundry proposals");
        if proposals.is_empty() {
            println!(
                "  {}",
                format!(
                    "none right now — a shape must recur ≥{} times across ≥{} distinct \
                     argument sets in the last {HISTORY_WINDOW} bash receipts",
                    config.min_occurrences, config.min_distinct_arguments
                )
                .dimmed()
            );
            return Ok(());
        }
        for p in &proposals {
            println!(
                "  {} {} {}",
                "·".dimmed(),
                p.name.bright_magenta(),
                format!(
                    "— `{}` ({}× across {} argument sets)",
                    p.signature, p.occurrences, p.distinct_arguments
                )
                .dimmed()
            );
        }
        println!(
            "\n  {}",
            "stage one for review with `stella tools --author <name>`".dimmed()
        );
        return Ok(());
    };

    let Some(proposal) = proposals.iter().find(|p| p.name == name) else {
        let available: Vec<&str> = proposals.iter().map(|p| p.name.as_str()).collect();
        return Err(if available.is_empty() {
            format!(
                "no proposal named `{name}` — the detector currently proposes nothing \
                 (run `stella tools --author` to see why)"
            )
        } else {
            format!(
                "no proposal named `{name}` — current proposals: {}",
                available.join(", ")
            )
        });
    };

    let authored = foundry_author::author(proposal)?;
    let staged_dir = root.join(PROPOSED_DIR);
    std::fs::create_dir_all(&staged_dir)
        .map_err(|e| format!("cannot create {}: {e}", staged_dir.display()))?;
    let manifest_path = staged_dir.join(&authored.manifest_filename);
    let script_path = staged_dir.join(&authored.script_filename);
    // Never clobber: a staged pair may carry a reviewer's hand edits, and
    // re-running the detector regenerates a fresh one anyway.
    for path in [&manifest_path, &script_path] {
        if path.exists() {
            return Err(format!(
                "`{}` is already staged — review it (or remove it) and re-run",
                path.display()
            ));
        }
    }
    std::fs::write(&script_path, &authored.script)
        .map_err(|e| format!("cannot write {}: {e}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", script_path.display()))?;
    }
    std::fs::write(&manifest_path, &authored.manifest_toml)
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    crate::tui::section_header("Tool staged — not installed");
    println!(
        "  {} {}\n  {} {}",
        "·".green(),
        manifest_path.display(),
        "·".green(),
        script_path.display()
    );
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let live = stella_tools::custom::discover_in(root, home.as_deref());
    if live.tools.iter().any(|t| t.name == authored.name) {
        println!(
            "  {} {}",
            "!".yellow(),
            format!(
                "a custom tool named `{}` already exists — adopting this one will be \
                 ignored as a duplicate until that is resolved",
                authored.name
            )
            .yellow()
        );
    }
    println!(
        "\n  {}",
        format!(
            "staged only — discovery cannot see {PROPOSED_DIR}/, so nothing is \
             registered or executable until a human enables it:"
        )
        .dimmed()
    );
    println!(
        "  {}\n  {}",
        format!("pre-flight:  stella tools --validate {PROPOSED_DIR}").dimmed(),
        format!(
            "enable:      mv {PROPOSED_DIR}/{} .stella/tools/",
            authored.manifest_filename
        )
        .dimmed()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_store::ToolCallState;

    fn bash_row(call_id: &str, command: &str, ok: bool) -> ToolCallRow {
        ToolCallRow {
            call_id: call_id.into(),
            name: "bash".into(),
            surface: "native".into(),
            args_json: serde_json::json!({ "command": command }).to_string(),
            args_digest: call_id.into(),
            reason: String::new(),
            state: if ok {
                ToolCallState::Ok
            } else {
                ToolCallState::Error
            },
            error: String::new(),
            bytes_out: 0,
            duration_ms: 1,
        }
    }

    /// A store seeded with the given bash commands under one execution.
    fn store_with(commands: &[(&str, &str, bool)]) -> Store {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "p", "zai", "glm-5.2")
            .expect("execution");
        let rows: Vec<ToolCallRow> = commands
            .iter()
            .map(|(cid, cmd, ok)| bash_row(cid, cmd, *ok))
            .collect();
        store.record_tool_calls(id, &rows).expect("record");
        store
    }

    #[test]
    fn a_recurring_shape_is_surfaced_once_and_deduped() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = NotificationStore::open(dir.path());

        let pushed = surface_tool_gaps_into(&store, &inbox, GapDetectionConfig::default());
        assert_eq!(pushed, 1, "one recurring shape → one proposal");

        let items = inbox.list();
        assert_eq!(items.len(), 1);
        let note = &items[0];
        assert_eq!(note.source, SOURCE);
        assert!(note.id.starts_with("ntf-toolfoundry-"));
        assert!(note.title.contains("Proposed tool"));
        assert!(
            note.body.contains("no tool was installed"),
            "body must carry the not-installed guardrail"
        );
        assert!(
            note.body.contains("jq {p1} {p2}"),
            "body shows the template"
        );

        // A second pass over the same history surfaces nothing new.
        let again = surface_tool_gaps_into(&store, &inbox, GapDetectionConfig::default());
        assert_eq!(again, 0, "dedup: same gap is not re-surfaced");
        assert_eq!(inbox.list().len(), 1);
    }

    #[test]
    fn no_pattern_surfaces_nothing() {
        // One-off commands: nothing recurs, so the inbox stays empty.
        let store = store_with(&[
            ("c1", "ls -la", true),
            ("c2", "pwd", true),
            ("c3", "whoami", true),
        ]);
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = NotificationStore::open(dir.path());
        assert_eq!(
            surface_tool_gaps_into(&store, &inbox, GapDetectionConfig::default()),
            0
        );
        assert!(inbox.list().is_empty());
    }

    #[test]
    fn malformed_and_empty_rows_are_ignored() {
        let store = Store::in_memory().expect("store");
        let id = store.begin_execution("run", "p", "z", "m").expect("exec");
        store
            .record_tool_calls(
                id,
                &[
                    ToolCallRow {
                        call_id: "c1".into(),
                        name: "bash".into(),
                        surface: "native".into(),
                        args_json: "not json".into(),
                        args_digest: "c1".into(),
                        reason: String::new(),
                        state: ToolCallState::Ok,
                        error: String::new(),
                        bytes_out: 0,
                        duration_ms: 1,
                    },
                    bash_row("c2", "   ", true),
                ],
            )
            .expect("record");
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = NotificationStore::open(dir.path());
        // Must not panic and must surface nothing.
        assert_eq!(
            surface_tool_gaps_into(&store, &inbox, GapDetectionConfig::default()),
            0
        );
    }

    #[test]
    fn author_stages_a_reviewable_pair_that_discovery_cannot_see() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let ws = tempfile::tempdir().expect("tmp");

        run_tools_author_in(ws.path(), &store, Some("jq"), GapDetectionConfig::default())
            .expect("authoring succeeds");

        let manifest_path = ws.path().join(PROPOSED_DIR).join("jq.toml");
        let script_path = ws.path().join(PROPOSED_DIR).join("jq.sh");
        assert!(manifest_path.exists(), "manifest staged");
        assert!(script_path.exists(), "script staged");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "script is executable");
        }

        // The staged manifest is real: it parses with the discovery parser.
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let parsed = stella_tools::custom::parse_manifest(&text, &manifest_path)
            .expect("staged manifest parses");
        assert_eq!(parsed.name, "jq");

        // The guardrail: discovery over this workspace registers nothing.
        let report = stella_tools::custom::discover_in(ws.path(), None);
        assert!(report.tools.is_empty(), "staged tool must not register");
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn author_refuses_to_clobber_a_staged_pair() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let ws = tempfile::tempdir().expect("tmp");
        run_tools_author_in(ws.path(), &store, Some("jq"), GapDetectionConfig::default())
            .expect("first authoring succeeds");

        // A reviewer may have hand-edited the staged pair; re-authoring must
        // fail closed instead of silently regenerating over it.
        let err = run_tools_author_in(ws.path(), &store, Some("jq"), GapDetectionConfig::default())
            .expect_err("second authoring refuses");
        assert!(err.contains("already staged"), "{err}");
    }

    #[test]
    fn author_with_unknown_name_lists_what_is_available() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let ws = tempfile::tempdir().expect("tmp");
        let err = run_tools_author_in(
            ws.path(),
            &store,
            Some("not_a_proposal"),
            GapDetectionConfig::default(),
        )
        .expect_err("unknown name errors");
        assert!(err.contains("jq"), "error names the real proposals: {err}");
        assert!(
            !ws.path().join(PROPOSED_DIR).exists(),
            "nothing staged on error"
        );
    }

    #[test]
    fn author_listing_mode_writes_nothing() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let ws = tempfile::tempdir().expect("tmp");
        run_tools_author_in(ws.path(), &store, None, GapDetectionConfig::default())
            .expect("listing succeeds");
        assert!(!ws.path().join(".stella").exists(), "listing is read-only");
    }

    #[test]
    fn inbox_proposals_point_at_the_author_command() {
        let store = store_with(&[
            ("c1", "jq '.a' a.json", true),
            ("c2", "jq '.b' b.json", true),
            ("c3", "jq '.c' c.json", true),
        ]);
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = NotificationStore::open(dir.path());
        surface_tool_gaps_into(&store, &inbox, GapDetectionConfig::default());
        let items = inbox.list();
        assert_eq!(items.len(), 1);
        assert!(
            items[0].body.contains("stella tools --author jq"),
            "body must name the concrete next step: {}",
            items[0].body
        );
    }

    #[test]
    fn dedup_id_is_stable_for_a_signature() {
        assert_eq!(
            notification_id("jq <str> <path>"),
            notification_id("jq <str> <path>")
        );
        assert_ne!(
            notification_id("jq <str> <path>"),
            notification_id("sed <str> <path>")
        );
    }
}
