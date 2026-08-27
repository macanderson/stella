// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The learned-skill lifecycle — SPEC 9.2, the half of the SKILLS tab that
//! applies only to skills stella wrote itself: the provenance a mined skill
//! carries ([`learned_provenance`]), the [`rename`] that gives it a human name
//! without losing the identity it was mined under, and the [`reject`] that
//! deletes it and records the negative signal the miner reads
//! ([`rejections`]).
//!
//! `stella_core::skills::render_skill_markdown` is byte-pinned, so the mined
//! identity and the turn cannot become frontmatter; they go where the parent's
//! header says state outside the `SKILL.md` goes — the per-scope sidecar.
//! Do not copy the traces there too: they are read back out of the file's own
//! `## Evidence` section, so the two can never disagree.

use std::path::Path;

use stella_core::skills::SkillRejection;
use stella_tui::{LearnedProvenance, LearnedSource, SkillScope};

use super::{
    LearnedRecord, RejectedSkill, ScopeState, list_scope, read_state, scope_root, slugify,
    uninstall, write_state,
};
use stella_core::skills::SkillOrigin;

/// The heading `stella_core::skills::render_skill_markdown` writes above a
/// mined skill's source traces. Matched literally, because that is the only
/// contract between the two halves — the renderer is byte-pinned, so the
/// heading cannot drift out from under this without a failing test there.
const EVIDENCE_HEADING: &str = "## Evidence";

/// The source traces a learned skill's body records, newest first.
///
/// Parsed back out of the `## Evidence` bullets rather than stored a second
/// time: the miner already writes every trace into the file it creates, and a
/// sidecar copy would be a second answer to one question — free to disagree
/// with the file after a hand edit, which is exactly the failure the `was
/// <hash>` provenance below has to be a sidecar to avoid.
///
/// A line the renderer did not write is skipped, not guessed at: prose someone
/// added under the heading is prose, not a trace.
fn source_traces(body: &str) -> Vec<LearnedSource> {
    let mut out = Vec::new();
    let mut in_evidence = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == EVIDENCE_HEADING {
            in_evidence = true;
            continue;
        }
        // Any other heading ends the section — a hand-edited file may well
        // have more to say after it.
        if in_evidence && trimmed.starts_with("## ") {
            break;
        }
        if in_evidence && let Some(trace) = parse_trace(trimmed) {
            out.push(trace);
        }
    }
    out
}

/// One ``- `<reference>` (observed at <secs>): <snippet>`` bullet.
fn parse_trace(line: &str) -> Option<LearnedSource> {
    let rest = line.strip_prefix("- `")?;
    let (reference, rest) = rest.split_once("` (observed at ")?;
    let (observed_at, snippet) = rest.split_once("): ")?;
    Some(LearnedSource {
        reference: reference.to_string(),
        observed_at: observed_at.parse().ok()?,
        snippet: snippet.to_string(),
    })
}

/// The `<hash8>` suffix of a mined identity (`<slug>-<hash8>`), which is what
/// SPEC 9.2's `was <hash>` names. Empty when the identity carries no such
/// suffix — a hand-written `origin: auto` file, say — so the row drops the
/// segment instead of printing the whole name as if it were a hash.
fn mined_hash(mined_as: &str) -> String {
    match mined_as.rsplit_once('-') {
        Some((_, hash)) if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            hash.to_string()
        }
        _ => String::new(),
    }
}

/// Assemble a learned row's provenance: traces from the file, identity and
/// turn from the sidecar.
///
/// `None` when the file records no traces at all *and* the sidecar knows
/// nothing about it — there is then genuinely nothing to say, and an empty
/// provenance line reading `from 0 traces` would be worse than none.
///
/// A learned skill with no sidecar entry is the common case in any workspace
/// older than this feature: its name is still its mined identity, because
/// nothing has renamed it, so `was <hash>` is recoverable from the name alone
/// and only `turn M` is genuinely lost.
pub(super) fn learned_provenance(
    state: &ScopeState,
    name: &str,
    body: &str,
) -> Option<LearnedProvenance> {
    let sources = source_traces(body);
    let record = state.learned.get(name);
    let mined_as = record.map_or(name, |r| r.mined_as.as_str());
    let was = mined_hash(mined_as);
    if sources.is_empty() && record.is_none() && was.is_empty() {
        return None;
    }
    Some(LearnedProvenance {
        traces: u32::try_from(sources.len()).unwrap_or(u32::MAX),
        turn: record.and_then(|r| r.turn),
        was,
        sources,
    })
}

/// Record the provenance of a skill the learner just wrote.
///
/// Called by the learning loop at creation, which is the only moment the turn
/// is knowable — `SKILL.md` has nowhere to put it and the file's mtime is not
/// a turn. Best-effort by return type: a sidecar that will not write costs the
/// row its `turn M`, never the skill.
pub fn record_learned(
    scope: SkillScope,
    name: &str,
    mined_as: &str,
    turn: Option<u64>,
    workspace_root: &Path,
) -> Result<(), String> {
    let root = scope_root(scope, workspace_root)
        .ok_or_else(|| "no $HOME for the user scope".to_string())?;
    let mut state = read_state(&root);
    state.learned.insert(
        name.to_string(),
        LearnedRecord {
            mined_as: mined_as.to_string(),
            turn,
        },
    );
    write_state(&root, &state)
}

/// Rewrite a `SKILL.md`'s frontmatter `name:` to `to`, leaving every other
/// line — including a `description:` the user wrote — exactly as it was.
///
/// Line-oriented rather than a frontmatter re-render, for the same reason
/// [`super::frontmatter_prefix`] exists: the authored keys and their order are
/// the user's, and a rename is not licence to reformat their file.
fn rewrite_name(content: &str, to: &str) -> String {
    let mut out = String::with_capacity(content.len() + to.len());
    let mut fences = 0;
    let mut replaced = false;
    for line in content.split_inclusive('\n') {
        if fences < 2 && line.trim_end() == "---" {
            fences += 1;
            out.push_str(line);
            continue;
        }
        if fences == 1 && !replaced && line.trim_start().starts_with("name:") {
            let newline = if line.ends_with('\n') { "\n" } else { "" };
            out.push_str(&format!("name: {to}{newline}"));
            replaced = true;
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Give a skill a new name, keeping everything the name is a key for.
///
/// A learned skill lands as `<slug>-<hash8>`, which is a fine identity and a
/// poor label, so SPEC 9.2 renames it — and the rename must *keep* the
/// provenance rather than reset it, because the mined identity is what the
/// miner re-derives and what a rejection is recorded against. Concretely, in
/// one pass: move the entry on disk, rewrite the frontmatter `name:` (both the
/// pinned `SKILL.md` and every archived version, so a later `p pin` does not
/// resurrect the old name), and carry the `disabled` / `pins` / `learned`
/// entries over to the new key — recording `mined_as` from the OLD name when
/// nothing has renamed this skill before, which is precisely when the old name
/// still *is* the mined identity.
pub fn rename(
    scope: SkillScope,
    from: &str,
    to: &str,
    workspace_root: &Path,
) -> Result<String, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("a skill needs a name".to_string());
    }
    if to == from {
        return Ok(format!("{from} already has that name"));
    }
    let root = scope_root(scope, workspace_root)
        .ok_or_else(|| "no $HOME for the user scope".to_string())?;
    let entries = list_scope(scope, &root);
    if entries.iter().any(|e| e.skill.name == to) {
        return Err(format!(
            "a skill named {to} already exists in the {} scope",
            scope.label()
        ));
    }
    let entry = entries
        .iter()
        .find(|e| e.skill.name == from)
        .ok_or_else(|| format!("no skill named {from} in the {} scope", scope.label()))?;
    // Same containment guard `uninstall` applies, and for the same reason: the
    // entry we are about to move must be a direct child of the scope dir.
    if entry.entry_path.parent() != Some(root.as_path()) {
        return Err(format!(
            "{from} is not directly under {} — refusing to rename it",
            root.display()
        ));
    }
    let slug = slugify(to);
    let dest = root.join(if entry.entry_path.is_dir() {
        slug.clone()
    } else {
        format!("{slug}.md")
    });
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    std::fs::rename(&entry.entry_path, &dest).map_err(|e| format!("rename failed: {e}"))?;

    // Rewrite `name:` in the pinned file and in every archived version.
    let mut files = vec![if dest.is_dir() {
        dest.join("SKILL.md")
    } else {
        dest.clone()
    }];
    if let Ok(versions) = std::fs::read_dir(dest.join("versions")) {
        files.extend(versions.flatten().map(|v| v.path().join("SKILL.md")));
    }
    for file in files.iter().filter(|f| f.is_file()) {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        std::fs::write(file, rewrite_name(&content, to))
            .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    }

    let mut state = read_state(&root);
    for name in state.disabled.iter_mut().filter(|n| *n == from) {
        *name = to.to_string();
    }
    if let Some(pin) = state.pins.remove(from) {
        state.pins.insert(to.to_string(), pin);
    }
    // The provenance is the half a rename must not lose. An entry already
    // recorded moves as-is; a learned skill with none is being renamed for the
    // first time, so its OLD name is the mined identity and is captured now —
    // after which the hash survives every later rename.
    let record = state.learned.remove(from).unwrap_or(LearnedRecord {
        mined_as: from.to_string(),
        turn: None,
    });
    if entry.skill.origin == SkillOrigin::AutoCreated {
        state.learned.insert(to.to_string(), record);
    }
    write_state(&root, &state)?;
    Ok(format!("renamed {from} → {to} ({})", scope.label()))
}

/// Reject a learned skill: record the negative signal, then delete the file.
///
/// The order is the point. Deleting first and recording second would leave a
/// window — and, on a sidecar write that fails, a permanent state — where the
/// skill is gone and the miner has learned nothing, which is exactly the
/// cosmetic rejection this exists to replace. So the record lands first and a
/// failure to record refuses the whole operation.
///
/// `now` is the caller's clock rather than `SystemTime::now()`, so a test can
/// place a rejection at a known instant the way the rest of the lifecycle
/// already does.
pub fn reject(
    scope: SkillScope,
    name: &str,
    now: u64,
    workspace_root: &Path,
) -> Result<String, String> {
    let root = scope_root(scope, workspace_root)
        .ok_or_else(|| "no $HOME for the user scope".to_string())?;
    let entry = list_scope(scope, &root)
        .into_iter()
        .find(|e| e.skill.name == name)
        .ok_or_else(|| format!("no skill named {name} in the {} scope", scope.label()))?;
    if entry.skill.origin != SkillOrigin::AutoCreated {
        return Err(format!(
            "{name} was not learned from traces — there is nothing to teach the \
             learner. Use ctrl+x twice to delete it."
        ));
    }
    let mut state = read_state(&root);
    let mined_as = state
        .learned
        .get(name)
        .map_or_else(|| name.to_string(), |r| r.mined_as.clone());
    // The lesson, not the whole file: the miner compares against the
    // representative text it clustered, and the `## Evidence` appendix would
    // drag every trace's wording into that comparison.
    let lesson = lesson_of(&entry.skill.body);
    state.rejected.retain(|r| r.mined_as != mined_as);
    state.rejected.push(RejectedSkill {
        mined_as: mined_as.clone(),
        lesson,
        rejected_at: now,
        name: name.to_string(),
    });
    state.learned.remove(name);
    write_state(&root, &state)?;
    uninstall(scope, name, workspace_root)?;
    Ok(format!(
        "rejected {name} ({}) — the miner will not propose it again",
        scope.label()
    ))
}

/// A mined skill's body without the `## Evidence` appendix — the
/// representative lesson the cluster minted, which is the string the miner
/// clusters and compares against.
fn lesson_of(body: &str) -> String {
    body.split_once(EVIDENCE_HEADING)
        .map_or(body, |(lesson, _)| lesson)
        .trim()
        .to_string()
}

/// Every rejection recorded across both scopes, in the shape the miner reads.
///
/// Both scopes, because a rejection is a statement about a *lesson* and the
/// learner writes only into the project scope: a user who rejected the
/// user-scope copy of a skill has said what they think of it, and re-mining it
/// into the project scope next turn would be the same failure wearing a
/// different directory.
pub fn rejections(workspace_root: &Path) -> Vec<SkillRejection> {
    let mut out = Vec::new();
    for scope in [SkillScope::Project, SkillScope::User] {
        let Some(root) = scope_root(scope, workspace_root) else {
            continue;
        };
        out.extend(
            read_state(&root)
                .rejected
                .into_iter()
                .map(|r| SkillRejection {
                    mined_as: r.mined_as,
                    lesson: r.lesson,
                    rejected_at: r.rejected_at,
                }),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{scratch, write_skill};
    use super::super::{enumerate, save_edit, set_enabled, set_pin};
    use super::*;

    /// A mined skill exactly as `render_skill_markdown` writes it — the same
    /// bytes the learner puts on disk, so the provenance parser below is
    /// reading the real format rather than one this test invented.
    fn write_learned_skill(dir: &Path, name: &str, lesson: &str, traces: &[u64]) {
        let candidate = stella_core::skills::SkillCandidate {
            name: name.to_string(),
            description: format!("Learned from {} observations.", traces.len()),
            domains: vec!["testing".into()],
            body: lesson.to_string(),
            occurrences: traces.len(),
            salient: false,
            evidence: {
                // Newest first, the order `mine_skill_candidates` sorts a
                // cluster into before it renders the section.
                let mut newest: Vec<u64> = traces.to_vec();
                newest.sort_unstable_by(|a, b| b.cmp(a));
                newest
                    .into_iter()
                    .map(|at| stella_core::skills::SkillEvidence {
                        reference: format!("reflection:{at}"),
                        occurred_at: at,
                        snippet: lesson.to_string(),
                    })
                    .collect()
            },
            score: 30.0,
        };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.md")),
            stella_core::skills::render_skill_markdown(&candidate),
        )
        .unwrap();
    }

    /// A learned row carries SPEC 9.2's provenance: the traces come out of the
    /// file's own `## Evidence` section, and the mined `<hash8>` out of its
    /// name — no sidecar needed for a skill nobody has renamed yet.
    #[test]
    fn enumerate_reads_a_learned_skills_traces_out_of_its_file() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "money amounts must be stored as minor units";
        let name = stella_core::skills::candidate_id(lesson);
        write_learned_skill(&ws.join(".stella/skills"), &name, lesson, &[100, 200, 300]);

        let rows = enumerate(&ws);
        let row = rows.iter().find(|r| r.name == name).expect("learned row");
        let learned = row.learned.as_ref().expect("provenance");
        assert_eq!(learned.traces, 3);
        assert_eq!(
            learned.was,
            name.rsplit_once('-').unwrap().1,
            "the `was <hash>` is the mined id's hash suffix"
        );
        assert_eq!(learned.turn, None, "nothing recorded a turn for it");
        assert_eq!(
            learned
                .sources
                .iter()
                .map(|s| s.observed_at)
                .collect::<Vec<_>>(),
            vec![300, 200, 100],
            "the traces come back in the order the file lists them"
        );
        assert!(learned.sources[0].reference.starts_with("reflection:"));
    }

    /// The turn the learner recorded reaches the row.
    #[test]
    fn a_recorded_turn_reaches_the_learned_row() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "run the migration before the integration suite";
        let name = stella_core::skills::candidate_id(lesson);
        write_learned_skill(&ws.join(".stella/skills"), &name, lesson, &[7, 8]);
        record_learned(SkillScope::Project, &name, &name, Some(37), &ws).unwrap();

        let rows = enumerate(&ws);
        let learned = rows
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.learned.as_ref())
            .expect("provenance");
        assert_eq!(learned.turn, Some(37));
    }

    /// Renaming a learned skill KEEPS the `was <hash>` — the property the
    /// whole verb exists for. The file moves, the frontmatter `name:` follows,
    /// and the row still names the identity it was mined under.
    #[test]
    fn rename_keeps_the_was_hash_provenance() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "money amounts must be stored as minor units";
        let mined = stella_core::skills::candidate_id(lesson);
        let hash = mined.rsplit_once('-').unwrap().1.to_string();
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[100, 200, 300]);

        rename(SkillScope::Project, &mined, "money-is-minor-units", &ws).unwrap();

        let rows = enumerate(&ws);
        assert!(
            rows.iter().all(|r| r.name != mined),
            "the mined name is gone from the list"
        );
        let row = rows
            .iter()
            .find(|r| r.name == "money-is-minor-units")
            .expect("the renamed row");
        assert_eq!(row.origin, "auto", "it is still a learned skill");
        let learned = row.learned.as_ref().expect("provenance survives");
        assert_eq!(learned.was, hash, "`was <hash>` survived the rename");
        assert_eq!(learned.traces, 3, "so did its traces");
    }

    /// A second rename keeps the ORIGINAL mined hash, not the previous
    /// human name — provenance is about where the skill came from, and a
    /// chain of renames does not move that.
    #[test]
    fn a_second_rename_still_names_the_original_mined_hash() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "feature flags default closed";
        let mined = stella_core::skills::candidate_id(lesson);
        let hash = mined.rsplit_once('-').unwrap().1.to_string();
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[1, 2, 3]);

        rename(SkillScope::Project, &mined, "flags-closed", &ws).unwrap();
        rename(SkillScope::Project, "flags-closed", "flag-defaults", &ws).unwrap();

        let rows = enumerate(&ws);
        let learned = rows
            .iter()
            .find(|r| r.name == "flag-defaults")
            .and_then(|r| r.learned.as_ref())
            .expect("provenance");
        assert_eq!(learned.was, hash);
    }

    /// Rename carries the state the name is a key for. A disabled, pinned
    /// skill must not silently come back on, or start reading a different
    /// version, because it was given a better name.
    #[test]
    fn rename_carries_the_disabled_and_pinned_state_across() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let dir = ws.join(".stella/skills");
        write_skill(&dir, "sql-style", "format sql");
        // Give it a second version so a pin is a real choice.
        save_edit(SkillScope::Project, "sql-style", "v2 body", &ws).unwrap();
        set_pin(SkillScope::Project, "sql-style", 1, &ws).unwrap();
        set_enabled(SkillScope::Project, "sql-style", false, &ws).unwrap();

        rename(SkillScope::Project, "sql-style", "sql-formatting", &ws).unwrap();

        let rows = enumerate(&ws);
        let row = rows
            .iter()
            .find(|r| r.name == "sql-formatting")
            .expect("renamed row");
        assert!(!row.enabled, "it is still disabled");
        assert_eq!(row.version, 1, "it is still pinned to v1");
        assert_eq!(row.latest, 2);
    }

    #[test]
    fn rename_refuses_a_name_that_is_already_taken() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let dir = ws.join(".stella/skills");
        write_skill(&dir, "one", "first");
        write_skill(&dir, "two", "second");
        let error = rename(SkillScope::Project, "one", "two", &ws).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert!(
            enumerate(&ws).iter().any(|r| r.name == "one"),
            "and the refused rename left the original alone"
        );
    }

    /// **Witness (#5046), the disk half.** Rejecting a learned skill deletes
    /// it AND leaves the negative signal behind in the shape the miner reads —
    /// keyed on the mined identity, carrying the lesson but not the evidence
    /// appendix.
    #[test]
    fn reject_deletes_the_skill_and_records_the_signal_the_miner_reads() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "money amounts must be stored as minor units";
        let mined = stella_core::skills::candidate_id(lesson);
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[100, 200, 300]);

        reject(SkillScope::Project, &mined, 1_700_000_000, &ws).unwrap();

        assert!(
            enumerate(&ws).is_empty(),
            "the row is gone from the tab as well"
        );
        let recorded = rejections(&ws);
        assert_eq!(recorded.len(), 1, "one rejection: {recorded:?}");
        assert_eq!(recorded[0].mined_as, mined);
        assert_eq!(
            recorded[0].lesson, lesson,
            "the lesson, without the `## Evidence` appendix — that is what \
             the miner clusters"
        );
        assert_eq!(recorded[0].rejected_at, 1_700_000_000);
    }

    /// A rejection recorded against a skill that was renamed first still names
    /// the mined identity, because that is the only key the miner re-derives.
    #[test]
    fn rejecting_a_renamed_skill_records_its_mined_identity() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "terraform plans belong in review before anyone applies them";
        let mined = stella_core::skills::candidate_id(lesson);
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[1, 2, 3]);
        rename(SkillScope::Project, &mined, "review-plans", &ws).unwrap();

        reject(SkillScope::Project, "review-plans", 5, &ws).unwrap();

        let recorded = rejections(&ws);
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].mined_as, mined,
            "the human name would be invisible to the miner"
        );
    }

    /// `x` is for skills the loop wrote. A hand-authored skill has no learner
    /// to teach, and the refusal names the key that does delete it rather than
    /// leaving the user pressing one that never will.
    #[test]
    fn reject_refuses_a_skill_nobody_learned() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        write_skill(&ws.join(".stella/skills"), "hand-written", "authored");
        let error = reject(SkillScope::Project, "hand-written", 1, &ws).unwrap_err();
        assert!(error.contains("not learned from traces"), "{error}");
        assert!(
            enumerate(&ws).iter().any(|r| r.name == "hand-written"),
            "and it is still on disk"
        );
    }

    /// Deleting a rejected skill's *file* a second way — an ordinary
    /// uninstall — must not take the rejection with it. The record is keyed on
    /// a mined identity, not on a file, and outliving the file is the point.
    #[test]
    fn uninstall_does_not_erase_a_recorded_rejection() {
        let (td, _home, _lock) = scratch();
        let ws = td.path().join("ws");
        let lesson = "keep generated bindings out of version control";
        let mined = stella_core::skills::candidate_id(lesson);
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[1, 2, 3]);
        reject(SkillScope::Project, &mined, 3, &ws).unwrap();
        // Re-create it by hand, then delete it the ordinary way.
        write_learned_skill(&ws.join(".stella/skills"), &mined, lesson, &[1, 2, 3]);
        uninstall(SkillScope::Project, &mined, &ws).unwrap();
        assert_eq!(rejections(&ws).len(), 1, "the rejection is still recorded");
    }
}
