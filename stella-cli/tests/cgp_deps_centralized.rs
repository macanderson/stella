//! Regression witness for #878 — the `contextgraph-*` version is declared
//! exactly once, in the root `[workspace.dependencies]`.
//!
//! #878 (residual of #819) flagged the failure mode: `contextgraph-types`,
//! `contextgraph-host`, `contextgraph-trace`, and `contextgraph-conformance`
//! were git dependencies pinned by commit rev, and each of `stella-cli`,
//! `stella-context`, and `stella-graph` carried its OWN copy of the rev. A
//! partial bump left the crates at different commits — a silent
//! desynchronization no compiler or test could see, because each manifest
//! parsed fine on its own.
//!
//! The fix (#1156) moved the four crates to crates.io and declared them ONCE
//! in the root manifest's `[workspace.dependencies]`; member crates opt in
//! with `.workspace = true` and carry no version of their own. That makes a
//! bump a one-line edit and `cargo update -p contextgraph-types` a no-edit
//! operation — but nothing stops a future PR from re-introducing a
//! per-member `version = ...` or `git = ...` pin and re-creating the drift.
//! This test is that stop: it parses the real manifests and fails the moment
//! any member manifest repeats the version, or the root declaration goes
//! missing.
//!
//! It deliberately does NOT hard-code the current version string — the test's
//! job is to outlive bumps, not to be one more place a bump must touch.

use std::path::{Path, PathBuf};

/// The four Context Graph Protocol crates #878 is about.
const CGP_CRATES: [&str; 4] = [
    "contextgraph-types",
    "contextgraph-host",
    "contextgraph-trace",
    "contextgraph-conformance",
];

/// The workspace root: two levels up from this crate's manifest dir
/// (`stella-cli/`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stella-cli has a parent")
        .to_path_buf()
}

fn read_manifest(path: &Path) -> toml_edit::DocumentMut {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.parse::<toml_edit::DocumentMut>()
        .unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

/// Every `[dependencies]`/`[dev-dependencies]`/`[build-dependencies]` table in
/// a manifest, plus `[workspace.dependencies]` when the root is under test.
fn dependency_tables(doc: &toml_edit::DocumentMut) -> Vec<(&str, &toml_edit::Table)> {
    let mut tables = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|item| item.as_table()) {
            tables.push((section, table));
        }
    }
    if let Some(table) = doc
        .get("workspace")
        .and_then(|ws| ws.get("dependencies"))
        .and_then(|item| item.as_table())
    {
        tables.push(("workspace.dependencies", table));
    }
    tables
}

/// The root manifest declares all four CGP crates in
/// `[workspace.dependencies]` — the single source of truth a bump edits.
#[test]
fn cgp_crates_declared_once_in_workspace_root() {
    let root = read_manifest(&workspace_root().join("Cargo.toml"));
    let ws_deps = root
        .get("workspace")
        .and_then(|ws| ws.get("dependencies"))
        .and_then(|item| item.as_table())
        .expect("root Cargo.toml has a [workspace.dependencies] table");

    for krate in CGP_CRATES {
        let entry = ws_deps.get(krate).unwrap_or_else(|| {
            panic!(
                "{krate} is missing from the root [workspace.dependencies] — \
                 the single source of truth #878 requires is gone"
            )
        });
        // A bare string ("=0.1.2") or an inline table with a `version` key
        // are both registry declarations. What must NOT reappear is a git
        // source — that is the #819 supply-chain shape #878 was mitigating.
        let as_str = entry.as_str();
        let git = entry.get("git").and_then(|g| g.as_str());
        assert!(
            as_str.is_some() || entry.get("version").is_some(),
            "{krate} in the root [workspace.dependencies] must be a registry \
             version (string or `version` key), found: {entry}"
        );
        assert!(
            git.is_none(),
            "{krate} in the root [workspace.dependencies] regressed to a git \
             dependency ({}) — the rev-pin drift #878 documented is back",
            git.unwrap_or("<unparseable>")
        );
    }
}

/// No member manifest repeats a CGP version or git source — members opt in
/// with `.workspace = true` so the root declaration is the only one.
#[test]
fn no_member_manifest_repeats_the_cgp_version() {
    let root = workspace_root();
    let root_manifest = read_manifest(&root.join("Cargo.toml"));
    let members = root_manifest
        .get("workspace")
        .and_then(|ws| ws.get("members"))
        .and_then(|m| m.as_array())
        .expect("root Cargo.toml has workspace.members");

    let mut offenders = Vec::new();
    for member in members {
        let member = member.as_str().expect("member path is a string");
        let manifest_path = root.join(member).join("Cargo.toml");
        if !manifest_path.exists() {
            continue; // glob members (bench/*) resolve per-crate; skip misses
        }
        let doc = read_manifest(&manifest_path);
        for (section, table) in dependency_tables(&doc) {
            for krate in CGP_CRATES {
                let Some(entry) = table.get(krate) else {
                    continue;
                };
                // The one legal shape: `contextgraph-*.workspace = true` (or
                // `{ workspace = true, ... }`). Anything carrying its own
                // `version` or `git` re-creates the multi-manifest drift.
                let is_workspace_inherit = entry
                    .get("workspace")
                    .and_then(|w| w.as_bool())
                    .unwrap_or(false);
                if !is_workspace_inherit {
                    offenders.push(format!(
                        "{member}/Cargo.toml [{section}] declares {krate} as \
                         `{entry}` instead of `workspace = true`"
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "member manifests re-declare CGP crates, re-creating the #878 drift:\n  {}",
        offenders.join("\n  ")
    );
}

/// The lockfile resolves every CGP crate from the crates.io registry — never
/// from a git source — so `cargo update -p contextgraph-types` stays a
/// no-manifest-edit operation.
#[test]
fn lockfile_sources_cgp_crates_from_the_registry() {
    let lock = workspace_root().join("Cargo.lock");
    let doc = read_manifest(&lock);
    let packages = doc
        .get("package")
        .and_then(|p| p.as_array_of_tables())
        .expect("Cargo.lock has [[package]] entries");

    let mut seen = Vec::new();
    for package in packages {
        let name = package.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if !CGP_CRATES.contains(&name) {
            continue;
        }
        let source = package.get("source").and_then(|s| s.as_str()).unwrap_or("");
        assert!(
            source.starts_with("registry+"),
            "{name} in Cargo.lock comes from `{source}`, not the crates.io \
             registry — a git rev pin (#819/#878) is back"
        );
        seen.push(name);
    }

    let mut missing: Vec<_> = CGP_CRATES
        .iter()
        .filter(|k| !seen.contains(k))
        .copied()
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "Cargo.lock is missing CGP crates the workspace declares: {missing:?} \
         — run `cargo check --workspace` and commit the lockfile"
    );
}
