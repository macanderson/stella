//! Extraction — turning a markdown document into reviewable context-record
//! proposals.
//!
//! This is the step the scan half of `stella ingest` set up: a classified
//! document is divided into slices ([`super::chunk`]) and each goes to the
//! worker model, which splits it into atomic claims; the slices' claims are
//! merged, and each claim is mapped to a [`Record`], run through the
//! deterministic safety [`gate`], probed against the tree for staleness, stamped
//! with its content-derived identity, and written to `.stella/proposals/` as a
//! `[[proposal]]` file in the shape of `docs/spec/adaptive-context/context-record-examples/05`.
//!
//! ## Why a document is divided rather than capped
//!
//! It used to be one call, with everything past a character cap dropped before
//! the model saw it — 46% of this repository's own `AGENTS.md`, reported as a
//! success (#2407). Raising the cap only moves that edge onto the reply, which
//! is roughly the size of the text that produced it. So the document is divided
//! and the claims merged, which makes coverage a property of the slicer rather
//! than a number someone has to keep ahead of the documents people write.
//!
//! Two collisions exist only because there is now more than one call, and
//! [`Merge`] resolves both: the same claim extracted from two slices, and a
//! `lineage_suffix` the prompt asks to be unique *within the document* that each
//! call can only make unique within its own reply.
//!
//! ## Why the safety work is here and not in the prompt
//!
//! The model is asked to split atomically and to surface any executable content
//! it finds, but nothing downstream trusts that it did. Atomicity, quarantine,
//! and probe-gating are re-decided by [`stella_core::ingest::gate`], which is
//! pure and cannot be talked out of a rule by a cleverly-worded document. The
//! model's job is extraction; the gate's job is safety.
//!
//! ## Failure is per-document and never fatal
//!
//! A document whose extraction fails — no key configured, an unparseable model
//! reply, a write error — is reported and skipped. Ingest is user-invoked, so a
//! failure is worth showing, but one bad document never aborts the others.

use std::collections::HashSet;
use std::path::Path;

use colored::Colorize;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use stella_core::context_record::Origin;
use stella_core::ingest::Tier;
use stella_core::ingest::record::Tier as PromotionTier;
use stella_core::ingest::{
    AppliesTo, ContextFile, Defaults, Enforcement, EnforcementMode, Force, Probe, ProbeKind,
    Proposal, Provenance, Record, RecordKind, Refutation, Steering, Truth, TruthBasis, Verdict,
    gate,
};
use stella_protocol::{
    CompletionMessage, CompletionRequest, FinishReason, ModelCallRole, Provider,
};

use super::chunk::{self, Chunk};
use super::probe;
use super::progress::Progress;

/// One document to extract from: its workspace-relative path, its text, and the
/// tier the scan assigned it (which sets the eligibility reason).
pub(super) struct NamedDoc {
    /// Workspace-relative, forward-slashed path.
    pub rel: String,
    /// The document's text.
    pub content: String,
    /// The classification tier.
    pub tier: Tier,
}

/// The most of a document extraction will read, in characters.
///
/// Re-exported from [`super::chunk`] rather than owned here: the ceiling belongs
/// to the slicer that enforces it, and the scan's `MAX_READ_BYTES` has to be
/// asserted against it — a scan that hides what extraction would accept is the
/// failure the two constants have to be compared to rule out.
pub(super) const MAX_DOCUMENT_CHARS: usize = chunk::MAX_DOCUMENT_CHARS;

/// The system prompt: the extractor's whole contract, including the two rules
/// that keep an untrusted document from becoming an attack — atomicity and
/// no-executables.
const SYSTEM_PROMPT: &str = "\
You extract durable engineering context from a project's own markdown into atomic, \
machine-checkable records. You return ONLY a JSON array, no prose, no code fences.

Each element is one ATOMIC claim: a claim that can receive exactly one true/false \
verdict. If a sentence asserts several things that could each be true or false on \
their own (\"deploys run from main, on Tuesdays, via make ship\"), split it into \
several records and set \"compound\": true on none of them. If you cannot split a \
genuinely compound sentence, emit it once with \"compound\": true.

NEVER put a shell command, script, or autofix into a record. If the document says to \
run something to fix a problem, put the verbatim command in the \"executable\" field \
and describe the situation in \"statement\"; the command will be quarantined, not run.

Record fields:
- lineage_suffix: short kebab-case slug unique within the document (e.g. \"pkg-manager\")
- kind: one of memory|fact|rule|preference|constraint|procedure
- statement: one sentence, the claim itself
- force: must|should|may|info (how hard it steers)
- enforcement_mode: hard|soft|none (hard only when a real guard is given)
- guard_tool, guard_deny_command, guard_deny_path: optional deny-shaped guard (blocks a tool call, never runs one)
- basis: decree|measured|asserted (why it is believed true)
- paths, tasks, keywords: optional arrays scoping when it applies
- confidence: integer 0-100
- source_lines: optional [start, end] line numbers in the document
- probe: optional { kind, path, pattern, expect } to re-check the claim. Use ONLY \
path_exists, path_absent, or file_contains against a tracked file (expect \"present\" \
or \"absent\"). NEVER command_succeeds or http_ok. Omit the probe if none of these fit.
- executable: the verbatim command, ONLY if the source asked to run one
- compound: true only if the sentence is irreducibly compound";

/// One extracted claim as the model returns it. Everything the gate and probes
/// need is here; identity and provenance are stamped in Rust, never asked of the
/// model.
#[derive(Debug, Deserialize)]
struct Claim {
    lineage_suffix: String,
    kind: RecordKind,
    statement: String,
    #[serde(default)]
    force: Option<Force>,
    #[serde(default)]
    enforcement_mode: Option<EnforcementMode>,
    #[serde(default)]
    guard_tool: Option<String>,
    #[serde(default)]
    guard_deny_command: Option<String>,
    #[serde(default)]
    guard_deny_path: Option<String>,
    #[serde(default)]
    basis: Option<TruthBasis>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    tasks: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    confidence: Option<u8>,
    #[serde(default)]
    source_lines: Option<Vec<u32>>,
    #[serde(default)]
    probe: Option<Probe>,
    #[serde(default)]
    executable: Option<String>,
    #[serde(default)]
    compound: bool,
}

/// What one document's extraction produced, for the run summary.
pub(super) struct DocSummary {
    /// The path the proposals were written to.
    pub written_to: String,
    /// Total proposals minted.
    pub total: usize,
    /// How many cleared the gate.
    pub eligible: usize,
    /// How many a probe refuted as stale.
    pub refuted: usize,
    /// How many were dismissed (compound or quarantined).
    pub dismissed: usize,
    /// How many were withheld because this workspace already decided about them —
    /// declined within a cooldown, or already published as a record.
    pub withheld: usize,
    /// The pinned footprint: statement characters across this document's
    /// eligible pinned-tier proposals, measured against the cached-channel
    /// budget for the overflow diagnostic (#2709). An approximation from
    /// below — the rendered bullet adds a handle and markers — so a warning
    /// here is never a false alarm.
    pub pinned_chars: usize,
    /// The run cost in USD, across every slice of the document.
    pub cost_usd: f64,
    /// How many slices the document was divided into, and how many of them
    /// failed. A failed slice is content that produced no records, so it is
    /// reported rather than folded into a success — the whole point of #2407 is
    /// that a partially-read document must never look like a complete one.
    pub slices: usize,
    /// The slices that failed, each with the reason, in document order.
    pub failed_slices: Vec<(usize, String)>,
    /// Claims dropped as restatements of a claim an earlier slice already made.
    pub duplicates: usize,
    /// The candidate ids of every proposal written, so the source file's
    /// lineage can name what it produced.
    pub candidate_ids: Vec<String>,
    /// Every claim the document asserts, *including* withheld and dismissed
    /// ones — the refresh diff compares published records against what the
    /// source still says, and a claim withheld as already-decided is exactly
    /// the "unchanged, keep the published record" case. Omitting withheld
    /// claims here would read every unchanged record as removed and retire it.
    pub asserted: Vec<stella_core::ingest::AssertedClaim>,
}

/// Extract every named document, writing one proposal file each.
///
/// Resolves the provider once (its `no API key` error is the friendly failure
/// for an unconfigured workspace), then extracts each document under a single
/// runtime and ingest-run id. Returns `Err` only when nothing could run at all;
/// per-document failures are printed and skipped.
pub(super) fn extract_all(
    root: &Path,
    docs: &[NamedDoc],
    options: super::IngestOptions,
    model: Option<&str>,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), String> {
    let cfg = crate::config::Config::load(model, api_key, base_url)?;
    // The same advisory pass `main` runs after `Config::load` — ingest
    // resolves its own config, so without this it was the one model-calling
    // command with no settings warnings at all (#895).
    crate::settings_check::report_at_launch(&cfg);
    let provider = crate::agent::build_provider(&cfg)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start runtime: {e}"))?;

    let set_id = derive_set_id(root);
    let ingest_run_id = format!("ing_{:x}", crate::memory::unix_now_secs());
    let observed_at = stella_context::format_rfc3339(crate::memory::unix_now_secs());

    println!();
    let mut any = false;
    for doc in docs {
        match runtime.block_on(extract_document(
            provider.as_ref(),
            &cfg.model_id,
            root,
            doc,
            &set_id,
            &ingest_run_id,
            &observed_at,
        )) {
            Ok(summary) => {
                any = true;
                // The lineage is the run's provenance receipt: the hash of the
                // text extraction actually read (not whatever is on disk by
                // now), and the candidates it minted. Best-effort — a failed
                // ledger write must not fail an ingest that already succeeded.
                super::lineage::record_ingest(
                    root,
                    &doc.rel,
                    stella_core::ingest::lineage::Lineage {
                        source_hash: super::lineage::ingested_content_hash(
                            root,
                            &doc.rel,
                            &doc.content,
                        ),
                        commit: git(root, &["rev-parse", "--short", "HEAD"]),
                        ingested_at: observed_at.clone(),
                        ingest_run_id: ingest_run_id.clone(),
                        candidate_ids: summary.candidate_ids.clone(),
                        alerts: stella_core::ingest::lineage::AlertState::Active,
                    },
                    options.keep_dismissed,
                );
                report(doc, &summary);
                // The reconciliation pass, after the summary so its retire
                // lines read as consequences of this document's extraction.
                // A failed retirement is this document's failure, not the
                // run's — the next document still extracts.
                if options.refresh
                    && let Err(err) = super::refresh::apply(root, &doc.rel, &summary.asserted)
                {
                    eprintln!("    {}", err.red());
                }
            }
            Err(err) => {
                eprintln!("  {}  {}", doc.rel.red(), err.dimmed());
            }
        }
    }

    if any {
        println!(
            "\n{}",
            "Review before anything steers: `stella proposals` (or edit the .toml directly)."
                .dimmed()
        );
    }
    Ok(())
}

/// Extract one document: slice, one model call each, merge, gate, probe, write.
///
/// Owns the progress line for the whole document so a reader sees one elapsed
/// clock across every slice and every record, and so exactly one statement ends
/// it — no error path below can leave a ticker drawing over later output.
///
/// Takes `model_hint` rather than the whole `Config` so the assembly path is
/// testable with a mock provider — the only thing extraction needs from config
/// is which model to attribute the call to.
#[allow(clippy::too_many_arguments)]
async fn extract_document(
    provider: &dyn Provider,
    model_hint: &str,
    root: &Path,
    doc: &NamedDoc,
    set_id: &str,
    ingest_run_id: &str,
    observed_at: &str,
) -> Result<DocSummary, String> {
    let progress = Progress::start(format!("reading {}", doc.rel));
    let outcome = extract_narrated(
        provider,
        model_hint,
        root,
        doc,
        set_id,
        ingest_run_id,
        observed_at,
        &progress,
    )
    .await;
    progress.finish().await;
    outcome
}

/// The body of [`extract_document`], narrating into a line the caller owns.
#[allow(clippy::too_many_arguments)]
async fn extract_narrated(
    provider: &dyn Provider,
    model_hint: &str,
    root: &Path,
    doc: &NamedDoc,
    set_id: &str,
    ingest_run_id: &str,
    observed_at: &str,
    progress: &Progress,
) -> Result<DocSummary, String> {
    let defaults = build_defaults(root, doc, ingest_run_id, model_hint, observed_at);
    let eligibility = eligibility_for(doc.tier);

    let extracted = extract_claims(provider, model_hint, root, doc, progress).await?;
    let Extracted {
        claims,
        cost_usd,
        slices,
        failed_slices,
        duplicates,
    } = extracted;
    if claims.is_empty() {
        return Err("the model found no durable context to extract".to_string());
    }

    // What this workspace has already decided. A claim the reviewer declined must
    // not come back while its cooldown holds, and one already kept is a record now
    // rather than a proposal — re-offering either is how a review surface teaches
    // people that reviewing accomplishes nothing.
    let decided =
        stella_core::records::decision::fold(&crate::context_records::read_decisions(root));

    let mut file = ContextFile::new_ingest(set_id, ingest_run_id, defaults.clone());
    let (mut eligible, mut refuted, mut dismissed) = (0usize, 0usize, 0usize);
    let mut withheld = 0usize;
    let mut pinned_chars = 0usize;
    let mut asserted = Vec::new();
    let total_claims = claims.len();
    for (done, claim) in claims.into_iter().enumerate() {
        // Each record is gated and probed, and a probe reads the tree — on a
        // document that atomized into hundreds of records that is a visible
        // stretch of work with no model call in it, which is exactly the window
        // that used to be silent. Naming the record in hand costs a string.
        progress.say(record_label(
            &doc.rel,
            &claim.lineage_suffix,
            done,
            total_claims,
        ));
        // Hand the runtime back so the ticker task — single-threaded here, so it
        // is otherwise starved by this loop — can actually draw the label above.
        tokio::task::yield_now().await;
        let proposal = build_proposal(root, set_id, &defaults, observed_at, eligibility, claim);
        asserted.push(stella_core::ingest::AssertedClaim {
            lineage_id: proposal.record.lineage_id.clone(),
            statement: proposal.record.statement.clone(),
        });
        if !stella_core::records::should_repropose(&decided, &proposal.candidate_id, observed_at) {
            withheld += 1;
            continue;
        }
        if proposal.dismissed_reason.is_some() {
            dismissed += 1;
        } else {
            eligible += 1;
            if proposal.record.tier() == PromotionTier::Pinned {
                pinned_chars += proposal.record.statement.chars().count();
            }
            if proposal
                .refutation
                .as_ref()
                .is_some_and(|r| r.verdict == Verdict::Refuted)
            {
                refuted += 1;
            }
        }
        file.proposals.push(proposal);
    }

    let written_to = write_proposals(root, &doc.rel, &file)?;
    let candidate_ids = file
        .proposals
        .iter()
        .map(|p| p.candidate_id.clone())
        .collect();
    Ok(DocSummary {
        written_to,
        total: file.proposals.len(),
        eligible,
        refuted,
        dismissed,
        withheld,
        pinned_chars,
        cost_usd,
        slices,
        failed_slices,
        duplicates,
        candidate_ids,
        asserted,
    })
}

/// What every slice of one document produced, merged.
struct Extracted {
    claims: Vec<Claim>,
    cost_usd: f64,
    slices: usize,
    failed_slices: Vec<(usize, String)>,
    duplicates: usize,
}

/// Divide the document and extract every slice, merging the claims.
///
/// A slice that fails does not abort the document — the other slices' claims are
/// real and already paid for — but it is carried out in [`Extracted::failed_slices`]
/// and reported, because the one thing this must never do is present a
/// partially-read document as a complete one. Every slice failing is the whole
/// document failing, and returns `Err`.
async fn extract_claims(
    provider: &dyn Provider,
    model_hint: &str,
    root: &Path,
    doc: &NamedDoc,
    progress: &Progress,
) -> Result<Extracted, String> {
    let (bounded, _dropped) = chunk::bound(&doc.content);
    let chunks = chunk::split(&bounded);
    let slices = chunks.len();
    if slices == 0 {
        return Err("the document has no text to extract".to_string());
    }

    let mut merge = Merge::default();
    let mut cost_usd = 0.0;
    let mut failed_slices = Vec::new();
    let mut first_error: Option<String> = None;

    for (index, chunk) in chunks.iter().enumerate() {
        let position = SlicePosition {
            index,
            of: slices,
            path: &chunk.path,
        };
        let label = slice_label(&doc.rel, position, merge.claims.len());
        progress.say(label.clone());
        match call_model(
            provider, model_hint, root, doc, chunk, position, &label, progress,
        )
        .await
        {
            Ok((claims, cost)) => {
                cost_usd += cost;
                merge.absorb(claims);
            }
            Err((cost, err)) => {
                // The spend is real whether or not the reply parsed, so it is
                // counted; the loss is named rather than absorbed.
                cost_usd += cost;
                progress.note(&format!(
                    "    {}",
                    format!("slice {} of {slices} failed: {err}", index + 1).red()
                ));
                first_error.get_or_insert_with(|| err.clone());
                failed_slices.push((index + 1, err));
            }
        }
    }

    if failed_slices.len() == slices {
        return Err(first_error.unwrap_or_else(|| "every slice failed".to_string()));
    }
    let (claims, duplicates) = merge.finish();
    Ok(Extracted {
        claims,
        cost_usd,
        slices,
        failed_slices,
        duplicates,
    })
}

/// Where a slice sits in its document, for the prompt and the progress line.
#[derive(Clone, Copy)]
struct SlicePosition<'a> {
    /// Zero-based.
    index: usize,
    of: usize,
    /// The heading path the slice opens under, outermost first.
    path: &'a [String],
}

/// Accumulates the claims of a document's slices, resolving the two collisions
/// that exist only because there is more than one call.
///
/// **The same claim, twice.** A rule stated in an overview section and restated
/// where it applies reaches two slices, and the extractor rightly emits it in
/// both. Two proposals for one claim is a review surface asking the same
/// question twice, so the later restatement is dropped. Statements are compared
/// case-folded and whitespace-collapsed, because that is the whole of the
/// difference between two renderings of one sentence.
///
/// **The same suffix, two claims.** The prompt asks for a slug "unique within
/// the document", which is exactly what a call reading one slice cannot
/// guarantee. `build_proposal` turns the suffix into `ctx.<set_id>.<suffix>`, so
/// a collision is two records with one identity. The first claim to use a suffix
/// keeps it — which is what makes a single-slice document byte-identical to
/// before this existed — and a later collision is numbered.
#[derive(Default)]
struct Merge {
    claims: Vec<Claim>,
    statements: HashSet<String>,
    suffixes: HashSet<String>,
    duplicates: usize,
}

impl Merge {
    fn absorb(&mut self, incoming: Vec<Claim>) {
        for mut claim in incoming {
            if !self.statements.insert(normalized(&claim.statement)) {
                self.duplicates += 1;
                continue;
            }
            claim.lineage_suffix = self.unique_suffix(&claim.lineage_suffix);
            self.claims.push(claim);
        }
    }

    /// The suffix this claim may keep. Compared in the kebab form
    /// `build_proposal` will slugify it to, so two suffixes that differ only in
    /// punctuation are caught here rather than colliding downstream.
    fn unique_suffix(&mut self, proposed: &str) -> String {
        let base = kebab(proposed);
        if self.suffixes.insert(base.clone()) {
            return base;
        }
        for n in 2.. {
            let candidate = format!("{base}-{n}");
            if self.suffixes.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!("the integers are not exhausted")
    }

    fn finish(self) -> (Vec<Claim>, usize) {
        (self.claims, self.duplicates)
    }
}

/// A statement reduced to what makes it the same claim: case-folded, with every
/// run of whitespace collapsed to one space.
fn normalized(statement: &str) -> String {
    statement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The file defaults every record in the document inherits.
fn build_defaults(
    root: &Path,
    doc: &NamedDoc,
    ingest_run_id: &str,
    extractor: &str,
    extracted_at: &str,
) -> Defaults {
    Defaults {
        sharing_scope: Some(stella_core::ingest::SharingScope::Repository),
        origin: Some(Origin::Imported),
        status: Some(stella_core::context_record::RecordStatus::Active),
        review_every: None,
        provenance: Some(Provenance {
            source_kind: Some("document".to_string()),
            source_uri: Some(doc.rel.clone()),
            source_digest: Some(digest_of(&doc.content)),
            source_lines: None,
            repo: git(root, &["remote", "get-url", "origin"]),
            commit: git(root, &["rev-parse", "--short", "HEAD"]),
            ingest_run_id: Some(ingest_run_id.to_string()),
            extracted_at: Some(extracted_at.to_string()),
            extractor: Some(extractor.to_string()),
        }),
    }
}

/// The floor for the output budget: 16k, not the 4k this used to carry. A
/// document near the old [`MAX_PROMPT_CHARS`] cap can atomize into dozens of
/// records, each carrying every optional field in the schema — 4k truncated
/// mid-object on real instruction files well before reaching the closing `]`,
/// which `parse_claims` then reported as a JSON syntax error rather than what it
/// actually was. 16k matches the ceiling `EngineConfig` already uses for the
/// same reason, and sits within every seeded catalog model's output limit.
const MIN_OUTPUT_TOKENS: u32 = 16_384;

/// The most this will ask any model to emit for one document.
///
/// The largest per-model ceiling in the seeded catalog is 384,000 and the
/// smallest is 30,000, so no single number is right everywhere; 128k is the
/// common ceiling among the frontier models people run extraction on, and a
/// provider that caps lower rejects or clamps the request rather than silently
/// producing less. Reaching this limit is reported, never absorbed.
const MAX_OUTPUT_TOKENS: u32 = 131_072;

/// The output budget to start with for `chars` characters of document.
///
/// One output token per input character, measured rather than guessed: 24,000
/// characters of this repository's `AGENTS.md` filled a 16,384-token budget
/// (`"finish_reason":"length"`) and finished inside 32,768. Every claim restates
/// its sentence and carries a dozen schema fields, so the reply is roughly the
/// size of the text that produced it.
///
/// Sizing the first request means a large document does not have to buy a
/// cut-off attempt before it is allowed a big enough one — the wasted attempt on
/// AGENTS.md cost $0.47 and 135 seconds and produced nothing. A short document
/// keeps the floor and is unaffected.
fn starting_output_budget(chars: usize) -> u32 {
    u32::try_from(chars)
        .unwrap_or(MAX_OUTPUT_TOKENS)
        .clamp(MIN_OUTPUT_TOKENS, MAX_OUTPUT_TOKENS)
}

/// Call the model for one slice, tolerating prose and one bad reply. Mirrors
/// `infer_domains`: bounded repair, then give up on this slice rather than
/// hammering.
///
/// The error carries the cost spent reaching it, because a failed slice is still
/// a paid call and a run that under-reports its own bill is the "measure
/// honestly" rule broken in the direction that flatters us.
#[allow(clippy::too_many_arguments)]
async fn call_model(
    provider: &dyn Provider,
    model_hint: &str,
    root: &Path,
    doc: &NamedDoc,
    chunk: &Chunk,
    position: SlicePosition<'_>,
    base_label: &str,
    progress: &Progress,
) -> Result<(Vec<Claim>, f64), (f64, String)> {
    let rel = &doc.rel;
    let user = format!(
        "Extract atomic context records from `{rel}`. Return ONLY the JSON array.\n\n\
         {}--- {rel} ---\n{}",
        slice_preamble(position),
        chunk.text
    );
    let mut messages = vec![
        CompletionMessage::system(SYSTEM_PROMPT),
        CompletionMessage::user(&user),
    ];
    let mut total_cost = 0.0;
    let mut max_output_tokens = starting_output_budget(chunk.text.chars().count());
    // Three, not two: the budget ladder and the malformed-JSON repair share this
    // counter, and with a right-sized first request a document large enough to
    // need a doubling should still have a repair left.
    const ATTEMPTS: usize = 3;

    for attempt in 0..ATTEMPTS {
        let request = CompletionRequest {
            messages: messages.clone(),
            max_output_tokens: Some(max_output_tokens),
            temperature: Some(0.0),
            effort: None,
            tools: Vec::new(),
            reasoning: None,
            params: None,
        };
        // The wait is narrated because it is long: this is a paid call that
        // runs for minutes on a real instruction file. The line is the caller's,
        // so the elapsed clock spans the whole document rather than resetting on
        // every slice and every retry — a clock that restarts hides exactly the
        // "how long has this really been going" the reader is asking.
        //
        // The first attempt keeps the label the caller already set, which
        // carries the running record count; a retry says so, because a silent
        // second attempt doubles both the wait and the spend.
        if attempt > 0 {
            progress.say(format!(
                "{base_label} — attempt {} of {ATTEMPTS}",
                attempt + 1
            ));
        }
        let outcome = crate::accounted_call::complete_standalone(
            root,
            provider,
            ModelCallRole::DomainInference,
            "ingest_extraction",
            model_hint,
            None,
            request,
        )
        .await;
        match outcome {
            Ok(accounted) => {
                total_cost += accounted.cost_usd;
                let cut_off = accounted.result.finish_reason == Some(FinishReason::Length);
                match parse_claims(&accounted.result.text) {
                    Ok(claims) => return Ok((claims, total_cost)),
                    // Out of attempts, or cut off with no room left to grant:
                    // either way there is nothing further to try, and the reason
                    // is named rather than left as a serde error.
                    Err(err)
                        if attempt + 1 == ATTEMPTS
                            || (cut_off && max_output_tokens >= MAX_OUTPUT_TOKENS) =>
                    {
                        return Err((
                            total_cost,
                            if cut_off {
                                format!(
                                    "the model's reply was cut off at {max_output_tokens} output \
                                     tokens before finishing this slice's record list — the \
                                     model is emitting more per character than the slice size \
                                     assumes"
                                )
                            } else {
                                format!("could not parse the model's records: {err}")
                            },
                        ));
                    }
                    // Cut off, not malformed: the reply was on track but ran out of
                    // room. Asking it to "respond with ONLY the JSON array" again
                    // wouldn't fix a token budget, and at temperature 0 would likely
                    // just truncate at the same point — give it more room instead
                    // and repeat the same request rather than re-litigating it.
                    // Clamped, because re-buying an identical call that already hit
                    // the ceiling spends real money to fail the same way (the arm
                    // above returns instead once there is no headroom left).
                    Err(_) if cut_off => {
                        let widened = max_output_tokens.saturating_mul(2).min(MAX_OUTPUT_TOKENS);
                        // Said out loud because it is the difference between a
                        // one-call wait and a two-call one, at twice the spend.
                        progress.note(&format!(
                            "    {}",
                            format!(
                                "the reply filled its {max_output_tokens}-token budget before \
                                 the record list ended — retrying with {widened}"
                            )
                            .yellow()
                        ));
                        max_output_tokens = widened;
                    }
                    Err(_) => {
                        // Feed the failure back once.
                        messages.push(CompletionMessage::assistant(&accounted.result.text));
                        messages.push(CompletionMessage::user(
                            "That was not a valid JSON array of records. Respond with ONLY the \
                             JSON array.",
                        ));
                    }
                }
            }
            Err(error) => {
                // The partial spend is already persisted by the accounting store,
                // and carried out so the document's reported bill includes the
                // slice that failed.
                return Err((total_cost, format!("model call failed: {}", error.message)));
            }
        }
    }
    // Unreachable: the final attempt always returns above. Kept total for the type.
    Err((total_cost, "extraction produced no records".to_string()))
}

/// The orienting header a slice carries, so a slice that opens mid-section is
/// not an unlabelled fragment.
///
/// Empty for a document that fits one call, which keeps that request
/// byte-identical to the one this repository has always sent — a single-slice
/// document must not pay for the machinery that only multi-slice documents need,
/// least of all in prompt-cache misses.
fn slice_preamble(position: SlicePosition<'_>) -> String {
    if position.of <= 1 {
        return String::new();
    }
    let where_ = if position.path.is_empty() {
        String::new()
    } else {
        format!(", under the section `{}`", position.path.join(" > "))
    };
    format!(
        "This is part {} of {} of the document{where_}. Extract only from the text below; \
         other parts are handled by their own calls.\n\n",
        position.index + 1,
        position.of
    )
}

/// What the progress line says while a slice is in flight.
///
/// Names the slice and the running record count, because "which of the five
/// calls am I paying for, and is it finding anything" is the question a person
/// watching a multi-minute ingest actually has. A single-slice document says
/// only what it used to.
fn slice_label(rel: &str, position: SlicePosition<'_>, found: usize) -> String {
    if position.of <= 1 {
        return format!("extracting {rel}");
    }
    format!(
        "extracting {rel} — part {} of {}, {found} record(s) so far",
        position.index + 1,
        position.of
    )
}

/// What the progress line says while a record is being gated and probed.
///
/// One line per record, naming it: the stretch after the model calls is
/// filesystem work with no network in it, and on a document that atomized into
/// hundreds of records it was the second-longest silence in the command.
fn record_label(rel: &str, suffix: &str, done: usize, total: usize) -> String {
    format!(
        "{rel}: processing {} — {} of {total} record(s), {} remaining",
        kebab(suffix),
        done + 1,
        total.saturating_sub(done + 1)
    )
}

/// Extract and parse the first JSON array in `text` (models add fences/prose).
fn parse_claims(text: &str) -> Result<Vec<Claim>, String> {
    let start = text.find('[').ok_or("no JSON array in reply")?;
    let end = text.rfind(']').ok_or("unterminated JSON array")?;
    if end <= start {
        return Err("malformed JSON array".to_string());
    }
    serde_json::from_str::<Vec<Claim>>(&text[start..=end]).map_err(|e| e.to_string())
}

/// Map one claim to a proposal: build the record, stamp it, gate it, probe it.
fn build_proposal(
    root: &Path,
    set_id: &str,
    defaults: &Defaults,
    observed_at: &str,
    eligibility: &str,
    claim: Claim,
) -> Proposal {
    let force = claim.force.unwrap_or_else(|| default_force(claim.kind));
    let mode = claim
        .enforcement_mode
        .unwrap_or_else(|| default_mode(claim.kind, &claim));
    let basis = claim.basis.unwrap_or_else(|| default_basis(claim.kind));
    let confidence = claim.confidence.unwrap_or(60).min(100);
    let suffix = kebab(&claim.lineage_suffix);
    let lineage_id = format!("ctx.{set_id}.{suffix}");

    // Probe-gating: keep only probes honored on an imported origin. A gated probe
    // (command_succeeds / http_ok) is dropped here and can never run.
    let honored_probe = claim
        .probe
        .clone()
        .filter(|p| gate::probe_honored(Origin::Imported, p.kind));

    let applies = applies_to(&claim);
    let steering = Steering {
        force,
        precedence: Some(precedence_for(force)),
        // Stamped explicitly even though it equals what `Record::tier` would
        // derive: explicit is what makes the classification visible in the
        // proposal TOML and overridable at review, and it stops a later force
        // edit from silently re-tiering the record (#2709).
        tier: Some(promotion_tier_for(force, applies.as_ref())),
        applies_to: applies,
    };
    let enforcement = Enforcement {
        mode,
        guard_tool: claim.guard_tool.clone(),
        guard_deny_command: claim.guard_deny_command.clone(),
        guard_deny_path: claim.guard_deny_path.clone(),
        severity: None,
        on_violation: None,
        check: None,
        rubric: None,
    };
    let truth = Truth {
        basis,
        confidence: Some(confidence),
        verified_by: None,
        verified_at: None,
        valid_from: None,
        ttl: None,
        on_expiry: None,
        review_every: None,
        probe: honored_probe.clone(),
    };
    let provenance = claim.source_lines.clone().map(|lines| Provenance {
        source_lines: Some(lines),
        ..Default::default()
    });

    let mut record = Record {
        lineage_id,
        record_id: None,
        record_hash: None,
        kind: claim.kind,
        statement: claim.statement.clone(),
        tags: Vec::new(),
        origin: None,
        sharing_scope: None,
        status: None,
        supersedes_record_id: None,
        provenance,
        steering: Some(steering),
        enforcement: Some(enforcement),
        truth: Some(truth),
        links: Vec::new(),
    };
    // A hash failure leaves the record unstamped; the proposal is still valid to
    // review, just without a derived id. It should not happen for a well-formed
    // record, and failing the whole extraction over it would be worse.
    let _ = record.stamp(defaults);

    let executable = claim
        .executable
        .as_deref()
        .map(|e| ("enforcement.autofix", e));
    let outcome = gate::gate_proposal(
        Origin::Imported,
        &claim.statement,
        claim.compound,
        executable,
    );

    // Only eligible proposals carry a refutation; a dismissed one already carries
    // its validation or quarantine finding.
    let refutation = outcome.is_eligible().then(|| match &honored_probe {
        Some(p) => probe::evaluate(root, p, observed_at),
        None => abstained(observed_at),
    });

    Proposal {
        candidate_id: candidate_id(&suffix, record.record_hash.as_deref()),
        proposal_kind: claim.kind.proposal_kind(),
        status: outcome.status,
        confidence,
        observed_at: observed_at.to_string(),
        eligibility: Some(eligibility.to_string()),
        dismissed_reason: outcome.dismissed_reason,
        record,
        refutation,
        quarantine: outcome.quarantine,
        validation: outcome.validation,
    }
}

/// The refutation for a claim with no runnable probe: honestly unfalsifiable, so
/// a reviewer knows its accuracy was never machine-checked.
fn abstained(checked_at: &str) -> Refutation {
    Refutation {
        verdict: Verdict::Unfalsifiable,
        checked_at: checked_at.to_string(),
        probe_kind: ProbeKind::None,
        detail: "no probe could judge this claim; measure compliance instead.".to_string(),
        recommend: None,
        links: Vec::new(),
    }
}

/// Write a document's proposals to `.stella/proposals/<source-slug>.toml`.
fn write_proposals(root: &Path, rel: &str, file: &ContextFile) -> Result<String, String> {
    let dir = root.join(".stella").join("proposals");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let name = format!(
        "{}.toml",
        kebab(rel.trim_end_matches(".md").trim_end_matches(".mdx"))
    );
    let path = dir.join(&name);
    let body =
        toml::to_string_pretty(file).map_err(|e| format!("cannot serialize proposals: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string()))
}

/// Print one document's result.
fn report(doc: &NamedDoc, summary: &DocSummary) {
    println!(
        "  {}  {}",
        doc.rel.green(),
        format!("→ {}", summary.written_to).dimmed()
    );
    let refuted = if summary.refuted > 0 {
        format!(", {} refuted as stale", summary.refuted)
            .yellow()
            .to_string()
    } else {
        String::new()
    };
    let dismissed = if summary.dismissed > 0 {
        format!(", {} dismissed", summary.dismissed)
    } else {
        String::new()
    };
    let across = if summary.slices > 1 {
        format!(" across {} parts", summary.slices)
    } else {
        String::new()
    };
    println!(
        "    {} {} record(s){}: {} eligible{}{}  {}",
        "·".dimmed(),
        summary.total,
        across,
        summary.eligible,
        refuted,
        dismissed,
        format!("(${:.4})", summary.cost_usd).dimmed()
    );
    // A slice that failed is a stretch of the document that produced no records.
    // Said in red and named, because the defect this whole path exists to fix is
    // a partially-read document reporting itself as a complete one (#2407).
    if !summary.failed_slices.is_empty() {
        let which: Vec<String> = summary
            .failed_slices
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        println!(
            "    {}",
            format!(
                "INCOMPLETE: part(s) {} of {} produced no records, so that text is not \
                 represented above. Re-run to retry them.",
                which.join(", "),
                summary.slices
            )
            .red()
        );
        for (n, err) in &summary.failed_slices {
            println!("      {}", format!("part {n}: {err}").dimmed());
        }
    }
    // Not a warning: a document that restates a rule where it applies is well
    // written, and one proposal per claim is the point. Counted so the arithmetic
    // between "records the model returned" and "proposals written" is visible.
    if summary.duplicates > 0 {
        println!(
            "    {}",
            format!(
                "{} restatement(s) merged — the same claim reached more than one part",
                summary.duplicates
            )
            .dimmed()
        );
    }
    // The "no gaps without bloat" half of the tier contract (#2709): pinned
    // records are guaranteed a prefix seat only while they fit the cached
    // budget, so an ingest that mints more pinned content than the budget
    // holds is told so HERE, where the classification can still be fixed at
    // review — not discovered later as a silently thinner prefix.
    if summary.pinned_chars > stella_core::records::CACHED_RECORD_BUDGET_CHARS {
        println!(
            "    {}",
            format!(
                "pinned overflow: this ingest's pinned records total {} characters, over \
                 the {}-character cached-prefix budget — the lowest-precedence ones will \
                 be dropped from the prefix at session open. Retier or trim them in \
                 `stella context review` before keeping.",
                summary.pinned_chars,
                stella_core::records::CACHED_RECORD_BUDGET_CHARS
            )
            .yellow()
        );
    }
    // Said out loud, because a claim that silently vanished from a re-run reads as
    // the extractor having missed it.
    if summary.withheld > 0 {
        println!(
            "    {}",
            format!(
                "{} claim(s) withheld — already decided in this workspace (declined within a \
                 cooldown, or already published). `stella context review` shows the decisions.",
                summary.withheld
            )
            .dimmed()
        );
    }
}

// ── Deterministic mapping helpers ────────────────────────────────────────────

/// The eligibility reason for a document's tier: named steering files carry
/// explicit instructions; everything else is imported prose.
fn eligibility_for(tier: Tier) -> &'static str {
    match tier {
        Tier::Primary | Tier::Instructional => "explicit_instruction",
        _ => "imported_document",
    }
}

/// The default steering force for a kind, when the model omits it.
fn default_force(kind: RecordKind) -> Force {
    match kind {
        RecordKind::Constraint | RecordKind::Rule => Force::Must,
        RecordKind::Procedure | RecordKind::Preference => Force::Should,
        RecordKind::Fact | RecordKind::Memory => Force::Info,
    }
}

/// The default enforcement mode: hard only when a real guard is present,
/// otherwise advisory for directives and none for facts.
fn default_mode(kind: RecordKind, claim: &Claim) -> EnforcementMode {
    if claim.guard_deny_command.is_some() || claim.guard_deny_path.is_some() {
        return EnforcementMode::Hard;
    }
    match kind {
        RecordKind::Fact | RecordKind::Memory => EnforcementMode::None,
        _ => EnforcementMode::Soft,
    }
}

/// The default truth basis: a directive from a document is a decree, a fact is
/// measured, softer kinds are asserted.
fn default_basis(kind: RecordKind) -> TruthBasis {
    match kind {
        RecordKind::Constraint | RecordKind::Rule | RecordKind::Procedure => TruthBasis::Decree,
        RecordKind::Fact => TruthBasis::Measured,
        RecordKind::Preference | RecordKind::Memory => TruthBasis::Asserted,
    }
}

/// The precedence a force implies, so conflicting records order deterministically.
///
/// Monotonic with [`Force::strength`], and a test pins that: `may` used to be
/// stamped *below* `info` here, which read as "an informational note outranks a
/// relevant one" everywhere precedence is consulted — silently, because the only
/// consumer at the time was conflict detection, which asks whether two
/// precedences are equal and never which is larger.
fn precedence_for(force: Force) -> u32 {
    match force {
        Force::Must => 100,
        Force::Should => 40,
        Force::May => 20,
        Force::Info => 15,
    }
}

/// The classifier-assigned promotion tier (#2709): binding forces pin, a
/// trigger scopes, everything else rides ordinary recall. Deterministic —
/// the model already chose the force and the scope, and the tier is a
/// restatement of those two, not a second model judgment.
fn promotion_tier_for(force: Force, applies_to: Option<&AppliesTo>) -> PromotionTier {
    if force.is_always_injected() {
        PromotionTier::Pinned
    } else if applies_to.is_some_and(|applies| !applies.is_empty()) {
        PromotionTier::Scoped
    } else {
        PromotionTier::Retrieved
    }
}

/// The record's scope, or `None` when it applies unconditionally.
fn applies_to(claim: &Claim) -> Option<AppliesTo> {
    let applies = AppliesTo {
        paths: claim.paths.clone(),
        tasks: claim.tasks.clone(),
        keywords: claim.keywords.clone(),
    };
    (!applies.is_empty()).then_some(applies)
}

/// The `<slug>-<hash8>` candidate id, stable across runs because it is derived
/// from the record's content hash. Falls back to the slug alone if the record
/// could not be stamped.
fn candidate_id(suffix: &str, record_hash: Option<&str>) -> String {
    match record_hash.and_then(|h| h.strip_prefix("sha256:")) {
        Some(hex) if hex.len() >= 8 => format!("{suffix}-{}", &hex[..8]),
        _ => suffix.to_string(),
    }
}

/// The record-set slug for this workspace: `org.repo` from the git remote when
/// resolvable, else the workspace directory name. Deterministic per checkout.
/// Shared with the promotion and mined-rule publication paths, so every record
/// a workspace emits lands in the same lineage namespace.
pub(crate) fn derive_set_id(root: &Path) -> String {
    if let Some(remote) = git(root, &["remote", "get-url", "origin"])
        && let Some(slug) = org_repo_from_remote(&remote)
    {
        return slug;
    }
    root.file_name()
        .map(|n| kebab_dots(&n.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string())
}

/// Parse `org.repo` out of a git remote URL (`git@github.com:acme/web.git` or
/// `https://github.com/acme/web`).
fn org_repo_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches(".git");
    let tail = trimmed.rsplit(':').next().unwrap_or(trimmed);
    let parts: Vec<&str> = tail.rsplit('/').take(2).collect();
    if parts.len() == 2 {
        let org = kebab_dots(parts[1]);
        let repo = kebab_dots(parts[0]);
        if !org.is_empty() && !repo.is_empty() {
            return Some(format!("{org}.{repo}"));
        }
    }
    None
}

/// Run `git` in `root`, returning trimmed stdout on success. Best-effort: any
/// failure (no git, no repo, no remote) is `None`, and provenance omits the field.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `sha256:<hex>` of a document's bytes, for the provenance digest.
fn digest_of(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

/// How many characters of `content` extraction will not read.
///
/// Almost always zero now: a document is divided rather than trimmed, so this is
/// non-zero only past [`MAX_DOCUMENT_CHARS`]. The caller asks rather than holding
/// a second copy of the ceiling.
pub(super) fn skipped_chars(content: &str) -> usize {
    chunk::dropped_chars(content)
}

/// Lower-case, dash-separated slug keeping `[a-z0-9-]`; runs of other characters
/// collapse to a single dash.
fn kebab(text: &str) -> String {
    slugify(text, '-')
}

/// Like [`kebab`] but keeps dots, so a `set_id` can be `org.repo`.
fn kebab_dots(text: &str) -> String {
    slugify(text, '-').replace('/', "-")
}

fn slugify(text: &str, sep: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_sep = false;
    for ch in text.chars() {
        let mapped = match ch {
            'A'..='Z' => ch.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '.' => ch,
            _ => sep,
        };
        if mapped == sep {
            if !last_was_sep && !out.is_empty() {
                out.push(sep);
            }
            last_was_sep = true;
        } else {
            out.push(mapped);
            last_was_sep = false;
        }
    }
    out.trim_end_matches(sep).to_string()
}

#[cfg(test)]
mod tests;
