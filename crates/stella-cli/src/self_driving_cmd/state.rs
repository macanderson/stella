//! The self-driving loop's durable state: `~/.stella/self-driving/<slug>/`.
//!
//! This module is the single writer of that directory (the shell driver
//! delegates here since #1548); the observatory reads it back read-only
//! (`stella-observatory/src/self_driving.rs`), so every shape written here is a
//! cross-process contract, not an implementation detail:
//!
//! - `ledger.jsonl` — one append per completed cycle.
//! - `runs.jsonl` — one append per run **state transition**; readers fold by
//!   `run_id`, last write winning. Append-only because the case it exists to
//!   record is a crash, and rewrite-in-place is the one structure a crash
//!   mid-write can corrupt.
//! - `run.json` — the single live pointer, rewritten atomically (tmp+rename)
//!   because the observatory polls it and a half-written JSON renders as a
//!   broken panel rather than the truth.
//! - `seen.txt`, `cycle`, `aperture`, `calibration.json`, `workspace.json` —
//!   small documents whose formats the shell driver established.
//!
//! The state follows the REPOSITORY, not the directory it is checked out in:
//! the slug comes from the remote URL (fallback: the shared git common dir),
//! so every worktree shares one ledger, seen-set, and calibration. Keying off
//! the checkout path would re-file every finding the main checkout already
//! triaged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};
use stella_core::self_driving::{AimdLimits, Calibration, CycleRecord, Floors};

use crate::timefmt::{now_unix, rfc3339_utc_now};

// ---------------------------------------------------------------------------
// Knobs — the same environment surface the shell driver answered to
// ---------------------------------------------------------------------------

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Two consecutive dry audits close an APERTURE, not the loop.
pub(crate) fn dry_streak_target() -> u32 {
    env_u64("SELF_DRIVING_DRY_STREAK", 2) as u32
}

/// A heartbeat older than this means nobody is driving the cycle any more.
/// Generous on purpose: a single model call inside an audit phase can
/// legitimately run for minutes, and calling a live cycle dead is worse than
/// noticing late.
pub(crate) fn stale_after_secs() -> i64 {
    env_u64("SELF_DRIVING_STALE_AFTER_SECS", 900) as i64
}

pub(crate) fn aimd_limits() -> AimdLimits {
    AimdLimits {
        batch_max: env_u64("SELF_DRIVING_BATCH_MAX", 20) as u32,
        batch_min: 2,
        parallel_max: env_u64("SELF_DRIVING_PARALLEL_MAX", 4) as u32,
    }
}

pub(crate) fn floors() -> Floors {
    Floors {
        disk_gb: env_u64("SELF_DRIVING_DISK_FLOOR_GB", 15),
        mem_gb: env_u64("SELF_DRIVING_MEM_FLOOR_GB", 4),
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

pub(crate) fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The workspace root: the enclosing git toplevel, else the cwd.
pub(crate) fn repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    git(&cwd, &["rev-parse", "--show-toplevel"]).map_or(cwd, PathBuf::from)
}

/// The repository's stable identity. The remote URL wins; the shared git
/// common dir is the fallback when there is no remote; the directory name is
/// the last resort.
pub(crate) fn repo_slug(root: &Path) -> String {
    if let Some(url) = git(root, &["config", "--get", "remote.origin.url"]) {
        let url = url.strip_suffix(".git").unwrap_or(&url);
        if let Some(base) = url.rsplit('/').next().filter(|s| !s.is_empty()) {
            return base.to_string();
        }
    }
    if let Some(common) = git(root, &["rev-parse", "--git-common-dir"]) {
        let common_path = if Path::new(&common).is_absolute() {
            PathBuf::from(&common)
        } else {
            root.join(&common)
        };
        if let Some(name) = common_path.parent().and_then(|p| p.file_name()) {
            return name.to_string_lossy().into_owned();
        }
    }
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string())
}

/// One loop's state directory, resolved and seeded.
pub(crate) struct LoopState {
    pub dir: PathBuf,
    pub repo_root: PathBuf,
}

impl LoopState {
    /// Resolve `$SELF_DRIVING_STATE_DIR`, else `<stella home>/self-driving/<slug>`,
    /// migrating a legacy home once rather than silently starting a fresh
    /// ledger — losing the seen-set would re-file every finding ever triaged.
    pub fn open() -> Result<Self, String> {
        let repo_root = repo_root();
        let dir = match std::env::var_os("SELF_DRIVING_STATE_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => {
                let home = stella_home::stella_home()
                    .ok_or_else(|| "cannot resolve the stella home directory".to_string())?;
                let slug = repo_slug(&repo_root);
                let dir = home.join("self-driving").join(&slug);
                migrate_legacy_state(&dir, &slug, &home);
                dir
            }
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create state dir {}: {e}", dir.display()))?;

        let state = Self { dir, repo_root };
        state.seed()?;
        state.record_workspace_root();
        Ok(state)
    }

    fn seed(&self) -> Result<(), String> {
        for file in ["ledger.jsonl", "seen.txt"] {
            let path = self.dir.join(file);
            if !path.exists() {
                write_file(&path, "")?;
            }
        }
        if !self.cycle_path().exists() {
            write_file(&self.cycle_path(), "0\n")?;
        }
        if !self.aperture_path().exists() {
            write_file(&self.aperture_path(), "rubric\n")?;
        }
        if !self.calibration_path().exists() {
            // Seed OPTIMISTICALLY (at the hard max): a ceiling that starts
            // low wastes a capable machine for a week; AIMD's decrease is
            // fast enough that starting high costs at most one degraded
            // cycle.
            self.write_calibration(&Calibration::seeded(&aimd_limits()))?;
        }
        Ok(())
    }

    /// Record every workspace root that has driven this loop. The observatory
    /// cannot run git, so it cannot re-derive the slug; the loop writes down
    /// its own roots and the dashboard matches on them. Worktrees
    /// legitimately add extra roots, so this is a bounded set, not a value.
    fn record_workspace_root(&self) {
        let path = self.dir.join("workspace.json");
        let mut doc = read_json(&path).unwrap_or_else(|| Value::Object(Map::new()));
        let Some(obj) = doc.as_object_mut() else {
            return;
        };
        let root = self.repo_root.to_string_lossy().into_owned();
        let mut roots: Vec<String> = obj
            .get("roots")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !roots.contains(&root) {
            roots.push(root);
        }
        let keep = roots.len().saturating_sub(32);
        let roots: Vec<String> = roots.into_iter().skip(keep).collect();
        obj.insert("slug".into(), Value::String(repo_slug(&self.repo_root)));
        obj.insert("roots".into(), serde_json::json!(roots));
        let _ = write_json_atomic(&path, &doc);
    }

    // -- paths --------------------------------------------------------------

    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }
    fn seen_path(&self) -> PathBuf {
        self.dir.join("seen.txt")
    }
    fn cycle_path(&self) -> PathBuf {
        self.dir.join("cycle")
    }
    fn calibration_path(&self) -> PathBuf {
        self.dir.join("calibration.json")
    }
    fn aperture_path(&self) -> PathBuf {
        self.dir.join("aperture")
    }
    fn run_path(&self) -> PathBuf {
        self.dir.join("run.json")
    }
    fn runs_path(&self) -> PathBuf {
        self.dir.join("runs.jsonl")
    }

    // -- the ledger ---------------------------------------------------------

    pub fn cycles(&self) -> Vec<CycleRecord> {
        read_jsonl(&self.ledger_path())
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    pub fn append_cycle(&self, rec: &CycleRecord) -> Result<(), String> {
        let line = serde_json::to_string(rec).map_err(|e| e.to_string())?;
        append_line(&self.ledger_path(), &line)
    }

    // -- the counter and the aperture ---------------------------------------

    pub fn cycle_counter(&self) -> u64 {
        std::fs::read_to_string(self.cycle_path())
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn set_cycle_counter(&self, n: u64) -> Result<(), String> {
        write_file(&self.cycle_path(), &format!("{n}\n"))
    }

    pub fn aperture(&self) -> String {
        std::fs::read_to_string(self.aperture_path())
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "rubric".to_string())
    }

    pub fn set_aperture(&self, lens: &str) -> Result<(), String> {
        write_file(&self.aperture_path(), &format!("{lens}\n"))
    }

    // -- the controller -----------------------------------------------------

    pub fn calibration(&self) -> Calibration {
        std::fs::read_to_string(self.calibration_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Calibration::seeded(&aimd_limits()))
    }

    pub fn write_calibration(&self, cal: &Calibration) -> Result<(), String> {
        let text = serde_json::to_string_pretty(cal).map_err(|e| e.to_string())?;
        write_file(&self.calibration_path(), &text)
    }

    // -- the seen-set -------------------------------------------------------

    pub fn seen(&self) -> Vec<String> {
        std::fs::read_to_string(self.seen_path())
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn seen_count(&self) -> usize {
        self.seen().iter().filter(|l| !l.trim().is_empty()).count()
    }

    pub fn add_seen(&self, digest: &str) -> Result<(), String> {
        append_line(&self.seen_path(), digest)
    }

    // -- the run lifecycle ---------------------------------------------------

    pub fn run_doc(&self) -> Option<Value> {
        read_json(&self.run_path())
    }

    pub fn current_run_id(&self) -> Option<String> {
        self.run_doc()?
            .get("run_id")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    pub fn update_run_doc(&self, mutate: impl FnOnce(&mut Map<String, Value>)) {
        let mut doc = self
            .run_doc()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        mutate(&mut doc);
        let _ = write_json_atomic(&self.run_path(), &Value::Object(doc));
    }

    pub fn clear_run_doc(&self) {
        let _ = std::fs::remove_file(self.run_path());
    }

    /// Rewrite the live pointer for a phase boundary — what turns "something
    /// is running" into "something is running, and it is in the audit stage
    /// of cycle 12".
    pub fn run_write(&self, phase: &str, cycle: Option<u64>, tier: Option<&str>) {
        let aperture = self.aperture();
        let driver =
            std::env::var("SELF_DRIVING_DRIVER").unwrap_or_else(|_| "interactive".to_string());
        let now = rfc3339_utc_now();
        self.update_run_doc(|doc| {
            if let Some(c) = cycle {
                doc.insert("cycle".into(), c.into());
            }
            if let Some(t) = tier {
                if !t.is_empty() {
                    doc.insert("tier".into(), t.into());
                }
            }
            doc.entry("started_at".to_string())
                .or_insert_with(|| Value::String(now.clone()));
            doc.insert("phase".into(), phase.into());
            doc.insert("aperture".into(), aperture.into());
            doc.insert("driver".into(), driver.into());
            doc.insert("pid".into(), u64::from(std::process::id()).into());
            doc.insert("host".into(), hostname().into());
            doc.insert("heartbeat".into(), now.clone().into());
            doc.insert("heartbeat_unix".into(), now_unix().into());
        });
    }

    /// Append one run state transition. NOT best-effort: this append IS the
    /// run history, and swallowing its failure while the caller clears the
    /// live pointer leaves the run reading `crashed` to every reader forever.
    pub fn append_run_record(&self, fields: BTreeMap<String, Value>) -> Result<Value, String> {
        let mut rec = BTreeMap::new();
        rec.insert("at".to_string(), Value::String(rfc3339_utc_now()));
        rec.insert("at_unix".to_string(), now_unix().into());
        rec.insert("host".to_string(), hostname().into());
        rec.extend(fields);
        let value = serde_json::to_value(&rec).map_err(|e| e.to_string())?;
        let line = serde_json::to_string(&rec).map_err(|e| e.to_string())?;
        append_line(&self.runs_path(), &line).map_err(|e| {
            format!("FAILED to append the run record — the run history is now incomplete: {e}")
        })?;
        Ok(value)
    }

    pub fn run_records(&self) -> Vec<Value> {
        read_jsonl(&self.runs_path())
    }

    /// Bank a run record left behind by a driver that died, so the history
    /// records the abandonment rather than the cycle silently disappearing.
    ///
    /// The witness is the HEARTBEAT, not the pid: every subcommand here is
    /// its own short-lived process, so the recorded pid is dead microseconds
    /// after it is written — checking it would declare every interactive run
    /// crashed on its very next command. A pid is only meaningful when a
    /// supervisor owns the run, so it is consulted for `driver=daemon` and
    /// ignored otherwise.
    pub fn reconcile_abandoned_run(&self) {
        let Some(doc) = self.run_doc() else { return };
        let rid = doc
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if rid.is_empty() {
            self.clear_run_doc();
            return;
        }

        let age = now_unix()
            - doc
                .get("heartbeat_unix")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        let driver = doc.get("driver").and_then(Value::as_str).unwrap_or("");
        if age < stale_after_secs() {
            if driver != "daemon" {
                return;
            }
            let pid = doc.get("pid").and_then(Value::as_i64).unwrap_or(0);
            if pid > 0 && process_alive(pid) {
                return;
            }
        }

        let mut fields = BTreeMap::new();
        fields.insert("run_id".to_string(), Value::String(rid.clone()));
        fields.insert("status".to_string(), Value::String("crashed".to_string()));
        fields.insert(
            "reason".to_string(),
            Value::String("driver exited without ending the run".to_string()),
        );
        let _ = self.append_run_record(fields);
        eprintln!("  ! previous run {rid} was abandoned — banked as crashed");
        self.clear_run_doc();
    }
}

/// Two earlier builds kept this state elsewhere, newest first:
///
/// 1. `<stella home>/fullauto/<slug>` — before the loop was renamed
///    `self-driving`. The spelling is a fact about bytes already on disk, so it
///    survives the rename verbatim; rewriting it here would point the migration
///    at a directory that has never existed and silently orphan a live ledger.
/// 2. `~/.fullauto/<slug>` — the original home, before the state moved under
///    the stella home.
///
/// Move the first one that exists, once. Never overwrite: if both exist the new
/// home wins and the legacy directory is left untouched for the user to inspect.
fn migrate_legacy_state(new_dir: &Path, slug: &str, stella_home: &Path) {
    if new_dir.exists() {
        return;
    }
    let mut candidates = vec![stella_home.join("fullauto").join(slug)];
    // Through `crate::paths`, never `var_os("HOME")` — that indirection is
    // what lets a test redirect the anchor without mutating process-global
    // state (#1139), and `nothing_else_in_this_crate_reads_a_home_out_of_the_environment`
    // enforces it.
    if let Some(home) = crate::paths::home() {
        candidates.push(home.join(".fullauto").join(slug));
    }

    let Some(legacy) = candidates.into_iter().find(|c| c.is_dir()) else {
        return;
    };
    if let Some(parent) = new_dir.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && std::fs::rename(&legacy, new_dir).is_ok()
    {
        eprintln!(
            "  ! migrated self-driving state: {} -> {}",
            legacy.display(),
            new_dir.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Small file helpers
// ---------------------------------------------------------------------------

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Temp-file-plus-rename, because `run.json` is polled by the observatory
/// and a half-written document must never be observable.
fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("cannot append to {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Parse a `.jsonl` file, skipping unparseable lines — a truncated final line
/// is the normal state of a file being appended to while it is read.
fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .filter(Value::is_object)
                .collect()
        })
        .unwrap_or_default()
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn process_alive(pid: i64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A new run id: `r-<utc-compact>-<4 hex>`, sortable and unique enough for a
/// per-machine ledger.
pub(crate) fn new_run_id() -> String {
    let compact: String = rfc3339_utc_now()
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let salt = (nanos ^ std::process::id()) & 0xffff;
    format!("r-{compact}-{salt:04x}")
}
