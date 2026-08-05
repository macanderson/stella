//! `stella dataset export` — a curated, redacted trajectory dataset from the
//! local receipt store (#872, the first slice of #836's weight-space track).
//!
//! One JSONL record per **accepted** turn — the prompt, the tool calls with
//! their arguments and outputs, the change that landed, and the judge's
//! verdict — beside a `manifest.json` that states exactly which filter
//! produced it. That manifest is the point: a dataset nobody can re-derive
//! the extraction rule for is not auditable, and #836 requires human sign-off
//! before any of this reaches a training run.
//!
//! ## A fold over the store, not a second capture path
//!
//! `trace.rs` (#1042) already assembles a redacted trajectory record — but
//! `trace_capture` defaults off and is wired at exactly one call site, so
//! `.stella/private/traces.jsonl` is absent in essentially every real
//! workspace and carries nothing historical. This module therefore folds
//! `store.db` directly: the `executions` header for provenance and the
//! per-execution `events` journal for everything else. That journal is the
//! only place tool OUTPUTS survive (`tool_calls` records `bytes_out` and
//! deliberately never the content), so it is the only possible source.
//!
//! ## "Accepted" is a judgement, not a column
//!
//! The store has no accepted flag. The settled signals are
//! `executions.outcome` — whose observed vocabulary is `completed`,
//! `goal_met`, `goal_unmet`, `verification_failed`, `aborted`, `cancelled`,
//! `failed`, `error`, `interrupted` — and `AgentEvent::JudgeVerdict`. So the
//! predicate is named ([`is_accepted`]), documented, and echoed **verbatim**
//! into the manifest, rather than left implicit in a `WHERE` clause. No
//! numeric reward is invented: nothing in the tree maps a ladder decision to
//! a reward scalar yet (#1043 is open), so the raw verdict is emitted and
//! scoring is left to whoever gets there.
//!
//! ## What a "diff" can honestly be here
//!
//! There is no unified diff anywhere in `store.db`. `files_touched` holds
//! path/ops/line-counts and no content; `AgentEvent::FileChange::diff` is one
//! coarse changed-region hunk, bounded at 200 lines per side by the recorder.
//! [`DatasetChange`] therefore carries the recorder's authoritative
//! `added`/`removed` counts, that bounded hunk, and a `diff_truncated` flag —
//! and the exact bytes the model produced are still recoverable from the
//! edit/write tool arguments in `tool_calls`. A field promising a full diff
//! would be lying.
//!
//! ## Privacy and determinism
//!
//! Every string leaf of every record and of the manifest passes through
//! [`stella_core::redact::redact_secrets`] AFTER assembly, so a field added
//! later cannot route around it. Output is written 0600 into a 0700
//! directory: redacted prompts plus full tool outputs are at least as
//! sensitive as the session archive that was hardened for the same reason.
//! Nothing here reads a clock — the manifest's watermark comes from the store
//! — and every ordering is a stable key, so the same store and filter produce
//! byte-identical files on every run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stella_core::redact::{PLACEHOLDER, redact_secrets};
use stella_protocol::AgentEvent;
use stella_store::{FinishedExecution, Store};

/// Bump when [`DatasetRecord`]'s shape changes incompatibly, so a reader can
/// refuse a dataset it does not understand instead of misparsing it.
pub const DATASET_SCHEMA_VERSION: u32 = 1;

/// The dataset file written under `--output`.
pub const DATASET_FILE: &str = "dataset.jsonl";

/// The manifest written beside it.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Terminal outcomes that count as the turn having succeeded. `goal_unmet`,
/// `verification_failed`, `aborted`, `cancelled`, `failed`, `error` and
/// `interrupted` are all excluded — a training set built from turns that did
/// not land teaches the wrong thing.
const ACCEPTED_OUTCOMES: &[&str] = &["completed", "goal_met"];

/// How many finished executions are read per cursor advance. The store's
/// reader is limit-bounded, so the fold pages rather than assuming a ceiling.
const SCAN_BATCH: u32 = 500;

/// Whether one settled execution is an *accepted turn*.
///
/// Two conditions, both necessary:
///
/// 1. `outcome` is one of [`ACCEPTED_OUTCOMES`] — the turn reported success;
/// 2. the journal holds at least one **mutating** `FileChange` — the turn
///    actually changed the workspace. A successful read-only turn is a real
///    outcome but not a trajectory anything can be distilled from: #872 asks
///    for "prompt → tool calls → the accepted diff", and there is no diff.
///
/// With `require_verdict`, a third: a `JudgeVerdict` whose `passed` is true.
/// That is off by default because most executions never reach a judge at all
/// (the deterministic ladder can fast-submit), so requiring it would silently
/// shrink the dataset to the subset that escalated.
fn is_accepted(outcome: &str, fold: &JournalFold, require_verdict: bool) -> bool {
    ACCEPTED_OUTCOMES.contains(&outcome)
        && !fold.changes.is_empty()
        && (!require_verdict || fold.verdict.as_ref().is_some_and(|v| v.passed))
}

/// The predicate [`is_accepted`] applies, in words, for the manifest. Stated
/// rather than described: a reader has to be able to reproduce the selection
/// without reading this file.
fn acceptance_predicate(require_verdict: bool) -> String {
    let base = format!(
        "executions.outcome in {{{}}} AND at least one mutating file_change event",
        ACCEPTED_OUTCOMES.join(", ")
    );
    if require_verdict {
        format!("{base} AND a judge_verdict event with passed=true")
    } else {
        base
    }
}

/// Output shape for `--format`. One variant today; it is an enum so the flag
/// is validated at parse time and a second format is additive.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetFormat {
    /// One JSON record per line — what a training loop reads.
    Jsonl,
}

impl DatasetFormat {
    fn wire(self) -> &'static str {
        match self {
            DatasetFormat::Jsonl => "jsonl",
        }
    }
}

/// Subcommands under `stella dataset`.
#[derive(Subcommand)]
pub enum DatasetCmd {
    /// Write every accepted turn as a redacted training record, plus a
    /// manifest describing the filter that selected them.
    Export {
        /// Record encoding.
        #[arg(long, value_enum, default_value_t = DatasetFormat::Jsonl)]
        format: DatasetFormat,
        /// Directory to write `dataset.jsonl` and `manifest.json` into.
        /// Created owner-only if missing.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
        /// Keep only turns started at or after this timestamp. Compared
        /// lexicographically against `executions.started_at`, which SQLite
        /// writes as UTC `YYYY-MM-DD HH:MM:SS`, so a bare date works.
        #[arg(long, value_name = "TIMESTAMP")]
        since: Option<String>,
        /// Keep only turns started strictly before this timestamp (same
        /// comparison as `--since`; the window is half-open).
        #[arg(long, value_name = "TIMESTAMP")]
        until: Option<String>,
        /// Additionally require a judge verdict of `passed`. Off by default:
        /// a turn the deterministic ladder cleared never reaches a judge.
        #[arg(long)]
        require_verdict: bool,
    },
}

/// One accepted turn, ready for a training loop.
///
/// Every field is always present — no `skip_serializing_if` anywhere — so
/// each line of the dataset carries the same keys and a consumer never has to
/// distinguish "absent" from "not applicable". Absence is spelled `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetRecord {
    pub schema: u32,
    // ---- provenance (#872 acceptance: session_id, repo, timestamp,
    // turn_instance) --------------------------------------------------------
    pub execution_id: i64,
    /// The chat session this turn belonged to. `None` for a one-shot
    /// `stella run`, which never stamps one — that is a fact about the turn,
    /// not missing data.
    pub session_id: Option<String>,
    /// [`stella_store::identity::TelemetryScope::repo_id`] — an FNV hash of
    /// the normalized `origin` URL, so the dataset is attributable to a repo
    /// without carrying the URL. Degrades to the path-derived project id when
    /// the workspace has no remote.
    pub repo: String,
    /// `executions.started_at`, UTC.
    pub timestamp: String,
    /// The sub-turn the accepted change landed in — the last `turn_instance`
    /// the journal reported. `0` for an execution with no receipts, which is
    /// also the value a single-turn execution genuinely has.
    pub turn_instance: u32,
    // ---- what ran ---------------------------------------------------------
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub outcome: String,
    pub cost_usd: f64,
    // ---- the trajectory ---------------------------------------------------
    /// The prompt as submitted, before the context engine expanded it.
    pub prompt: String,
    /// Every tool invocation, in journal order.
    pub tool_calls: Vec<DatasetToolCall>,
    /// Every mutating file change, in journal order.
    pub changes: Vec<DatasetChange>,
    /// The last judge verdict, when the turn reached one.
    pub verdict: Option<DatasetVerdict>,
    /// Whether redaction actually replaced something anywhere in this record.
    /// Recorded so "this was redacted" is a visible fact rather than an
    /// assumption (the `stella_core::redact` posture).
    pub redacted: bool,
}

/// One tool invocation: what the model asked for and what came back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetToolCall {
    /// The journal `seq` of the `tool_start`, so a consumer can re-order or
    /// interleave against the raw stream.
    pub seq: i64,
    pub turn_instance: u32,
    pub name: String,
    pub call_id: String,
    /// The full arguments the model produced — the same value `tool_calls`
    /// projects `args_json` from.
    pub arguments: serde_json::Value,
    /// The result text as the model read it, or the error message it was
    /// asked to retry against. `None` when the journal holds no matching
    /// `tool_result` (an interrupted turn).
    pub output: Option<String>,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
}

/// One mutating file change.
///
/// `added`/`removed` are the recorder's own counts and are authoritative;
/// they must NOT be re-derived by counting `+`/`-` lines in `diff`, which is a
/// bounded changed-region hunk rather than a full unified diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetChange {
    pub path: String,
    /// `created` | `modified` | `deleted` — reads never appear here.
    pub kind: String,
    pub added: u32,
    pub removed: u32,
    /// The recorder's changed-region hunk, when it captured one.
    pub diff: Option<String>,
    /// `true` when the hunk demonstrably shows fewer changed lines than the
    /// recorder counted, i.e. the 200-lines-per-side bound cut it. One-way:
    /// `false` means "no evidence of truncation", not "provably complete".
    pub diff_truncated: bool,
}

/// A judge verdict, carried raw. No reward scalar is derived from it: nothing
/// in the tree maps a ladder decision onto one yet (#1043).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetVerdict {
    pub passed: bool,
    /// `true` when the verdict came from the deterministic ladder rather than
    /// a model judge — never conflate the two.
    pub deterministic: bool,
    pub summary: String,
}

/// The dataset's provenance document: what was extracted, by what rule, with
/// which redactor, from a store at which point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub schema: u32,
    pub format: String,
    pub file: String,
    pub records: u64,
    pub repo: String,
    pub filter: DatasetFilter,
    pub redaction: RedactionStamp,
    /// The store's own newest timestamp — the "as of" of this extraction.
    /// Read from the store rather than the clock so re-running the same
    /// extraction produces the same manifest.
    pub store_watermark: Option<String>,
    /// `[lowest, highest]` accepted execution id, or `None` for an empty
    /// dataset.
    pub execution_id_range: Option<[i64; 2]>,
}

/// The extraction filter, stated so the selection is reproducible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetFilter {
    /// [`acceptance_predicate`], verbatim.
    pub acceptance_predicate: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub require_verdict: bool,
    /// Settled executions read from the store.
    pub executions_scanned: u64,
    /// Of those, how many fell inside the `since`/`until` window.
    pub executions_in_window: u64,
    /// Of those, how many the predicate accepted. Equals `records`.
    pub executions_accepted: u64,
}

/// Which redactor produced this dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionStamp {
    pub function: String,
    pub placeholder: String,
    /// A digest over the redactor's decisions on a fixed probe corpus (see
    /// [`redaction_behavior_digest`]).
    pub behavior_digest: String,
}

/// `stella dataset <cmd>`.
pub fn run_dataset(cmd: &DatasetCmd) -> Result<(), String> {
    match cmd {
        DatasetCmd::Export {
            format,
            output,
            since,
            until,
            require_verdict,
        } => run_export(&ExportArgs {
            format: *format,
            output: output.clone(),
            since: since.clone(),
            until: until.clone(),
            require_verdict: *require_verdict,
        }),
    }
}

/// The parsed `stella dataset export` request.
struct ExportArgs {
    format: DatasetFormat,
    output: PathBuf,
    since: Option<String>,
    until: Option<String>,
    require_verdict: bool,
}

fn run_export(args: &ExportArgs) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = open_readonly_store(&workspace_root)?;
    let repo = stella_store::identity::TelemetryScope::resolve(&workspace_root).repo_id;

    let (records, filter) = extract(&store, &repo, args)?;
    let manifest = DatasetManifest {
        schema: DATASET_SCHEMA_VERSION,
        format: args.format.wire().to_string(),
        file: DATASET_FILE.to_string(),
        records: records.len() as u64,
        repo,
        filter,
        redaction: RedactionStamp {
            function: "stella_core::redact::redact_secrets".to_string(),
            placeholder: PLACEHOLDER.to_string(),
            behavior_digest: redaction_behavior_digest(),
        },
        store_watermark: store
            .last_log_timestamp()
            .map_err(|e| format!("cannot read store watermark: {e}"))?,
        execution_id_range: match (records.first(), records.last()) {
            (Some(first), Some(last)) => Some([first.execution_id, last.execution_id]),
            _ => None,
        },
    };

    let (dataset_path, manifest_path) = write_dataset(&args.output, &records, &manifest)?;
    report(&records, &manifest, &dataset_path, &manifest_path);
    Ok(())
}

/// Open the workspace store without creating it. Mirrors `stella inspect`: a
/// question about recorded history must never write one into existence.
fn open_readonly_store(workspace_root: &Path) -> Result<Store, String> {
    let db_path = stella_store::existing_workspace_private_sqlite_path(workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?;
    if db_path.is_none() {
        return Err(
            "no local store in this workspace yet — run something first, then export it".into(),
        );
    }
    Store::open(workspace_root).map_err(|e| format!("cannot open local store: {e}"))
}

/// Fold the whole store into the accepted, redacted records — ascending by
/// execution id, which is the dataset's stable order.
fn extract(
    store: &Store,
    repo: &str,
    args: &ExportArgs,
) -> Result<(Vec<DatasetRecord>, DatasetFilter), String> {
    let mut records: Vec<DatasetRecord> = Vec::new();
    let (mut scanned, mut in_window) = (0u64, 0u64);
    let mut cursor = 0i64;
    loop {
        let batch = store
            .finished_executions_after(cursor, SCAN_BATCH)
            .map_err(|e| format!("cannot read executions: {e}"))?;
        if batch.is_empty() {
            break;
        }
        for execution in &batch {
            cursor = execution.execution_id;
            scanned += 1;
            let Some(record) = accepted_record(store, execution, repo, args, &mut in_window)?
            else {
                continue;
            };
            records.push(record);
        }
        if batch.len() < SCAN_BATCH as usize {
            break;
        }
    }

    let filter = DatasetFilter {
        acceptance_predicate: acceptance_predicate(args.require_verdict),
        since: args.since.clone(),
        until: args.until.clone(),
        require_verdict: args.require_verdict,
        executions_scanned: scanned,
        executions_in_window: in_window,
        executions_accepted: records.len() as u64,
    };
    Ok((records, filter))
}

/// One execution's record, or `None` when the window or the predicate excludes
/// it. Increments `in_window` for every execution the date filter admits.
fn accepted_record(
    store: &Store,
    execution: &FinishedExecution,
    repo: &str,
    args: &ExportArgs,
    in_window: &mut u64,
) -> Result<Option<DatasetRecord>, String> {
    let id = execution.execution_id;
    let Some(summary) = store
        .execution_summary(id)
        .map_err(|e| format!("cannot read execution {id}: {e}"))?
    else {
        // The row vanished between the two reads (a concurrent prune). Not an
        // error: the dataset simply does not contain it.
        return Ok(None);
    };
    if !in_range(
        &summary.started_at,
        args.since.as_deref(),
        args.until.as_deref(),
    ) {
        return Ok(None);
    }
    *in_window += 1;

    let journal = store
        .execution_events(id)
        .map_err(|e| format!("cannot read journal for execution {id}: {e}"))?;
    let fold = fold_journal(&journal.events);
    if !is_accepted(&execution.outcome, &fold, args.require_verdict) {
        return Ok(None);
    }

    let record = DatasetRecord {
        schema: DATASET_SCHEMA_VERSION,
        execution_id: id,
        session_id: execution.session_id.clone(),
        repo: repo.to_string(),
        timestamp: summary.started_at,
        turn_instance: fold.turn_instance,
        kind: summary.kind,
        provider: summary.provider,
        model: summary.model,
        outcome: execution.outcome.clone(),
        cost_usd: summary.cost_usd,
        prompt: summary.prompt,
        tool_calls: fold.tool_calls,
        changes: fold.changes,
        verdict: fold.verdict,
        redacted: false,
    };
    Ok(Some(redact_after_assembly(&record)?))
}

/// Half-open `[since, until)` on the timestamp string. Both bounds are
/// lexicographic, which is exact for the `YYYY-MM-DD HH:MM:SS` form SQLite's
/// `CURRENT_TIMESTAMP` writes and lets a bare date stand for its midnight.
fn in_range(started_at: &str, since: Option<&str>, until: Option<&str>) -> bool {
    since.is_none_or(|s| started_at >= s) && until.is_none_or(|u| started_at < u)
}

/// What one ordered pass over an execution's journal recovers.
#[derive(Default)]
struct JournalFold {
    tool_calls: Vec<DatasetToolCall>,
    changes: Vec<DatasetChange>,
    verdict: Option<DatasetVerdict>,
    turn_instance: u32,
}

fn fold_journal(events: &[stella_store::SessionEventRecord]) -> JournalFold {
    let mut fold = JournalFold::default();
    // call_id → index into `fold.tool_calls`, so a result closes the call that
    // opened it rather than the most recent one (tools can overlap).
    let mut open: HashMap<String, usize> = HashMap::new();
    for record in events {
        match &record.event {
            AgentEvent::StepManifest { turn_instance, .. } => {
                fold.turn_instance = *turn_instance;
            }
            AgentEvent::ToolStart { call } => {
                open.insert(call.call_id.clone(), fold.tool_calls.len());
                fold.tool_calls.push(DatasetToolCall {
                    seq: record.seq,
                    turn_instance: fold.turn_instance,
                    name: call.name.clone(),
                    call_id: call.call_id.clone(),
                    arguments: call.input.clone(),
                    output: None,
                    is_error: None,
                    duration_ms: None,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                ..
            } => {
                // A result with no start is a journal that begins mid-turn;
                // there is no call to attach it to, so it is dropped rather
                // than invented.
                if let Some(index) = open.remove(call_id)
                    && let Some(entry) = fold.tool_calls.get_mut(index)
                {
                    entry.output = Some(tool_text(output));
                    entry.is_error = Some(output.is_error());
                    entry.duration_ms = Some(*duration_ms);
                }
            }
            AgentEvent::FileChange {
                path,
                kind,
                added,
                removed,
                diff,
            } if kind.is_mutation() => {
                fold.changes.push(DatasetChange {
                    path: path.clone(),
                    kind: wire_token(kind),
                    added: *added,
                    removed: *removed,
                    diff: diff.clone(),
                    diff_truncated: hunk_is_truncated(diff.as_deref(), *added, *removed),
                });
            }
            AgentEvent::JudgeVerdict { passed, evidence } => {
                fold.verdict = Some(DatasetVerdict {
                    passed: *passed,
                    deterministic: evidence.deterministic,
                    summary: evidence.summary.clone(),
                });
            }
            _ => {}
        }
    }
    fold
}

/// The text a tool returned, whichever arm it took — the model read one or
/// the other, and a training record should see the same thing it did.
fn tool_text(output: &stella_protocol::ToolOutput) -> String {
    match output {
        stella_protocol::ToolOutput::Ok { content } => content.clone(),
        stella_protocol::ToolOutput::Error { message } => message.clone(),
    }
}

/// Did the recorder's per-side line bound cut this hunk?
///
/// Inferred by comparison, never by re-deriving the counts: if the hunk shows
/// fewer `+` lines than the recorder counted added (or fewer `-` than
/// removed), lines are missing from the rendering. The converse does not hold
/// — a changed-region hunk can render unchanged lines on both sides — so this
/// is one-directional and `false` claims only "no evidence".
fn hunk_is_truncated(diff: Option<&str>, added: u32, removed: u32) -> bool {
    let Some(diff) = diff else { return false };
    let (mut plus, mut minus) = (0u32, 0u32);
    for line in diff.lines() {
        match line.as_bytes().first() {
            Some(b'+') if !line.starts_with("+++") => plus += 1,
            Some(b'-') if !line.starts_with("---") => minus += 1,
            _ => {}
        }
    }
    plus < added || minus < removed
}

/// A protocol enum's wire token (its `snake_case` serde name), so the dataset
/// and the event stream share one vocabulary.
fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_string(),
    }
}

/// Redact every string leaf of an already-assembled value, then rebuild the
/// typed record from the redacted tree.
///
/// The round trip is deliberate, and it buys two things a field-by-field
/// redaction cannot. First, redaction happens *after* assembly over the whole
/// tree, so a field added to [`DatasetRecord`] later is covered without anyone
/// remembering to route it through. Second, the value finally serialized is
/// the STRUCT, so the emitted key order is declaration order — not
/// `serde_json::Map`'s, whose ordering depends on whether the `preserve_order`
/// feature is enabled somewhere in the dependency graph. Byte-identity across
/// runs must not be hostage to a transitive feature flag.
fn redact_after_assembly<T>(value: &T) -> Result<T, String>
where
    T: Serialize + serde::de::DeserializeOwned + WithRedactionFlag,
{
    let mut tree =
        serde_json::to_value(value).map_err(|e| format!("cannot serialize record: {e}"))?;
    let mut redacted = false;
    redact_json_strings(&mut tree, &mut redacted);
    let mut back: T =
        serde_json::from_value(tree).map_err(|e| format!("cannot rebuild record: {e}"))?;
    back.set_redacted(redacted);
    Ok(back)
}

/// Lets [`redact_after_assembly`] record whether anything was replaced on the
/// value it just rebuilt. Implemented as a trait rather than a return tuple so
/// the flag cannot be computed and then dropped on the floor by a caller.
trait WithRedactionFlag {
    fn set_redacted(&mut self, redacted: bool);
}

impl WithRedactionFlag for DatasetRecord {
    fn set_redacted(&mut self, redacted: bool) {
        self.redacted = redacted;
    }
}

impl WithRedactionFlag for DatasetManifest {
    /// The manifest has no flag of its own — it describes the redactor rather
    /// than carrying redacted content — but it still goes through the same
    /// walk, because `--since`/`--until` are user-supplied strings and the
    /// dataset's guarantee is about the OUTPUT, not about one file of it.
    fn set_redacted(&mut self, _redacted: bool) {}
}

/// Replace every recognized secret in every string leaf, recording whether
/// anything actually changed.
fn redact_json_strings(value: &mut serde_json::Value, redacted: &mut bool) {
    match value {
        serde_json::Value::String(text) => {
            let redaction = redact_secrets(text);
            if redaction.redacted {
                *redacted = true;
                *text = redaction.text;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_strings(item, redacted);
            }
        }
        serde_json::Value::Object(map) => {
            for (_key, item) in map.iter_mut() {
                redact_json_strings(item, redacted);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Probes the redactor is fingerprinted over — one per rule it implements,
/// plus the negatives it must leave alone. Two datasets whose manifests agree
/// on the digest were redacted by a redactor that agrees on all of these.
const REDACTION_PROBES: &[&str] = &[
    // Rule 2: vendor prefix, JWT shape, high-entropy blob.
    "ghp_0123456789abcdef0123456789abcdef012345",
    "sk-ant-api03-0123456789abcdefghijklmnopqrstuvwxyz",
    "AKIAIOSFODNN7EXAMPLE",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    "Zm9vYmFyQmF6MDEyMzQ1Njc4OWFiY2RlZmdoaWpr",
    // Rule 3: a sensitive key name in each of its three spellings.
    "API_KEY=hunter2",
    "password: hunter2",
    "\"client_secret\": \"hunter2\"",
    // Rule 4: URL userinfo.
    "https://user:hunter2@example.com/repo.git",
    // Rule 1: a PEM private key block.
    "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----",
    // Negatives — over-redaction is a behaviour change too.
    "0731387ec0ffee0731387ec0ffee0731387ec0ff",
    "stella-cli/src/dataset_cmd.rs",
    "the quick brown fox jumps over the lazy dog",
];

/// A stable digest over the redactor's *decisions*, for the manifest.
///
/// Behavioural, not a hash of the rule tables: it moves whenever the redactor
/// answers any probe differently, which covers a changed threshold or a
/// rewritten matcher as well as an edited list. Its one limit, stated plainly:
/// a rule added for a vendor no probe exercises leaves it unchanged — a new
/// rule should arrive with a new probe.
fn redaction_behavior_digest() -> String {
    let mut hasher = Sha256::new();
    hasher.update(PLACEHOLDER.as_bytes());
    for probe in REDACTION_PROBES {
        let redaction = redact_secrets(probe);
        hasher.update(redaction.text.as_bytes());
        hasher.update(if redaction.redacted { b"\x01" } else { b"\x00" });
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_string()
}

/// Write the dataset and its manifest into `dir`, owner-only. Returns both
/// paths.
fn write_dataset(
    dir: &Path,
    records: &[DatasetRecord],
    manifest: &DatasetManifest,
) -> Result<(PathBuf, PathBuf), String> {
    crate::export::create_private_dir(dir)?;

    let mut jsonl = String::new();
    for record in records {
        let line =
            serde_json::to_string(record).map_err(|e| format!("cannot serialize record: {e}"))?;
        jsonl.push_str(&line);
        jsonl.push('\n');
    }
    let manifest = redact_after_assembly(manifest)?;
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| format!("cannot serialize manifest: {e}"))?;

    let dataset_path = dir.join(DATASET_FILE);
    let manifest_path = dir.join(MANIFEST_FILE);
    for (path, bytes) in [
        (&dataset_path, jsonl.as_bytes()),
        (&manifest_path, manifest_json.as_bytes()),
    ] {
        stella_store::durable::write_atomic(path, bytes, stella_store::durable::MODE_PRIVATE)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok((dataset_path, manifest_path))
}

/// What was written, and what was left out — an empty dataset says why rather
/// than printing a bare zero.
fn report(
    records: &[DatasetRecord],
    manifest: &DatasetManifest,
    dataset_path: &Path,
    manifest_path: &Path,
) {
    let filter = &manifest.filter;
    println!(
        "{} accepted turn(s) from {} settled execution(s) ({} in window)",
        records.len().to_string().bold(),
        filter.executions_scanned,
        filter.executions_in_window,
    );
    println!("  accepted when: {}", filter.acceptance_predicate);
    println!("  {}", dataset_path.display().to_string().cyan());
    println!("  {}", manifest_path.display().to_string().cyan());
    if records.is_empty() {
        println!(
            "\nNothing matched. Turns qualify only once they have settled with a \
             success outcome AND changed a file; a read-only or aborted turn is \
             recorded but is not a trajectory to distil from."
        );
        return;
    }
    let redacted = records.iter().filter(|r| r.redacted).count();
    println!(
        "\n{redacted} record(s) had a secret replaced with `{PLACEHOLDER}` \
         (redactor {}).",
        manifest.redaction.behavior_digest
    );
    println!(
        "{}",
        "Human sign-off is required before any dataset is used for training (#836).".dimmed()
    );
}

#[cfg(test)]
mod tests;
