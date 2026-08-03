//! End-to-end witness for `stella doctor`: drive the real binary against a
//! deliberately corrupted store and pin the contract a script depends on — the
//! exit code, the diagnosis on stdout, and the fact that nothing moves until
//! `--repair` is typed.
//!
//! The verdict logic itself is covered by `stella-store`'s integrity tests and
//! the module tests in `doctor.rs`; what only a process can prove is that the
//! command is reachable without a provider or API key configured, and that
//! failure leaves the process exiting non-zero.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use stella_store::Store;

/// A workspace with a real, cleanly closed `store.db` holding one execution.
fn workspace_with_store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "keep the lights on", "zai", "glm-5.2")
        .expect("execution");
    store
        .finish_execution(id, "completed", 0.25)
        .expect("finish");
    drop(store);
    dir
}

fn store_db(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(".stella/private/store.db")
}

/// Overwrite the SQLite header: the file is no longer a database at all.
fn corrupt(db_path: &Path) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(db_path)
        .expect("open db for corruption");
    file.write_all(&[0x7f; 128]).expect("write garbage");
    file.sync_all().expect("sync");
}

/// Run `stella doctor …` in `dir`. No provider or key is configured on purpose —
/// a corrupt store must be diagnosable in exactly that state. `NO_COLOR` keeps
/// the assertions off ANSI escapes.
///
/// HOME and the data dir are pointed at scratch: the `model config` check
/// (#895) reads the user settings scope and the on-disk model catalog, so
/// without this the store assertions below would pass or fail depending on
/// which models the developer happens to have pinned. `model_config_cli.rs`
/// owns that check's own assertions.
fn doctor(dir: &tempfile::TempDir, args: &[&str]) -> Output {
    let home = dir.path().join("scratch-home");
    std::fs::create_dir_all(&home).expect("scratch home");
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .arg("doctor")
        .args(args)
        .current_dir(dir.path())
        .env("HOME", &home)
        .env("STELLA_DATA_DIR", home.join("data"))
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env("STELLA_MANAGED_SETTINGS", home.join("no-managed.json"))
        .env_remove("STELLA_HOME")
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_BASE_URL")
        .env_remove("STELLA_BUDGET")
        .output()
        .expect("run stella doctor")
}

#[test]
fn doctor_reports_a_healthy_store_and_exits_zero() {
    let dir = workspace_with_store();
    let output = doctor(&dir, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a healthy workspace exits 0: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("store integrity") && stdout.contains("quick_check"),
        "the check is named and says what it ran: {stdout}"
    );
    assert!(
        stdout.contains("fleet ledger"),
        "the fleet-ledger check is reported too: {stdout}"
    );
    assert!(
        stdout.contains("session sidecars"),
        "the sidecar-orphan check is reported too: {stdout}"
    );
    assert!(
        stdout.contains("model config"),
        "the model-config check is reported too: {stdout}"
    );
    assert!(stdout.contains("4 ok, 0 failed"), "{stdout}");
}

#[test]
fn doctor_detects_a_corrupt_store_exits_one_and_touches_nothing() {
    let dir = workspace_with_store();
    let db_path = store_db(&dir);
    corrupt(&db_path);
    let before = std::fs::read(&db_path).expect("read corrupt db");

    let output = doctor(&dir, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a corrupt store fails the command: {stdout}"
    );
    assert!(
        stdout.contains(".stella/private/store.db"),
        "the report names the file: {stdout}"
    );
    assert!(
        stdout.contains("not readable as a SQLite database"),
        "the report states the diagnosis: {stdout}"
    );
    assert!(
        stdout.contains("--repair") && stdout.contains(".recover"),
        "the report offers both repair paths: {stdout}"
    );
    assert_eq!(
        std::fs::read(&db_path).expect("read db"),
        before,
        "diagnosing must not modify the file"
    );
}

#[test]
fn doctor_repair_quarantines_the_store_and_the_next_check_passes() {
    let dir = workspace_with_store();
    let db_path = store_db(&dir);
    corrupt(&db_path);
    let corrupt_bytes = std::fs::read(&db_path).expect("read corrupt db");

    let repaired = doctor(&dir, &["--repair"]);
    let stdout = String::from_utf8_lossy(&repaired.stdout);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "a completed repair leaves the workspace healthy: {stdout}"
    );
    assert!(
        stdout.contains("quarantined") && stdout.contains("nothing was deleted"),
        "the repair says what it did: {stdout}"
    );
    assert!(!db_path.exists(), "the corrupt file was moved aside");

    // The corrupt bytes are still on disk under a timestamped name.
    let backup = std::fs::read_dir(db_path.parent().expect("private dir"))
        .expect("read private dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("store.db.corrupt-"))
        })
        .expect("timestamped backup");
    assert_eq!(
        std::fs::read(&backup).expect("read backup"),
        corrupt_bytes,
        "the backup is byte-for-byte what the user had"
    );

    // Re-running the plain check now passes: the workspace is usable again.
    let again = doctor(&dir, &[]);
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
}
