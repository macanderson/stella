use super::*;

use stella_tools::rootfd::RootHandle;

/// The granted tree, held open — the only way in to an artifact identity since
/// #3483 replaced the resolve-then-open pair with one confined walk.
fn watched(root: &std::path::Path) -> RootHandle {
    RootHandle::open(root).expect("a real directory opens")
}

#[cfg(unix)]
#[test]
fn witness_identity_rejects_symlinks_hardlinks_and_hashes_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("witness.rs");
    let hardlink = dir.path().join("hardlink.rs");
    let symlink = dir.path().join("symlink.rs");
    std::fs::write(&file, "test bytes\n").unwrap();
    let root = watched(dir.path());

    let before = fs_artifact_identity(&root, "witness.rs").unwrap();
    assert_eq!(before.kind, ArtifactKind::Regular);
    assert!(before.is_regular_single_link());
    assert_eq!(
        before.path, "witness.rs",
        "the identity attests the location it was observed at"
    );

    std::fs::hard_link(&file, &hardlink).unwrap();
    assert!(
        fs_artifact_identity(&root, "witness.rs").is_none(),
        "multi-link files fail closed at the identity boundary"
    );

    std::os::unix::fs::symlink(&file, &symlink).unwrap();
    assert!(
        fs_artifact_identity(&root, "symlink.rs").is_none(),
        "no-follow identity must never open a symlink target"
    );

    std::fs::remove_file(&hardlink).unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&file, permissions).unwrap();
    let executable = fs_artifact_identity(&root, "witness.rs").unwrap();
    assert_ne!(before.fingerprint, executable.fingerprint);
}

#[cfg(unix)]
#[test]
fn opened_witness_identity_rejects_path_retarget_before_fingerprinting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("witness.rs");
    let moved = dir.path().join("original.rs");
    std::fs::write(&path, "original bytes\n").unwrap();
    let root = watched(dir.path());
    let entry = root
        .open_entry("witness.rs")
        .expect("regular file opens no-follow");

    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, "replacement bytes\n").unwrap();

    assert!(
        witness_identity(&entry).is_none(),
        "the opened handle must not be credited after its name is retargeted"
    );
}

#[cfg(unix)]
#[test]
fn witness_identity_attests_the_observed_location_through_an_aliased_lookup() {
    // A renamed witness can stay reachable at its pinned path when the
    // lookup is aliased — here a symlinked parent directory, the same
    // shape a case-folding filesystem produces. The identity must report
    // where the bytes actually live, so the tamper watch's pinned-path
    // equality rejects the move as tampering.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("moved")).unwrap();
    std::fs::write(dir.path().join("moved/witness.rs"), "test bytes\n").unwrap();
    std::os::unix::fs::symlink(dir.path().join("moved"), dir.path().join("tests")).unwrap();

    let identity = fs_artifact_identity(&watched(dir.path()), "tests/witness.rs")
        .expect("the aliased lookup still opens a regular file");
    assert_eq!(
        identity.path, "moved/witness.rs",
        "the attested path is where the walk landed, not the asked-for one"
    );
    assert!(
        !stella_plugin::witness_identity_matches(
            &ArtifactIdentity {
                path: "tests/witness.rs".into(),
                ..identity.clone()
            },
            Some(&identity)
        ),
        "an identity pinned at the asked-for path must reject the moved artifact"
    );
}

/// An interior directory that is a symlink out of the tree yields no identity.
///
/// The answer is the one the pre-#3483 code already gave; what changed is where
/// it comes from. That version followed the link, opened the outside file and
/// hashed its bytes, and only then discarded the result because the location
/// would not state itself inside the root. The confined walk refuses at the
/// component, so the outside file is never opened at all — which is why this is
/// a containment regression guard and not a witness for #3483.
#[cfg(unix)]
#[test]
fn an_artifact_reached_through_an_outward_link_has_no_identity() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("witness.rs"), "test bytes\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("tests")).unwrap();

    assert!(
        fs_artifact_identity(&watched(dir.path()), "tests/witness.rs").is_none(),
        "an interior link out of the root is not a path into the root"
    );
}

#[test]
fn witness_identity_requires_established_platform_link_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("witness.rs"), "test bytes\n").unwrap();
    let identity = fs_artifact_identity(&watched(dir.path()), "witness.rs");
    #[cfg(unix)]
    assert!(
        identity.is_some(),
        "Unix exposes link count from the handle"
    );
    #[cfg(not(unix))]
    assert!(
        identity.is_none(),
        "platforms without a stable handle link count fail closed"
    );
}
