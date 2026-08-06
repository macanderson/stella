//! Post-seal escape detection (#1538) — did this candidate write *through*
//! its isolation into the real tree? A child module of `candidate_ws`, like
//! [`super::adopt`], so the workspace's private fields stay reachable.
//!
//! Every other isolation property of a candidate workspace is structural
//! (detached worktree, re-rooted tool registry, `O_NOFOLLOW` writes); the
//! shell is the one capability that can still reach the real tree, because
//! the bash tool is unconfined (#1300 removed the local sandbox). Until this
//! module, that escape surfaced only when it happened to collide with the
//! winner's adoption — an entire turn lost, attributed after the fact by
//! [`super::adopt`]'s conflict forensics. This runs the same blob-signature
//! comparison *proactively*, immediately after each seal, so the run fails
//! the escaping candidate in the round that caused it.
//!
//! The discriminator is unchanged from [`super::adopt::PathDivergence`]: the
//! candidate's sealed blob is a signature nothing else on the machine
//! produces, so a real-tree path holding those exact bytes where the baseline
//! differed was written by this candidate. A mid-run *user* edit classifies
//! as [`PathDivergence::ChangedUnderTheRun`] and is deliberately not this
//! module's business.
//!
//! Detection boundaries, accepted and documented: an escape that never enters
//! the candidate's own sealed delta (bytes written *only* outside the
//! snapshot) carries no signature and is invisible here, as is an escape by
//! *deletion* (no bytes to sign). For those, adoption's conflict attribution
//! remains the backstop. Best-effort throughout — every failed probe reports
//! "no escape" rather than failing a candidate on broken forensics.

use super::adopt::{PathDivergence, blob_id};
use super::{GitCandidateWorkspace, git};

/// Ceiling on blob classifications per check — three `git` probes each, on
/// top of the two flat enumeration calls. A candidate's delta is normally a
/// handful of paths; a run that changed more than this many *suspicious*
/// paths gets its first `MAX_CLASSIFIED` examined, and the adoption-time
/// attribution still covers anything beyond the cap.
const MAX_CLASSIFIED: usize = 24;

/// Ceiling on pathspecs handed to the one real-tree `git status` call, so an
/// enormous candidate delta cannot build an unbounded argv.
const MAX_STATUS_PATHSPECS: usize = 400;

impl GitCandidateWorkspace {
    /// Workspace-relative paths where the real tree already holds this
    /// candidate's own sealed bytes at a path whose baseline differed — the
    /// port contract of `CandidateWorkspace::escaped_paths` (#1538).
    ///
    /// Cost on a well-behaved candidate: exactly two `git` invocations (the
    /// delta enumeration in the shadow, one pathspec-limited `status` against
    /// the real tree). The three-probe blob classification is paid only for
    /// paths the real tree reports moved *and* the candidate also changed —
    /// normally none.
    pub(super) async fn escaped_paths_inner(&self) -> Vec<String> {
        let Some(sealed) = self
            .sealed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        else {
            // Never sealed: there are no candidate bytes to sign a match with.
            return Vec::new();
        };
        let Ok(delta) = git(
            &self.dir,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                &self.baseline,
                &sealed,
            ],
        )
        .await
        else {
            return Vec::new();
        };
        let changed: Vec<&str> = delta
            .split('\0')
            .filter(|s| !s.is_empty())
            .take(MAX_STATUS_PATHSPECS)
            .collect();
        if changed.is_empty() {
            return Vec::new();
        }
        // One real-tree `status` over exactly the candidate's own paths. A
        // path the real tree does not report moved cannot be holding fresh
        // candidate bytes (modulo a candidate re-creating HEAD's exact
        // content, which also cannot collide with adoption), so everything
        // clean is skipped without paying a blob probe.
        let mut status_args = vec![
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
        ];
        status_args.extend(changed.iter().copied());
        let Ok(status) = git(&self.toplevel, &status_args).await else {
            return Vec::new();
        };
        let mut escaped = Vec::new();
        for path in status_paths(&status).into_iter().take(MAX_CLASSIFIED) {
            // The same three probes as `adopt`'s failure-path attribution:
            // what the real tree holds now, what the candidate started from,
            // and what it sealed. `--path` keeps clean filters in play.
            let real =
                blob_id(git(&self.toplevel, &["hash-object", "--path", path, "--", path]).await);
            let baseline = blob_id(
                git(
                    &self.dir,
                    &["rev-parse", &format!("{}:{path}", self.baseline)],
                )
                .await,
            );
            let sealed_blob =
                blob_id(git(&self.dir, &["rev-parse", &format!("{sealed}:{path}")]).await);
            if PathDivergence::classify(
                real.as_deref(),
                baseline.as_deref(),
                sealed_blob.as_deref(),
            ) == PathDivergence::CandidateWroteOutside
            {
                escaped.push(path.to_string());
            }
        }
        escaped.sort();
        escaped
    }
}

/// The paths in `git status --porcelain -z` output: NUL-separated
/// `XY <path>` entries (two status characters and a space; renames are
/// disabled by the caller, so no two-path records).
fn status_paths(raw: &str) -> Vec<&str> {
    raw.split('\0')
        .filter_map(|entry| entry.get(3..))
        .filter(|path| !path.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use stella_pipeline::ports::CandidateWorkspace;
    use stella_tools::RegistryOptions;

    use super::super::GitCandidateWorkspaces;
    use super::super::tests::{read, scaffold};
    use super::status_paths;

    fn port(root: std::path::PathBuf) -> GitCandidateWorkspaces {
        GitCandidateWorkspaces::new(
            root,
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        )
    }

    /// The observed Terminal-Bench failure shape: the candidate writes its
    /// answer inside the snapshot AND copies it to the real tree. The check
    /// names the path in the round it happened — before adoption can collide.
    #[tokio::test]
    async fn a_candidate_copying_its_answer_out_is_named_after_seal() {
        let root = scaffold("escape-out");
        let port = port(root.clone());
        let ws = port.create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("answer.js"), "const answer = 42;\n").unwrap();
        // The escape: the same bytes land in the real tree out-of-band.
        std::fs::write(root.join("answer.js"), "const answer = 42;\n").unwrap();
        ws.seal().await.unwrap();

        assert_eq!(ws.escaped_paths().await, vec!["answer.js".to_string()]);

        // Detection only — the real tree is left exactly as found for the
        // caller to report; nothing here mutates either tree.
        assert_eq!(read(&root.join("answer.js")), "const answer = 42;\n");
        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A mid-run user edit at a path the candidate also changed is the OTHER
    /// divergence — it must never be blamed on the candidate.
    #[tokio::test]
    async fn a_mid_run_user_edit_is_not_an_escape() {
        let root = scaffold("escape-user-edit");
        let port = port(root.clone());
        let ws = port.create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        std::fs::write(root.join("tracked.txt"), "base\nuser-edit\n").unwrap();
        ws.seal().await.unwrap();

        assert_eq!(ws.escaped_paths().await, Vec::<String>::new());
        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A file that was already dirty when the run began (and so is dirty in
    /// `git status` throughout) still matches the snapshotted baseline —
    /// the probe classifies it as untouched, not as an escape.
    #[tokio::test]
    async fn pre_existing_dirt_the_candidate_edited_only_in_its_snapshot_is_clean() {
        let root = scaffold("escape-preexisting");
        let port = port(root.clone());
        let ws = port.create_workspace().await.unwrap();

        // `tracked.txt` is dirty vs HEAD in the real tree by construction
        // (scaffold wrote "base\ndirty\n" over the committed "base\n").
        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        ws.seal().await.unwrap();

        assert_eq!(ws.escaped_paths().await, Vec::<String>::new());
        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// Before any seal there are no candidate bytes to match — the check
    /// stays quiet instead of guessing.
    #[tokio::test]
    async fn an_unsealed_workspace_reports_nothing() {
        let root = scaffold("escape-unsealed");
        let port = port(root.clone());
        let ws = port.create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("answer.js"), "const answer = 42;\n").unwrap();
        std::fs::write(root.join("answer.js"), "const answer = 42;\n").unwrap();

        assert_eq!(ws.escaped_paths().await, Vec::<String>::new());
        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn status_paths_reads_porcelain_z_records() {
        assert_eq!(
            status_paths("?? new file.js\0 M tracked.txt\0"),
            vec!["new file.js", "tracked.txt"]
        );
        assert_eq!(status_paths(""), Vec::<&str>::new());
    }
}
