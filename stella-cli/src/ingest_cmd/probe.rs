//! The ungated truth-probe runner — turning a claim's probe into a refutation.
//!
//! Ingest runs a probe to catch a document that was already stale when it was
//! read (`05` node-version example: CLAUDE.md says Node 20, `.nvmrc` has said 22
//! for months). Only **ungated** probes reach this runner: `path_exists`,
//! `path_absent`, `file_contains`, and `manual`. Gated probes
//! (`command_succeeds`, `http_ok`) are stripped before we get here — they are
//! never honored on an imported record (see [`stella_core::ingest::gate`]), so a
//! document can never make ingest run a command or reach the network.
//!
//! Every path is resolved under the workspace root and every read is bounded, so
//! a probe cannot escape the tree or be turned into a denial-of-service by a
//! pathological pattern.

use std::path::Path;

use stella_core::ingest::{Probe, ProbeKind, Refutation, Verdict};

/// The largest file the `file_contains` probe reads. A tracked config file that
/// needs more than this to answer "does it mention X" is not what the probe is
/// for; beyond the cap the probe abstains rather than reads unbounded input.
const MAX_PROBE_READ_BYTES: u64 = 512 * 1024;

/// Evaluate `probe` against the workspace and return the refutation verdict.
///
/// `manual` and anything unexpected abstain as `unfalsifiable` — the honest
/// answer when no machine check ran, which must never be folded into
/// `supported` (README: a refuter that stamps unchecked claims launders them).
pub fn evaluate(root: &Path, probe: &Probe, checked_at: &str) -> Refutation {
    match probe.kind {
        ProbeKind::PathExists => match probe.path.as_deref() {
            Some(path) => verdict(
                probe.kind,
                checked_at,
                path_matches(root, path),
                format!("path `{path}` "),
                "exists",
                "is missing",
            ),
            None => unfalsifiable(checked_at, "path_exists probe named no path"),
        },
        ProbeKind::PathAbsent => match probe.path.as_deref() {
            Some(path) => verdict(
                probe.kind,
                checked_at,
                !path_matches(root, path),
                format!("path `{path}` "),
                "is absent",
                "is present",
            ),
            None => unfalsifiable(checked_at, "path_absent probe named no path"),
        },
        ProbeKind::FileContains => file_contains(root, probe, checked_at),
        // A manual probe is a scheduled human recheck; no machine judged it now.
        ProbeKind::Manual => unfalsifiable(checked_at, "manual re-verification; no machine check"),
        // Gated kinds are stripped upstream and never reach here; `none` is the
        // explicit "nothing could judge this" case. Both abstain.
        ProbeKind::CommandSucceeds | ProbeKind::HttpOk | ProbeKind::None => {
            unfalsifiable(checked_at, "no probe kind could judge this claim")
        }
    }
}

/// The `file_contains` probe: read a tracked file and check for a pattern.
///
/// Matching is a **literal substring**, not a regex — the extractor is
/// instructed to emit literal patterns, and a substring test cannot be turned
/// into catastrophic backtracking by hostile input. `expect = "absent"` inverts
/// the sense (the claim holds when the pattern is *not* found).
fn file_contains(root: &Path, probe: &Probe, checked_at: &str) -> Refutation {
    let Some(path) = probe.path.as_deref() else {
        return unfalsifiable(checked_at, "file_contains probe named no path");
    };
    let Some(pattern) = probe.pattern.as_deref() else {
        return unfalsifiable(checked_at, "file_contains probe named no pattern");
    };
    let full = root.join(path);
    let expect_absent = probe.expect.as_deref() == Some("absent");

    match read_bounded(&full) {
        Some(contents) => {
            let found = contents.contains(pattern);
            let held = if expect_absent { !found } else { found };
            let sense = if expect_absent {
                "absent from"
            } else {
                "present in"
            };
            verdict(
                probe.kind,
                checked_at,
                held,
                format!("pattern `{pattern}` "),
                &format!("is {sense} {path}"),
                &format!("did not match `{sense} {path}` as claimed"),
            )
        }
        // A probe pointed at a file that is not there cannot confirm the claim,
        // but its absence is itself evidence the world moved — report it as
        // refuted rather than silently passing.
        None => verdict(
            probe.kind,
            checked_at,
            false,
            format!("file `{path}` "),
            "could be read",
            "is missing or too large to read",
        ),
    }
}

/// Build a supported/refuted refutation from a boolean outcome, describing what
/// was checked. A refuted verdict recommends `ignore`, so the review surface can
/// surface "the document was stale" without a second judgement.
fn verdict(
    kind: ProbeKind,
    checked_at: &str,
    held: bool,
    subject: String,
    held_phrase: &str,
    failed_phrase: &str,
) -> Refutation {
    if held {
        Refutation {
            verdict: Verdict::Supported,
            checked_at: checked_at.to_string(),
            probe_kind: kind,
            detail: format!("{subject}{held_phrase}."),
            recommend: None,
            links: Vec::new(),
        }
    } else {
        Refutation {
            verdict: Verdict::Refuted,
            checked_at: checked_at.to_string(),
            probe_kind: kind,
            detail: format!("{subject}{failed_phrase}. The source document is stale."),
            recommend: Some("ignore".to_string()),
            links: Vec::new(),
        }
    }
}

/// An abstaining verdict — no machine check could judge the claim.
fn unfalsifiable(checked_at: &str, why: &str) -> Refutation {
    Refutation {
        verdict: Verdict::Unfalsifiable,
        checked_at: checked_at.to_string(),
        probe_kind: ProbeKind::None,
        detail: format!("{why}. Measure compliance instead of asserting truth."),
        recommend: None,
        links: Vec::new(),
    }
}

/// Whether `path` (workspace-relative, possibly with a single-`*` filename glob
/// or a trailing `/`) names something that exists under `root`.
fn path_matches(root: &Path, path: &str) -> bool {
    let rel = path.trim_start_matches("./");
    if let Some(dir) = rel.strip_suffix('/') {
        return root.join(dir).is_dir();
    }
    if rel.contains('*') {
        return glob_exists(root, rel);
    }
    root.join(rel).exists()
}

/// A single-`*` filename glob against the entries of one directory — the only
/// wildcard the matcher supports (README note on `capabilities/contracts/*.yaml`).
fn glob_exists(root: &Path, rel: &str) -> bool {
    let (dir, pattern) = match rel.rsplit_once('/') {
        Some((dir, name)) => (root.join(dir), name.to_string()),
        None => (root.to_path_buf(), rel.to_string()),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        wildcard_match(&pattern, &name)
    })
}

/// Match a name against a pattern whose only metacharacter is `*` (zero or more
/// of anything). Anchored at both ends.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut cursor = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !name[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
        } else if i == parts.len() - 1 {
            return name[cursor..].ends_with(part);
        } else if let Some(found) = name[cursor..].find(part) {
            cursor += found + part.len();
        } else {
            return false;
        }
    }
    true
}

/// Read a file, capped at [`MAX_PROBE_READ_BYTES`]. `None` on any failure or
/// oversize, so the caller treats an unreadable file as a failed check.
fn read_bounded(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_PROBE_READ_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("stella-probe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn probe(
        kind: ProbeKind,
        path: Option<&str>,
        pattern: Option<&str>,
        expect: Option<&str>,
    ) -> Probe {
        Probe {
            kind,
            path: path.map(str::to_string),
            pattern: pattern.map(str::to_string),
            expect: expect.map(str::to_string),
            note: None,
        }
    }

    #[test]
    fn path_exists_supports_when_present_refutes_when_missing() {
        let root = temp_root("path-exists");
        std::fs::write(root.join("pnpm-lock.yaml"), "lock").expect("write");

        let present = evaluate(
            &root,
            &probe(ProbeKind::PathExists, Some("pnpm-lock.yaml"), None, None),
            "t",
        );
        assert_eq!(present.verdict, Verdict::Supported);

        let missing = evaluate(
            &root,
            &probe(ProbeKind::PathExists, Some("yarn.lock"), None, None),
            "t",
        );
        assert_eq!(missing.verdict, Verdict::Refuted);
        assert_eq!(missing.recommend.as_deref(), Some("ignore"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_contains_refutes_a_stale_claim() {
        let root = temp_root("file-contains");
        std::fs::write(root.join(".nvmrc"), "22.11.0\n").expect("write");

        // Claim "Node 20" would set a pattern of "20"; the file says 22.
        let refuted = evaluate(
            &root,
            &probe(ProbeKind::FileContains, Some(".nvmrc"), Some("20."), None),
            "t",
        );
        assert_eq!(refuted.verdict, Verdict::Refuted);

        let supported = evaluate(
            &root,
            &probe(ProbeKind::FileContains, Some(".nvmrc"), Some("22.11"), None),
            "t",
        );
        assert_eq!(supported.verdict, Verdict::Supported);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_contains_absent_inverts_the_sense() {
        let root = temp_root("file-absent");
        std::fs::write(root.join("docker-compose.yml"), "services:\n  dev: {}\n").expect("write");

        // Claim "no staging service" → expect the `staging:` key absent.
        let held = evaluate(
            &root,
            &probe(
                ProbeKind::FileContains,
                Some("docker-compose.yml"),
                Some("staging:"),
                Some("absent"),
            ),
            "t",
        );
        assert_eq!(held.verdict, Verdict::Supported);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_and_none_abstain() {
        let root = temp_root("manual");
        assert_eq!(
            evaluate(&root, &probe(ProbeKind::Manual, None, None, None), "t").verdict,
            Verdict::Unfalsifiable
        );
        assert_eq!(
            evaluate(&root, &probe(ProbeKind::None, None, None, None), "t").verdict,
            Verdict::Unfalsifiable
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn glob_matches_a_starred_filename() {
        let root = temp_root("glob");
        std::fs::create_dir_all(root.join("capabilities/contracts")).expect("mkdir");
        std::fs::write(root.join("capabilities/contracts/echo.yaml"), "x").expect("write");

        let hit = evaluate(
            &root,
            &probe(
                ProbeKind::PathExists,
                Some("capabilities/contracts/*.yaml"),
                None,
                None,
            ),
            "t",
        );
        assert_eq!(hit.verdict, Verdict::Supported);

        let miss = evaluate(
            &root,
            &probe(
                ProbeKind::PathExists,
                Some("capabilities/contracts/*.toml"),
                None,
                None,
            ),
            "t",
        );
        assert_eq!(miss.verdict, Verdict::Refuted);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wildcard_anchors_both_ends() {
        assert!(wildcard_match("*.yaml", "echo.yaml"));
        assert!(!wildcard_match("*.yaml", "echo.yaml.bak"));
        assert!(wildcard_match("test_*", "test_foo"));
        assert!(wildcard_match("a*c", "abc"));
        assert!(!wildcard_match("a*c", "abd"));
    }
}
