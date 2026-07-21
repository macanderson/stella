use std::path::Path;

use rusqlite::Connection;
use stella_protocol::AgentEvent;

use crate::Store;

#[cfg(unix)]
#[test]
fn store_db_is_private_inside_permissive_dot_stella_without_chmodding_project_files() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let dot = root.path().join(".stella");
    let rules = dot.join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o777)).unwrap();
    let settings = dot.join("settings.json");
    let rule = rules.join("canonical.md");
    std::fs::write(&settings, "{}\n").unwrap();
    std::fs::write(&rule, "# canonical\n").unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o664)).unwrap();
    std::fs::set_permissions(&rule, std::fs::Permissions::from_mode(0o664)).unwrap();

    let store = Store::open(root.path()).unwrap();
    let execution = store
        .begin_execution("test", "sensitive prompt", "local", "test")
        .unwrap();
    store
        .record_event(
            execution,
            0,
            &AgentEvent::Text {
                delta: "private transcript".into(),
            },
        )
        .unwrap();

    let mode = |path: &Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        mode(&dot),
        0o777,
        "mixed project directory is not private state"
    );
    assert_eq!(mode(&dot.join("store.db")), 0o600);
    assert_eq!(
        mode(&settings),
        0o664,
        "committable settings stay untouched"
    );
    assert_eq!(mode(&rule), 0o664, "canonical rules stay untouched");
}

#[cfg(unix)]
#[test]
fn opening_repairs_an_existing_permissive_store_db() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let dot = root.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    let db = dot.join("store.db");
    drop(Connection::open(&db).unwrap());
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o666)).unwrap();

    drop(Store::open(root.path()).unwrap());
    assert_eq!(
        std::fs::symlink_metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn store_open_rejects_a_symlink_database_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let dot = root.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    let target = root.path().join("outside.db");
    std::fs::write(&target, b"outside stays byte-identical").unwrap();
    symlink(&target, dot.join("store.db")).unwrap();

    assert!(Store::open(root.path()).is_err());
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"outside stays byte-identical"
    );
}

#[cfg(unix)]
#[test]
fn private_state_creation_ignores_an_ambient_zero_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD: &str = "STELLA_PRIVATE_UMASK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("private_state_tests::private_state_creation_ignores_an_ambient_zero_umask")
            .arg("--nocapture")
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated zero-umask witness failed");
        return;
    }

    // Isolated child process: changing umask cannot race another test.
    unsafe { libc::umask(0) };
    let tmp = tempfile::tempdir().unwrap();
    let dot = tmp.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o777)).unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store
        .begin_execution("test", "private", "local", "test")
        .unwrap();

    let usage_dir = tmp.path().join("user-data");
    let usage_db = usage_dir.join("usage.db");
    drop(crate::usage::UsageStore::open_at(&usage_db).unwrap());
    let sessions_dir = tmp.path().join("sessions");
    let registry = crate::sessions::SessionRegistry::open(&sessions_dir);
    let record = crate::sessions::SessionRecord::new("/w", "private");
    registry.upsert(&record).unwrap();

    let mode = |path: &Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dot.join("store.db")), 0o600);
    for suffix in ["store.db-wal", "store.db-shm"] {
        let sidecar = dot.join(suffix);
        if sidecar.exists() {
            assert_eq!(mode(&sidecar), 0o600, "{} is private", sidecar.display());
        }
    }
    assert_eq!(mode(&usage_dir), 0o700);
    assert_eq!(mode(&usage_db), 0o600);
    assert_eq!(mode(&sessions_dir), 0o700);
    assert_eq!(
        mode(&sessions_dir.join(format!("{}.json", record.id))),
        0o600
    );
}
