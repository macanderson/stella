//! The witness's **path frame**: which tree its assertions are written
//! against, versus the tree the oracle actually runs them in (#2130).
//!
//! Witness authoring is a two-tree operation by construction (see
//! [`crate::pipeline`]'s `witness_stage`): the artifact is authored in a
//! pristine snapshot, grafted into the candidate that executed, and observed
//! there. Neither of those roots is the project's own root, and the two are
//! not each other. The one frame that resolves in all three is a path
//! **relative to the test's working directory** — which is the root of
//! whichever copy is running it.
//!
//! An author given a goal phrased in the project's absolute paths ("create
//! `/app/ssl/server.crt`") does not know that, and writes them down. The
//! result is unsatisfiable by construction: on `openssl-selfsigned-cert`
//! (match `cc00894779ff`) the authored script pinned `SSL_DIR=/app/ssl` while
//! the oracle ran it inside `/tmp/stella_candidate_460_0/`, so both oracle
//! runs failed with the identical hash `failure:4b33a44975e094db` — the same
//! answer they would give for any change whatsoever, correct or not. The
//! "missing file" verdicts that followed sent the worker through two
//! misdiagnosis loops and rode a trial that was otherwise finished into its
//! 900-second timeout at reward 0.
//!
//! [`screen_witness_frame`] is the enforced half of the answer, applied to
//! the authored bytes at the `create_witness_test` boundary beside
//! [`super::density::screen_witness_source`] — the same boundary, for the
//! same reason: it is the only path witness bytes take to disk, so a check
//! there is complete rather than advisory, and a refusal arrives as a tool
//! error *inside the author's own turn*, where its one create is still
//! available and it can revise for free. The advisory half is the prompt
//! ([`super::witness_prompt`]), which states the frame and names the
//! project root so the author can translate the goal's paths itself.
//!
//! # What is not screened
//!
//! Absolute paths to the **machine** — `/bin/sh`, `/usr/bin/openssl`,
//! `/etc/nginx/nginx.conf` — are left alone. They are not the tree under
//! test, they mean the same thing in every copy of it, and an infra task
//! whose deliverable genuinely lives outside the workspace root must still be
//! able to assert on it. Only the roots the caller declares are refused.

/// Why an authored witness could never be satisfied by correct work: it
/// anchors an assertion to a tree the oracle will not run it in.
///
/// One variant, and it names both the offending literal and the form the
/// author should have written — the message is what the author revises
/// against, exactly as in [`super::density::VacuousWitness`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MisframedWitness {
    /// The source hardcodes an absolute path into one of the declared trees.
    #[error(
        "the witness hardcodes `{literal}`, an absolute path into the project tree. Your test \
         is executed inside an isolated COPY of that tree, rooted at a different directory, so \
         `{literal}` reads a tree the work under test never touches — no correct change can \
         ever satisfy it. Assert on `{relative}` instead: a path relative to the test's working \
         directory resolves in whichever copy runs it. Absolute paths to the MACHINE \
         (`/bin/sh`, `/usr/bin/openssl`) are fine — those are not the tree under test."
    )]
    TreeAbsolutePath {
        /// The offending path token, quoted exactly as the author wrote it.
        literal: String,
        /// The same location relative to the test's working directory — the
        /// form that resolves in every copy. `.` when the literal named a
        /// root itself.
        relative: String,
    },
}

/// Screen an authored witness's source for assertions anchored to a tree the
/// test will not run in.
///
/// `tree_roots` are the absolute roots that must not appear: the project's own
/// workspace root (the frame the goal is phrased in) and the isolated copy the
/// author is standing in (which the graft is about to leave behind). Order is
/// the caller's; the first offending literal found wins, so list the root an
/// author is likeliest to have copied from the goal first.
///
/// `Ok(())` means no declared root is anchored to — not that the paths are
/// right, only that they are expressed in a frame that can resolve.
///
/// Backslash spellings are matched too: the comparison runs over a
/// `\`→`/` copy of both the roots and the source. That substitution is
/// byte-for-byte length-preserving, so every offset still indexes the original
/// bytes and the quoted literal is what the author actually typed — the same
/// technique [`super::density`] uses to quote from unsanitized source.
pub fn screen_witness_frame(tree_roots: &[String], source: &str) -> Result<(), MisframedWitness> {
    let normalized = source.replace('\\', "/");
    for root in tree_roots {
        let root = root.replace('\\', "/");
        let Some(root) = framing_root(&root) else {
            continue;
        };
        if let Some(misframed) = first_anchored_reference(root, source, &normalized) {
            return Err(misframed);
        }
    }
    Ok(())
}

/// A root that can serve as a frame — absolute, naming at least one component
/// — trimmed of trailing separators, or `None` when it cannot.
///
/// One answer, consulted by both halves of the fix, so the prompt's prose and
/// this module's enforcement can never disagree about which roots participate:
/// [`super::witness_prompt`] states a root only when this returns it, and
/// [`screen_witness_frame`] matches on exactly the same set.
///
/// The filesystem root is excluded rather than special-cased. Every absolute
/// path is "inside" `/`, so screening for it would refuse `/bin/sh` and leave
/// the author no legal move, and a workspace rooted there has no relative
/// frame to offer anyway. A drive-qualified Windows root (`C:\work`)
/// qualifies; a bare drive (`C:\`) does not, for the same reason `/` does not.
pub fn framing_root(root: &str) -> Option<&str> {
    let trimmed = root.trim_end_matches(['/', '\\']);
    let normalized = trimmed.replace('\\', "/");
    let has_components = match normalized.split_once(":/") {
        Some((drive, rest)) if drive.len() == 1 => !rest.is_empty(),
        _ => normalized.starts_with('/') && normalized.len() > 1,
    };
    has_components.then_some(trimmed)
}

/// The first path token in `source` anchored at `root`, as the refusal the
/// author should read.
fn first_anchored_reference(
    root: &str,
    source: &str,
    normalized: &str,
) -> Option<MisframedWitness> {
    let bytes = normalized.as_bytes();
    for (start, _) in normalized.match_indices(root) {
        // A token that merely *contains* the root's spelling is a different
        // path: `/tmp/app/x` is not `/app`, and `$PREFIX/app` is already
        // relative to something.
        if start > 0
            && bytes
                .get(start - 1)
                .is_some_and(|byte| continues_a_token(*byte))
        {
            continue;
        }
        let end = start + root.len();
        // Nor is `/appliance` `/app`. Only a separator (a child of the root)
        // or a token boundary (the root named alone) counts.
        if bytes
            .get(end)
            .is_some_and(|byte| *byte != b'/' && is_path_char(*byte))
        {
            continue;
        }
        let stop = bytes
            .get(end..)
            .and_then(|rest| rest.iter().position(|byte| !is_path_char(*byte)))
            .map_or(bytes.len(), |offset| end + offset);
        // Every byte in `start..stop` is ASCII, so `stop` lands on a character
        // boundary — but this reads model-authored bytes, so it asks rather
        // than asserts (invariant 5).
        let (Some(literal), Some(tail)) = (source.get(start..stop), normalized.get(end..stop))
        else {
            continue;
        };
        let relative = tail.trim_start_matches('/');
        return Some(MisframedWitness::TreeAbsolutePath {
            literal: literal.to_string(),
            relative: if relative.is_empty() {
                ".".to_string()
            } else {
                relative.to_string()
            },
        });
    }
    None
}

/// Bytes that continue a filesystem path token.
fn is_path_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

/// Whether a byte *preceding* a match means the match is mid-token. Non-ASCII
/// bytes count: the screen would rather miss a reference embedded in prose it
/// cannot segment than refuse a legitimate witness over one.
fn continues_a_token(byte: u8) -> bool {
    is_path_char(byte) || !byte.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(roots: &[&str]) -> Vec<String> {
        roots.iter().map(|root| (*root).to_string()).collect()
    }

    /// #2130's shape, verbatim in outline from the trace that motivated this:
    /// a shell witness that pins the project's absolute paths while the oracle
    /// runs it inside a candidate copy. It is refused, and the refusal hands
    /// back the working-directory-relative form the author should have used.
    #[test]
    fn the_openssl_witness_shape_is_refused_and_names_the_relative_form() {
        let source = "#!/bin/sh\n\
                      # Witness: self-signed TLS certificate provisioning.\n\
                      set -e\n\
                      SSL_DIR=/app/ssl\n\
                      PY=/app/check_cert.py\n\
                      test -f \"$SSL_DIR/server.crt\" || exit 1\n";
        match screen_witness_frame(&roots(&["/app"]), source) {
            Err(MisframedWitness::TreeAbsolutePath { literal, relative }) => {
                assert_eq!(literal, "/app/ssl");
                assert_eq!(relative, "ssl");
            }
            other => panic!("the misframed witness must be refused, got {other:?}"),
        }
    }

    /// The trees are two, and anchoring to either is equally unsatisfiable:
    /// the authoring snapshot is discarded after the graft, so a witness
    /// pinned to it reads a directory that no longer exists by verify time.
    #[test]
    fn the_authoring_copy_is_as_wrong_as_the_project_root() {
        let source = "test -f /tmp/stella_candidate_460_0/ssl/server.crt\n";
        let declared = roots(&["/app", "/tmp/stella_candidate_460_0"]);
        match screen_witness_frame(&declared, source) {
            Err(MisframedWitness::TreeAbsolutePath { literal, relative }) => {
                assert_eq!(literal, "/tmp/stella_candidate_460_0/ssl/server.crt");
                assert_eq!(relative, "ssl/server.crt");
            }
            other => panic!("the authoring root must be refused too, got {other:?}"),
        }
    }

    /// Naming the root itself — `cd /app`, the commonest first line of a
    /// misframed shell witness — is the same defect, and its relative form is
    /// the working directory.
    #[test]
    fn the_root_named_alone_resolves_to_the_working_directory() {
        match screen_witness_frame(&roots(&["/app/"]), "cd /app\nls ssl\n") {
            Err(MisframedWitness::TreeAbsolutePath { literal, relative }) => {
                assert_eq!(literal, "/app");
                assert_eq!(relative, ".", "the root itself is the working directory");
            }
            other => panic!("a bare root reference must be refused, got {other:?}"),
        }
    }

    /// The screen refuses the tree under test and nothing else. A witness
    /// needs an interpreter, the tools it shells out to, and often a genuinely
    /// external deliverable; a lookalike directory is not the root; and the
    /// relative form the author is being pushed toward must of course pass.
    #[test]
    fn machine_paths_lookalikes_and_the_relative_form_are_left_alone() {
        for source in [
            "#!/bin/sh\nopenssl x509 -in ssl/server.crt -noout\n",
            "test -f /usr/bin/openssl && test -x /etc/nginx/nginx.conf\n",
            // `/appliance` and `/app.d` merely start with the root's spelling.
            "test -d /appliance/ssl && test -d /app.d/ssl\n",
            // Mid-token: a longer absolute path, and a prefix variable join.
            "test -f /tmp/app/ssl/x && test -f \"$PREFIX/app/ssl/x\"\n",
            "curl https://internal.example.com/app/ssl/server.crt\n",
            // The frame the author is supposed to use.
            "SSL_DIR=ssl\ntest -f \"$SSL_DIR/server.crt\"\n",
        ] {
            assert_eq!(
                screen_witness_frame(&roots(&["/app"]), source),
                Ok(()),
                "must not refuse: {source}"
            );
        }
    }

    /// A root with no relative frame to offer is not screened: every absolute
    /// path is inside `/`, so screening for it would refuse `/bin/sh` and
    /// leave the author no legal move at all. Empty and relative roots are
    /// likewise skipped rather than matched against everything.
    ///
    /// [`framing_root`] is the single answer both halves read, so the prompt
    /// that states a root and the screen that enforces it agree by
    /// construction — this asserts the two directions together.
    #[test]
    fn a_root_that_cannot_frame_anything_is_neither_stated_nor_screened() {
        for root in ["/", "//", "", ".", "relative/root", "C:\\"] {
            assert_eq!(framing_root(root), None, "root {root:?} frames nothing");
            assert_eq!(
                screen_witness_frame(&roots(&[root]), "test -f /bin/sh && test -f /app/ssl\n"),
                Ok(()),
                "root {root:?} must not be screened"
            );
        }
        assert_eq!(framing_root("/app/"), Some("/app"));
        assert_eq!(framing_root("C:\\work\\"), Some("C:\\work"));
    }

    /// Backslash spellings are the same reference. The comparison runs over a
    /// length-preserving copy, so the quoted literal still comes from the
    /// author's own bytes — and multi-byte prose neither panics nor produces a
    /// mid-character quote.
    #[test]
    fn backslash_spellings_match_and_multibyte_prose_is_safe() {
        match screen_witness_frame(&roots(&["C:\\work"]), "assert Path(r\"C:\\work\\ssl\")\n") {
            Err(MisframedWitness::TreeAbsolutePath { literal, relative }) => {
                assert_eq!(literal, "C:\\work\\ssl", "quotes the author's own spelling");
                assert_eq!(relative, "ssl");
            }
            other => panic!("a drive-qualified root must be refused, got {other:?}"),
        }
        // Non-ASCII on both sides of a lookalike: no panic, no refusal.
        assert_eq!(
            screen_witness_frame(&roots(&["/app"]), "# 証明書は/appにある\n"),
            Ok(())
        );
        // ...and a real reference introduced by non-ASCII prose is still a
        // token start when a space separates them.
        assert!(screen_witness_frame(&roots(&["/app"]), "# 証明書: /app/ssl\n").is_err());
    }
}
