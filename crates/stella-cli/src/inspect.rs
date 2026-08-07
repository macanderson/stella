//! `stella inspect` — the read surface over the context receipts: show the
//! exact `Vec<CompletionMessage>` a past model call was sent, rebuilt from the
//! append-only fold rather than from any live engine state.
//!
//! The storage layer has been able to do this since the receipts plane landed
//! ([`Store::reconstruct_call`]); until now nothing exposed it, so the honest
//! answer to "what context did the model actually get?" was "it's in SQLite,
//! write your own query." This is that query.
//!
//! Reads only — like `stella stats`, it refuses to create `.stella/` as a side
//! effect of being asked a question, and needs no API key or provider.
//!
//! Three levels, narrowing as you supply more:
//!
//! - `stella inspect` — executions that have receipts, most recent first.
//! - `stella inspect <id>` — every recorded model call of one execution.
//! - `stella inspect <id> --step N` — the reconstructed context of one call.
//!
//! ## What "verified" means in the output
//!
//! Every block whose preimage came from the event journal is re-hashed and
//! checked against the digest the receipt recorded at emission, so bytes that
//! are not that block's surface as a mismatch instead of a plausible-looking
//! lie. The two gap kinds (the system prefix and the assembled user/recall
//! message) are stored as local bytes, so their check is tautological and is
//! deliberately not counted as evidence — see [`stella_store::Reconstruction`].
//! The banner reports both facts rather than flattening them into one word.
//!
//! A mismatch says the recovered bytes are not this block's; it does not say
//! anyone altered anything. The ordinary cause is a compaction pass rewriting
//! a tool result in place while the journal keeps only the pre-compaction
//! output under that `call_id`, so replay recovers a real result that is
//! simply not the one this step sent.
//!
//! ## `--diff`: what changed, rather than what was sent
//!
//! Printing the whole context answers "what did the model see", which is the
//! question the receipts plane was built for. It is the wrong tool for "what
//! did *this* call see that the last one didn't" — a system prompt is hundreds
//! of stable lines, and finding the paragraph that moved by reading two of them
//! side by side is not a thing a person can do reliably.
//!
//! `--diff` renders the same reconstruction as a unified diff instead, against
//! one of three baselines:
//!
//! - `prev` (the default) — whatever ran immediately before, **in the same
//!   role**. Same-role matters: a step that ran the worker and a verifier holds
//!   two unrelated prompts, and diffing across them produces noise, not a
//!   delta.
//! - `first` — the first call of that role in the session: the first ever turn.
//! - `prompt` — the bytes the user actually submitted, before the context
//!   engine touched them.
//!
//! ### A turn is an execution
//!
//! `prev` searches the whole session, not just the execution named on the
//! command line, because a *turn* — one prompt the user submitted — is one
//! execution row, while `turn_instance` counts sub-turns inside it and is zero
//! for nearly every real call. Stopping at the execution boundary would answer
//! the wrong question in the most important case: a system prompt is
//! byte-stable across the steps of one execution *by design* (the prompt-cache
//! invariant), so the only place it can drift is across turns. A diff that
//! refused to look there would print "no change" for exactly the comparison
//! worth making. The `---` header names the execution whenever the baseline
//! came from an earlier one, so the crossing is never silent.
//!
//! ### Why the prompt is a baseline at all
//!
//! The `prompt` baseline is not a fallback so much as the honest answer for the
//! very first call of a session, which has no predecessor: everything the
//! reconstruction contains beyond what was typed is, by definition, what Stella
//! added. That mutation is what the user never sees — they know only the words
//! they submitted — and it is invisible in every other view, since the
//! transcript shows the sum and never the delta.

use colored::Colorize;
use serde::Serialize;
// The differ lives in its own zero-dep leaf crate (#1511) so the Observatory's
// prompt-diff route and this command share one implementation.
use stella_diff::{Diff, Op, unified_diff};
use stella_protocol::{CompletionMessage, MessageRole, ToolOutput};
use stella_store::{JournalEra, MismatchSeverity, Reconstruction, RecordedCall, Store};

use crate::query_format::{QueryFormat, Rows, Versioned};

/// How much of a message body the text format prints before eliding. The whole
/// point of this command is seeing the real bytes, so the cap is generous and
/// `--full` removes it entirely.
const DEFAULT_BODY_LIMIT: usize = 4_000;

/// Prompt preview width in the execution index.
const PROMPT_PREVIEW: usize = 68;

/// Which earlier state `--diff` compares this call against. See the module
/// docs for why `Prev` is same-role and why `Prompt` is the right answer for a
/// role's first call rather than an error.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DiffBase {
    /// The call that immediately preceded this one — the previous step of this
    /// turn, or the last call of the previous turn when this is a turn's first
    /// call. Falls back to `prompt` when nothing preceded it at all.
    Prev,
    /// The first call of this role in the whole session — the first ever turn.
    First,
    /// This turn's prompt exactly as submitted, before any context was added.
    Prompt,
}

/// Which messages the compared document keeps. The complaint `--diff` answers
/// is specifically about *system prompts*, and a step-to-step comparison of the
/// whole context is dominated by the tool round-trips each step appends — so
/// narrowing to one role is the difference between a readable delta and a wall.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum RoleFilter {
    All,
    System,
    User,
    Assistant,
    Tool,
}

impl RoleFilter {
    fn keeps(self, role: MessageRole) -> bool {
        match self {
            RoleFilter::All => true,
            RoleFilter::System => role == MessageRole::System,
            RoleFilter::User => role == MessageRole::User,
            RoleFilter::Assistant => role == MessageRole::Assistant,
            RoleFilter::Tool => role == MessageRole::Tool,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RoleFilter::All => "all messages",
            RoleFilter::System => "system messages",
            RoleFilter::User => "user messages",
            RoleFilter::Assistant => "assistant messages",
            RoleFilter::Tool => "tool messages",
        }
    }
}

/// Everything `stella inspect` was invoked with. A struct rather than nine
/// positional parameters: the flags are all optional refinements of one
/// question, and threading them individually through three call layers is how
/// argument order gets silently transposed.
pub(crate) struct InspectArgs {
    pub(crate) execution_id: Option<i64>,
    pub(crate) turn: u32,
    pub(crate) step: Option<u64>,
    pub(crate) call_seq: u64,
    pub(crate) format: QueryFormat,
    pub(crate) full: bool,
    pub(crate) diff: Option<DiffBase>,
    pub(crate) context: usize,
    pub(crate) only: RoleFilter,
}

/// `stella inspect [id] [--turn T] [--step N] [--call-seq S] [--diff [BASE]]`.
pub(crate) fn run_inspect(args: &InspectArgs) -> Result<(), String> {
    let store = open_readonly_store()?;
    match (args.execution_id, args.step) {
        (None, _) => list_executions(&store, args.format),
        (Some(id), None) if args.diff.is_some() => Err(format!(
            "--diff compares one call against an earlier one, so it needs to know which: \
             add --step <STEP> (`stella inspect {id}` lists them)"
        )),
        (Some(id), None) => list_calls(&store, id, args.format),
        (Some(id), Some(step)) => match args.diff {
            Some(base) => show_diff(&store, id, step, base, args),
            None => show_call(
                &store,
                id,
                args.turn,
                step,
                args.call_seq,
                args.format,
                args.full,
            ),
        },
    }
}

/// `stella calibration` (#871, #1293): fold every recorded session's pass
/// verdicts against the evidence that later contradicted or confirmed them,
/// and report the verifier's measured false-positive rate beside the
/// deterministic cohort's.
///
/// Three passes over what already exists — no new write path, no daemon:
///
/// 1. Fold each session, keeping the passes it could not settle itself.
/// 2. Build the workspace's answer key: every terminal CI verdict from
///    **every** session (so a verdict recorded after another session ended
///    still reaches it), plus the reverts in the git history (a human saying
///    "this was wrong", the source #1293 added).
/// 3. Settle what the key can reach, and leave the rest unreconciled.
pub(crate) fn run_calibration(format: QueryFormat) -> Result<(), String> {
    use stella_pipeline::replay::ground_truth::{GroundTruth, reconcile};

    let store = open_readonly_store()?;
    let sessions = store
        .event_session_ids()
        .map_err(|e| format!("cannot enumerate recorded sessions: {e}"))?;
    let mut total = stella_pipeline::replay::CalibrationReport::default();
    let mut pending = Vec::new();
    // Seeded with the git history's reverts before any stream is read: a
    // revert is evidence about a commit, not about a session, so it belongs
    // to the workspace rather than to whichever stream happens to mention it.
    let mut truth = GroundTruth::default().with_reverts(revert_targets_from_git());
    for session_id in &sessions {
        let journal = store
            .session_events(session_id)
            .map_err(|e| format!("cannot read session {session_id}: {e}"))?;
        let events: Vec<stella_protocol::AgentEvent> =
            journal.events.into_iter().map(|r| r.event).collect();
        let (report, unsettled) = stella_pipeline::replay::calibration_pending(session_id, &events);
        total = total.merge(report);
        pending.extend(unsettled);
        // Every session contributes its CI observations to the key, including
        // the ones that arrived too late for their own stream.
        truth = truth.with_stream(&events);
    }
    let settled_late = reconcile(&mut total, &pending, &truth);
    if matches!(format, QueryFormat::Json) {
        return print_json(&Versioned::new(serde_json::json!({
            "sessions": sessions.len(),
            "verifier_passes": total.verifier_passes,
            "verifier_reconciled": total.verifier_reconciled,
            "verifier_false_positives": total.verifier_false_positives,
            "verifier_false_positive_rate": total.verifier_false_positive_rate(),
            "deterministic_passes": total.deterministic_passes,
            "deterministic_reconciled": total.deterministic_reconciled,
            "deterministic_false_positives": total.deterministic_false_positives,
            "deterministic_false_positive_rate": total.deterministic_false_positive_rate(),
            // #1295: the verifier-alone cohort, so the decision about
            // `verifier_evidence_demand` can be taken from a measured number
            // rather than from the last run someone remembers.
            "snapshotted_verdicts": total.snapshotted_verdicts,
            "uncorroborated_verdicts": total.uncorroborated_verdicts,
            "uncorroborated_rate": total.uncorroborated_rate(),
            "verifier_passes_standing_alone": total.verifier_passes_standing_alone,
            // #1293: the answer key's own reach — how many earlier verdicts
            // evidence arriving after their session settled, and how many of
            // the reconciled ones a human revert (not CI) contradicted.
            "settled_by_late_evidence": settled_late,
            "unreconciled_passes": pending.len() as u32 - settled_late,
            "verifier_reverted": total.verifier_reverted,
            "deterministic_reverted": total.deterministic_reverted,
        })));
    }
    println!(
        "{} session(s) with recorded events\n{}\n  \
         late evidence settled {settled_late} verdict(s) their own session never resolved; \
         {} still unreconciled",
        sessions.len(),
        stella_pipeline::replay::render_calibration(&total),
        pending.len() as u32 - settled_late,
    );
    Ok(())
}

/// The commits this repository's history says were reverted (#1293).
///
/// Read-only and best-effort, exactly like every other question `stella
/// inspect` asks: a workspace that is not a git repository, or a `git` that
/// is not installed, contributes no revert evidence and the calibration
/// simply has one fewer source. It never fails the command — a missing
/// answer key is the state this is trying to improve, not an error.
///
/// The window is deliberately bounded. A revert contradicts a judgement made
/// *before* it, so history older than the recorded sessions cannot settle
/// anything, and reading the whole log of a long-lived repository would cost
/// seconds to learn nothing.
fn revert_targets_from_git() -> std::collections::BTreeSet<String> {
    use stella_pipeline::replay::ground_truth::parse_revert_targets;

    let Ok(root) = std::env::current_dir() else {
        return Default::default();
    };
    let mut cmd = std::process::Command::new("git");
    cmd.args(["log", "--no-merges", "-n", "2000", "--format=%B"])
        .current_dir(&root);
    // The same discipline as every other git spawn in this crate: a
    // hook-exported GIT_* var must not re-target the read at another repo.
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    let Ok(output) = cmd.output() else {
        return Default::default();
    };
    if !output.status.success() {
        return Default::default();
    }
    parse_revert_targets(&String::from_utf8_lossy(&output.stdout))
}

/// Open the workspace store without creating it. Mirrors `stella stats`: a
/// question about recorded history must never write.
fn open_readonly_store() -> Result<Store, String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let db_path = stella_store::existing_workspace_private_sqlite_path(&workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?;
    if db_path.is_none() {
        return Err(
            "no local store in this workspace yet — run something first, then inspect it".into(),
        );
    }
    Store::open(&workspace_root).map_err(|e| format!("cannot open local store: {e}"))
}

fn list_executions(store: &Store, format: QueryFormat) -> Result<(), String> {
    let rows = store
        .inspectable_executions(40)
        .map_err(|e| format!("cannot read receipts: {e}"))?;
    if matches!(format, QueryFormat::Json) {
        let rows: Vec<_> = rows.iter().map(execution_json).collect();
        return print_json(&Rows::new(&rows));
    }
    if rows.is_empty() {
        println!(
            "No reconstructable executions recorded yet.\n\
             Receipts are written by builds carrying the context-receipts plane; \
             executions from before it landed have none."
        );
        return Ok(());
    }
    println!("{:>8}  {:>5}  {:<20}  PROMPT", "EXEC", "CALLS", "STARTED");
    for row in &rows {
        println!(
            "{:>8}  {:>5}  {:<20}  {}",
            row.execution_id,
            row.calls,
            truncate(&row.started_at, 20),
            truncate(row.prompt.lines().next().unwrap_or(""), PROMPT_PREVIEW),
        );
    }
    println!("\nstella inspect <EXEC> to list that execution's model calls.");
    Ok(())
}

fn list_calls(store: &Store, execution_id: i64, format: QueryFormat) -> Result<(), String> {
    let calls = store
        .recorded_calls(execution_id)
        .map_err(|e| format!("cannot read receipts: {e}"))?;
    if matches!(format, QueryFormat::Json) {
        let rows: Vec<_> = calls.iter().map(call_json).collect();
        return print_json(&Rows::new(&rows));
    }
    if calls.is_empty() {
        println!("Execution {execution_id} has no recorded receipts.");
        return Ok(());
    }
    // The FRAME column is empty for every call recorded while
    // `context.lifecycle.enabled` was off, which is the default — so it reads
    // as "not computed", not as "computed and empty". Two calls showing the
    // same id saw byte-identical context; that comparison is the column's job,
    // so the id is shown rather than the full hash it prefixes.
    let any_frame = calls.iter().any(|c| c.compiled_frame_id.is_some());
    println!(
        "{:>4}  {:>4}  {:>3}  {:<14}  {:<10}  {:>9}{}",
        "TURN",
        "STEP",
        "SEQ",
        "ROLE",
        "PROVIDER",
        "EST TOK",
        if any_frame { "  FRAME" } else { "" }
    );
    for call in &calls {
        println!(
            "{:>4}  {:>4}  {:>3}  {:<14}  {:<10}  {:>9}{}",
            call.turn_instance,
            call.step,
            call.call_seq,
            truncate(&call.call_role, 14),
            truncate(&call.provider, 10),
            call.estimated_input_tokens,
            match (&call.compiled_frame_id, any_frame) {
                (Some(id), _) => format!("  {id}"),
                (None, true) => "  —".to_string(),
                (None, false) => String::new(),
            }
        );
    }
    println!(
        "\nstella inspect {execution_id} --step <STEP> [--turn <TURN>] [--call-seq <SEQ>] \
         to see the context one call was sent."
    );
    Ok(())
}

fn show_call(
    store: &Store,
    execution_id: i64,
    turn: u32,
    step: u64,
    call_seq: u64,
    format: QueryFormat,
    full: bool,
) -> Result<(), String> {
    let recon = store
        .reconstruct_call(execution_id, turn, step, call_seq)
        .map_err(|e| format!("cannot reconstruct: {e}"))?;
    if recon.messages.is_empty() && recon.unresolved.is_empty() {
        return Err(format!(
            "no receipt for execution {execution_id} turn {turn} step {step} call-seq {call_seq} \
             — `stella inspect {execution_id}` lists what was recorded"
        ));
    }
    match format {
        QueryFormat::Json => print_json(&Versioned::new(reconstruction_json(&recon))),
        QueryFormat::Text => {
            print_reconstruction(&recon, execution_id, turn, step, call_seq, full);
            Ok(())
        }
    }
}

fn print_reconstruction(
    recon: &Reconstruction,
    execution_id: i64,
    turn: u32,
    step: u64,
    call_seq: u64,
    full: bool,
) {
    println!("execution {execution_id} · turn {turn} · step {step} · call-seq {call_seq}");
    println!(
        "{} message(s){}",
        recon.messages.len(),
        if full { ", full bodies" } else { "" }
    );
    // Report the two failure modes separately: an unresolved block is a
    // documented coverage gap, a digest mismatch means the bytes recovered for
    // a block are not the bytes it recorded. Collapsing them into "unverified"
    // would hide which one happened.
    if !recon.unresolved.is_empty() {
        println!(
            "!  {} block(s) could not be resolved (synthetic results, discarded \
             speculation, or attachments): {}",
            recon.unresolved.len(),
            recon.unresolved.join(", ")
        );
    }
    if let Some(line) = digest_mismatch_line(recon) {
        println!("{line}");
    }
    if recon.is_verified() {
        println!("verified: every journal-resolved block re-hashed to its recorded digest");
    }
    for (index, message) in recon.messages.iter().enumerate() {
        println!("\n─── [{index}] {} ───", role_tag(message.role));
        let body = message_body(message);
        if full || body.len() <= DEFAULT_BODY_LIMIT {
            println!("{body}");
        } else {
            let cut = floor_char_boundary(&body, DEFAULT_BODY_LIMIT);
            println!(
                "{}\n… {} more bytes (--full to print, --format json for the exact bytes)",
                &body[..cut],
                body.len() - cut
            );
        }
    }
    // The whole context answers "what was sent"; it is a poor way to find what
    // *moved*. Say so here rather than in the help text nobody re-reads.
    println!(
        "\nstella inspect {execution_id} --step {step} --diff \
         to see only what changed since the previous call of this role."
    );
}

/// The digest-mismatch line, or `None` when nothing mismatched.
///
/// A pure function because the wording *is* the feature, and the two eras it
/// distinguishes are the point of #1981: on a journal written before
/// compaction recorded its rewrites (#1667) a compacted block mismatches as a
/// matter of course, so calling that an integrity breach trains the reader to
/// skip the line — the one outcome that would matter if it were real. On a
/// journal that records every rewrite there is no routine explanation left,
/// and the alarm is the honest reading.
///
/// The severity verdict comes from [`Reconstruction::mismatch_severity`]
/// rather than from any reasoning here, so this surface cannot drift from
/// `stella trace`, the deck overlay, or the observatory.
pub(crate) fn digest_mismatch_line(recon: &Reconstruction) -> Option<String> {
    let count = recon.digest_mismatches.len();
    let ids = recon.digest_mismatches.join(", ");
    match recon.mismatch_severity() {
        MismatchSeverity::None => None,
        MismatchSeverity::Compaction => Some(format!(
            "!  {count} block(s) did NOT re-hash to their recorded digest — shown from the \
             closest preimage. This journal predates compaction recording its rewrites, so a \
             compacted block reads this way as a matter of course: {ids}"
        )),
        // `!!` rather than `!`: the marker is the only part of the banner a
        // reader scanning output will register, and the two eras must not
        // share one. The line is otherwise uncoloured like every other banner
        // line here — a severity that survives a pipe is worth more than one
        // that needs a terminal.
        MismatchSeverity::Integrity => Some(format!(
            "!! {count} block(s) did NOT re-hash to their recorded digest. This journal records \
             every compaction rewrite, so nothing routine accounts for these bytes — treat the \
             reconstruction as untrustworthy: {ids}"
        )),
    }
}

/// The earlier state a diff is taken against, already resolved to bytes plus
/// the label the `---` header prints.
struct Baseline {
    label: String,
    /// The `kind` the JSON format reports — `prev`, `first`, or `prompt`. The
    /// requested base and the resolved one can differ (`prev` on a role's first
    /// call resolves to `prompt`), and a script must be able to see which it
    /// actually got.
    kind: &'static str,
    document: String,
}

fn show_diff(
    store: &Store,
    execution_id: i64,
    step: u64,
    base: DiffBase,
    args: &InspectArgs,
) -> Result<(), String> {
    let turn = args.turn;
    let call_seq = args.call_seq;
    let recon = store
        .reconstruct_call(execution_id, turn, step, call_seq)
        .map_err(|e| format!("cannot reconstruct: {e}"))?;
    if recon.messages.is_empty() && recon.unresolved.is_empty() {
        return Err(format!(
            "no receipt for execution {execution_id} turn {turn} step {step} call-seq {call_seq} \
             — `stella inspect {execution_id}` lists what was recorded"
        ));
    }
    let calls = store
        .recorded_calls(execution_id)
        .map_err(|e| format!("cannot read receipts: {e}"))?;
    let role = calls
        .iter()
        .find(|c| c.turn_instance == turn && c.step == step && c.call_seq == call_seq)
        .map(|c| c.call_role.clone())
        .unwrap_or_else(|| "worker".to_string());
    let target = CallRef {
        execution_id,
        turn,
        step,
        call_seq,
        role: role.clone(),
    };

    let baseline = resolve_baseline(store, &target, base, args)?;
    let document = render_document(&recon, args.only);
    let computed = unified_diff(&baseline.document, &document, args.context);

    let target_label = target.label(execution_id);
    match args.format {
        QueryFormat::Json => print_json(&Versioned::new(diff_json(
            &computed,
            &baseline,
            &target_label,
            args,
        ))),
        QueryFormat::Text => {
            print_diff(&computed, &baseline, &target_label, args);
            Ok(())
        }
    }
}

/// One recorded call, addressed across executions. `RecordedCall` deliberately
/// carries no execution id — it is always read for a known one — but a baseline
/// can live in an earlier *turn*, which is an earlier execution, so the id has
/// to travel with the coordinates here.
struct CallRef {
    execution_id: i64,
    turn: u32,
    step: u64,
    call_seq: u64,
    role: String,
}

impl CallRef {
    /// How this call is named in a `---`/`+++` header. The execution id is
    /// printed only when it differs from the call being inspected, so a
    /// same-turn comparison stays terse and a cross-turn one is unmistakable.
    fn label(&self, relative_to: i64) -> String {
        let prefix = if self.execution_id == relative_to {
            String::new()
        } else {
            format!("execution {} · ", self.execution_id)
        };
        format!(
            "{prefix}turn {} · step {} · seq {} ({})",
            self.turn, self.step, self.call_seq, self.role
        )
    }
}

/// Pick the earlier call — or the submitted prompt — this call is measured
/// against, and render it to the same document shape as the target.
///
/// The search runs over the whole *session*, oldest execution first, keeping
/// only calls of the same role. Same-role matters because a step that ran the
/// worker and a verifier holds two unrelated prompts; whole-session matters
/// because a system prompt is byte-stable inside one execution by design, so
/// the only place it can drift is across turns — and stopping at the execution
/// boundary would report "no change" for the exact comparison the user came to
/// make.
fn resolve_baseline(
    store: &Store,
    target: &CallRef,
    base: DiffBase,
    args: &InspectArgs,
) -> Result<Baseline, String> {
    if matches!(base, DiffBase::Prompt) {
        return prompt_baseline(store, target.execution_id);
    }

    // Every turn of this conversation, oldest first. An execution with no
    // session (a one-shot `stella run`) is its own session.
    let mut turns = store
        .session_executions(target.execution_id)
        .map_err(|e| format!("cannot read the session: {e}"))?;
    if turns.is_empty() {
        turns.push(target.execution_id);
    }

    let mut earlier: Vec<CallRef> = Vec::new();
    for turn_execution in turns {
        // Later turns cannot be a baseline for an earlier one; ids are
        // allocated in submission order.
        if turn_execution > target.execution_id {
            break;
        }
        let calls = store
            .recorded_calls(turn_execution)
            .map_err(|e| format!("cannot read receipts: {e}"))?;
        for call in calls {
            let candidate = CallRef {
                execution_id: turn_execution,
                turn: call.turn_instance,
                step: call.step,
                call_seq: call.call_seq,
                role: call.call_role,
            };
            if candidate.execution_id == target.execution_id
                && (candidate.turn, candidate.step, candidate.call_seq)
                    >= (target.turn, target.step, target.call_seq)
            {
                break;
            }
            if candidate.role == target.role {
                earlier.push(candidate);
            }
        }
    }

    let picked = match base {
        DiffBase::First => earlier.into_iter().next(),
        _ => earlier.pop(),
    };
    // Nothing of this role ran before: the only prior state that ever existed
    // is what the user typed, so that is the honest baseline rather than an
    // error or an empty diff.
    let Some(previous) = picked else {
        return prompt_baseline(store, target.execution_id);
    };

    let recon = store
        .reconstruct_call(
            previous.execution_id,
            previous.turn,
            previous.step,
            previous.call_seq,
        )
        .map_err(|e| format!("cannot reconstruct the baseline call: {e}"))?;
    Ok(Baseline {
        label: previous.label(target.execution_id),
        kind: if matches!(base, DiffBase::First) {
            "first"
        } else {
            "prev"
        },
        document: render_document(&recon, args.only),
    })
}

fn prompt_baseline(store: &Store, execution_id: i64) -> Result<Baseline, String> {
    let prompt = store
        .execution_prompt(execution_id)
        .map_err(|e| format!("cannot read the submitted prompt: {e}"))?
        .ok_or_else(|| format!("execution {execution_id} is not in the local store"))?;
    Ok(Baseline {
        label: "prompt as submitted".to_string(),
        kind: "prompt",
        document: prompt,
    })
}

/// Flatten a reconstruction into the line-oriented document the diff compares.
///
/// Messages are headed by role alone, never by index: a message inserted in the
/// middle would renumber every header after it, and a diff that reports forty
/// changed lines because one number shifted is worse than no diff at all.
fn render_document(recon: &Reconstruction, only: RoleFilter) -> String {
    let mut out = String::new();
    for message in recon.messages.iter().filter(|m| only.keeps(m.role)) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("── {} ──\n", role_tag(message.role)));
        out.push_str(&message_body(message));
        out.push('\n');
    }
    out
}

fn print_diff(computed: &Diff, baseline: &Baseline, target_label: &str, args: &InspectArgs) {
    println!("--- {}", baseline.label.bold());
    println!("+++ {}", target_label.bold());
    if !computed.changed() {
        println!(
            "no change: the {} are byte-identical to {}",
            args.only.label(),
            baseline.label
        );
        return;
    }
    println!(
        "{} added, {} removed  ({}{})",
        format!("+{}", computed.added).green(),
        format!("-{}", computed.removed).red(),
        args.only.label(),
        if computed.minimal {
            String::new()
        } else {
            ", coarse: input too large for an exact diff".to_string()
        }
    );
    // The submitted prompt has no role, so a role filter cannot apply to it —
    // and its line then shows as removed, which is true of the two documents
    // but reads like the prompt was taken away. Say what it means once, rather
    // than leaving the reader to work it out from a `-` that is technically
    // correct and practically confusing.
    if baseline.kind == "prompt" && !matches!(args.only, RoleFilter::All) {
        println!(
            "note: the submitted prompt is not one of the {}, so it shows as removed \
             — everything added is what Stella put there.",
            args.only.label()
        );
    }
    for hunk in &computed.hunks {
        println!("{}", hunk.header().cyan());
        for line in &hunk.lines {
            let rendered = format!("{}{}", line.op.sigil(), line.text);
            match line.op {
                Op::Add => println!("{}", rendered.green()),
                Op::Remove => println!("{}", rendered.red()),
                Op::Equal => println!("{rendered}"),
            }
        }
    }
}

#[derive(Serialize)]
struct DiffLineJson {
    op: &'static str,
    text: String,
}

#[derive(Serialize)]
struct HunkJson {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<DiffLineJson>,
}

#[derive(Serialize)]
struct DiffJson {
    /// Which baseline was actually used — `prev` can resolve to `prompt`.
    base: &'static str,
    base_label: String,
    target_label: String,
    scope: &'static str,
    changed: bool,
    added: usize,
    removed: usize,
    /// `false` when the input exceeded the exact-diff bound and every line was
    /// reported as replaced. Surfaced so a script never reads a coarse script
    /// as a precise measurement.
    minimal: bool,
    hunks: Vec<HunkJson>,
}

fn diff_json(
    computed: &Diff,
    baseline: &Baseline,
    target_label: &str,
    args: &InspectArgs,
) -> DiffJson {
    DiffJson {
        base: baseline.kind,
        base_label: baseline.label.clone(),
        target_label: target_label.to_string(),
        scope: args.only.label(),
        changed: computed.changed(),
        added: computed.added,
        removed: computed.removed,
        minimal: computed.minimal,
        hunks: computed
            .hunks
            .iter()
            .map(|h| HunkJson {
                old_start: h.old_start,
                old_count: h.old_count,
                new_start: h.new_start,
                new_count: h.new_count,
                lines: h
                    .lines
                    .iter()
                    .map(|l| DiffLineJson {
                        op: l.op.tag(),
                        text: l.text.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// The message's printable body: text content, plus a compact rendering of any
/// tool calls/results it carried (they are separate blocks in the receipt but
/// belong to one message on the wire).
///
/// Shared with the deck's INSPECT overlay (`command_deck::service_inspect_action`):
/// the CLI and the deck must never disagree about what a message looked like,
/// so there is one renderer, not a copy per surface.
pub(crate) fn message_body(message: &CompletionMessage) -> String {
    let mut out = String::new();
    if !message.content.is_empty() {
        out.push_str(&message.content);
    }
    for call in &message.tool_calls {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "→ tool_call {} {} {}",
            call.call_id, call.name, call.input
        ));
    }
    for result in &message.tool_results {
        if !out.is_empty() {
            out.push('\n');
        }
        // Render the payload directly rather than serializing the whole
        // `ToolOutput` enum. Serialization escapes every newline and quote
        // inside a result body (file contents, command stdout), turning a
        // readable transcript into one long line of `\n` and `\"`.
        out.push_str("← tool_result ");
        out.push_str(&result.call_id);
        out.push('\n');
        match &result.output {
            ToolOutput::Ok { content } => out.push_str(content),
            ToolOutput::Error { message } => out.push_str(&format!("error: {message}")),
        }
    }
    out
}

/// The stable wire tag for a message role — the same token the deck's INSPECT
/// overlay shows, for the same reason [`message_body`] is shared.
pub(crate) fn role_tag(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

#[derive(Serialize)]
struct ExecutionJson {
    execution_id: i64,
    kind: String,
    prompt: String,
    started_at: String,
    calls: u64,
}

#[derive(Serialize)]
struct CallJson {
    turn_instance: u32,
    step: u64,
    call_seq: u64,
    call_role: String,
    provider: String,
    model: String,
    estimated_input_tokens: u64,
    /// Phase 2 (#713): the compiled frame's identity, when the lifecycle was
    /// on for this call. Omitted rather than null when it was off — a receipt
    /// with no frame is not a receipt with an empty frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    compiled_frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_hash: Option<String>,
}

#[derive(Serialize)]
struct MessageJson {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ReconstructionJson {
    verified: bool,
    unresolved: Vec<String>,
    digest_mismatches: Vec<String>,
    /// Which compaction-journaling era wrote this execution's journal —
    /// `compaction_journaled` or `compaction_unjournaled`. A script cannot
    /// read `digest_mismatches` honestly without it: on an unjournaled journal
    /// a compacted block mismatches as a matter of course.
    journal_era: &'static str,
    /// What those mismatches mean here — `none`, `compaction`, or `integrity`.
    /// Derived from the two fields above so a caller never has to combine them
    /// itself, and the same three words every other surface styles from.
    digest_mismatch_severity: &'static str,
    messages: Vec<MessageJson>,
}

/// The stable JSON spelling of an era. Named constants rather than `Debug`,
/// because these are a wire contract for `--format json` consumers.
fn era_tag(era: JournalEra) -> &'static str {
    match era {
        JournalEra::CompactionUnjournaled => "compaction_unjournaled",
        JournalEra::CompactionJournaled => "compaction_journaled",
    }
}

/// The deck's mirror of a store era. `stella-tui` links no store, so the
/// driver maps the two — the same bargain every other type on the inspect
/// envelope takes, and it lives here beside the other inspect mappings rather
/// than in `command_deck.rs`.
pub(crate) fn deck_journal_era(era: JournalEra) -> stella_tui::JournalEra {
    match era {
        JournalEra::CompactionUnjournaled => stella_tui::JournalEra::CompactionUnjournaled,
        JournalEra::CompactionJournaled => stella_tui::JournalEra::CompactionJournaled,
    }
}

/// The stable JSON spelling of a mismatch verdict. Shared with the observatory
/// payload by convention, not by linkage — that crate links no workspace crate
/// (see its README) — so the three words are pinned by a test on both sides.
pub(crate) fn severity_tag(severity: MismatchSeverity) -> &'static str {
    match severity {
        MismatchSeverity::None => "none",
        MismatchSeverity::Compaction => "compaction",
        MismatchSeverity::Integrity => "integrity",
    }
}

fn execution_json(row: &stella_store::InspectableExecution) -> ExecutionJson {
    ExecutionJson {
        execution_id: row.execution_id,
        kind: row.kind.clone(),
        prompt: row.prompt.clone(),
        started_at: row.started_at.clone(),
        calls: row.calls,
    }
}

fn call_json(call: &RecordedCall) -> CallJson {
    CallJson {
        turn_instance: call.turn_instance,
        step: call.step,
        call_seq: call.call_seq,
        call_role: call.call_role.clone(),
        provider: call.provider.clone(),
        model: call.model.clone(),
        estimated_input_tokens: call.estimated_input_tokens,
        compiled_frame_id: call.compiled_frame_id.clone(),
        frame_hash: call.frame_hash.clone(),
    }
}

fn reconstruction_json(recon: &Reconstruction) -> ReconstructionJson {
    ReconstructionJson {
        verified: recon.is_verified(),
        unresolved: recon.unresolved.clone(),
        digest_mismatches: recon.digest_mismatches.clone(),
        journal_era: era_tag(recon.journal_era),
        digest_mismatch_severity: severity_tag(recon.mismatch_severity()),
        messages: recon
            .messages
            .iter()
            .map(|m| MessageJson {
                role: role_tag(m.role),
                content: message_body(m),
            })
            .collect(),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    let rendered =
        serde_json::to_string_pretty(value).map_err(|e| format!("cannot render json: {e}"))?;
    println!("{rendered}");
    Ok(())
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let cut: String = s.chars().take(limit.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Largest index `<= limit` that is a char boundary — slicing a UTF-8 body at a
/// raw byte offset would panic on any multibyte character.
fn floor_char_boundary(s: &str, limit: usize) -> usize {
    let mut cut = limit.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("abc", 8), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // Multibyte input must not panic or split a character.
        assert_eq!(truncate("ααααααα", 3), "αα…");
    }

    #[test]
    fn body_cut_lands_on_a_char_boundary() {
        let s = "α".repeat(100);
        let cut = floor_char_boundary(&s, 51);
        assert!(s.is_char_boundary(cut), "cut must be sliceable");
        assert_eq!(cut, 50, "rounds down to the boundary below the limit");
    }

    #[test]
    fn message_body_renders_tool_round_trips_not_just_text() {
        use stella_protocol::{ToolCall, ToolOutput, ToolResult};
        let message = CompletionMessage {
            role: MessageRole::Assistant,
            content: "reading the file".into(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
            tool_results: vec![],
            attachments: vec![],
        };
        let body = message_body(&message);
        assert!(body.contains("reading the file"));
        assert!(body.contains("→ tool_call c1 read_file"));

        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn main() {}".into(),
                },
            }],
            attachments: vec![],
        };
        assert!(message_body(&tool).contains("← tool_result c1"));
    }

    #[test]
    fn message_body_keeps_newlines_and_quotes_unescaped() {
        // Tool results routinely carry file contents and command stdout with
        // embedded newlines and quotes. The old renderer serialized the whole
        // `ToolOutput` enum, which escaped every `\n` and `"` into one long
        // unreadable line. The body must print the raw payload instead.
        use stella_protocol::ToolResult;
        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn main() {\n    println!(\"hi\");\n}".into(),
                },
            }],
            attachments: vec![],
        };
        let body = message_body(&tool);
        assert!(body.contains("fn main() {\n    println!(\"hi\");\n}"));
        assert!(!body.contains("\\n"));
        assert!(!body.contains("\\\""));
    }

    #[test]
    fn message_body_labels_error_results() {
        use stella_protocol::ToolResult;
        let tool = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "e1".into(),
                output: ToolOutput::Error {
                    message: "file not found".into(),
                },
            }],
            attachments: vec![],
        };
        let body = message_body(&tool);
        assert!(body.contains("← tool_result e1\nerror: file not found"));
    }
}
