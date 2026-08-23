//! Code-graph build cluster: workspace index construction, the eager semantic
//! embedding pass, and the session-lifetime live watcher.
use super::*;
use crate::search_cmd::readiness::IndexReadiness;

#[cfg(test)]
mod tests;

/// Build the workspace code-graph index into `.stella/private/codegraph.db` (the
/// `stella-graph` tree-sitter indexer). This is the data side of `init`: the
/// domain taxonomy tags graph nodes/edges, and the index makes the symbols +
/// import edges queryable as `ContextFrame`s by the context plane.
///
/// Idempotent and best-effort: a failure degrades to a warning (init still
/// succeeds, offline included) — the graph can always be rebuilt on a later
/// `init` once a toolchain/parser is available. Progress goes to `emit`
/// (plain text, no ANSI) so both the CLI and the deck transcript can show it.
///
pub(super) async fn build_code_graph(
    workspace_root: &std::path::Path,
    emit: &mut dyn FnMut(InitLine),
) {
    build_code_graph_with(
        workspace_root,
        &stella_embed::EmbedderEnv::from_process(),
        emit,
    )
    .await;
}

/// [`build_code_graph`] against an explicit embedder configuration.
///
/// The environment arrives as **data** rather than being read in here:
/// resolution is a pure function of it (`stella_embed::resolve`), so the
/// witness test drives exactly the code a real `stella init` runs — no
/// test-only branch, and no process-environment mutation racing every other
/// test in the binary.
async fn build_code_graph_with(
    workspace_root: &std::path::Path,
    embed_env: &stella_embed::EmbedderEnv,
    emit: &mut dyn FnMut(InitLine),
) {
    emit(InitLine::Step("◈ indexing code graph…".to_string()));
    // A full-tree tree-sitter index is seconds-to-minutes of blocking file
    // reads + parsing + SQLite on a large repo. Run it on the blocking pool
    // so it never pins a runtime worker — the deck driver awaits `/init`
    // inline and must stay responsive to queue edits and cancels meanwhile
    // (the incremental watcher path already does this, stella-graph
    // watch.rs). Progress crosses back over the boundary as throttled
    // transcript lines (#3102), so a long pass reads as work, not a wedge.
    let root = workspace_root.to_path_buf();
    let outcome = drive_index_blocking(root, INDEX_PROGRESS_INTERVAL, emit).await;
    match outcome {
        Ok(Ok(stats)) => {
            emit(InitLine::Step(format_graph_stats(&stats)));
            if let Some(line) = format_entry_points(&stats) {
                emit(InitLine::Step(line));
            }
        }
        Ok(Err(warning)) => emit(InitLine::Step(warning)),
        Err(e) => emit(InitLine::Step(format!(
            "! code-graph indexing task failed: {e} — run `stella init` again to retry"
        ))),
    }
    warm_semantic_index(workspace_root, embed_env, emit).await;
}

/// Minimum quiet time between narrated index/embedding progress lines. One
/// line a second is liveness without spam: a pass shorter than this narrates
/// nothing (the summary line stands alone), and a pass that looks wedged
/// proves it is not, once a second, in the transcript (#3102).
const INDEX_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// Time-based progress throttle: [`ready`](Self::ready) answers "is it time
/// for another line?". The first call arms the clock rather than firing, so
/// anything faster than the interval stays silent. Pure over the `now` it is
/// handed — the tests drive it with constructed instants.
pub(super) struct ProgressTicker {
    interval: Duration,
    last: Option<Instant>,
}

impl ProgressTicker {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// True when at least `interval` has passed since the last `true` (the
    /// first call arms and answers `false`).
    pub(super) fn ready(&mut self, now: Instant) -> bool {
        match self.last {
            None => {
                self.last = Some(now);
                false
            }
            Some(prev) if now.duration_since(prev) >= self.interval => {
                self.last = Some(now);
                true
            }
            Some(_) => false,
        }
    }
}

/// One narrated line of index progress — the live counts the maintainer
/// asked to watch increment (#3102). Running totals for THIS pass, straight
/// from the loop inside the transaction.
pub(super) fn format_index_progress(stats: &stella_graph::IndexStats) -> String {
    format!(
        "· {} files walked — {} parsed, {} unchanged, {} symbols…",
        stats.files_seen, stats.files_parsed, stats.files_skipped_unchanged, stats.symbols
    )
}

/// Run [`index_workspace_graph_blocking`] on the blocking pool while
/// streaming its throttled progress lines back to `emit` as they happen.
///
/// The pass runs inside one SQLite transaction on a blocking thread, and
/// `emit` lives on the async side (it may be the deck's non-`Send` closure)
/// — so progress crosses the boundary through a channel: the blocking side
/// sends already-formatted lines, this side forwards each to `emit` the
/// moment it arrives, and any lines still queued when the task finishes are
/// drained before the outcome is returned. Generic over the closure so the
/// session builder's `Send` closure keeps its task spawnable while `stella
/// init`'s plain printer needs no such bound.
async fn drive_index_blocking<F: FnMut(InitLine) + ?Sized>(
    root: std::path::PathBuf,
    interval: Duration,
    emit: &mut F,
) -> Result<Result<GraphSummary, String>, tokio::task::JoinError> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<InitLine>();
    let mut handle = tokio::task::spawn_blocking(move || {
        let mut ticker = ProgressTicker::new(interval);
        index_workspace_graph_blocking(&root, &mut |stats| {
            if ticker.ready(Instant::now()) {
                let _ = tx.send(InitLine::Progress(format_index_progress(stats)));
            }
        })
    });
    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some(line) => emit(line),
                // Channel closed: the blocking task dropped its sender, so
                // nothing more can arrive — await the outcome.
                None => return handle.await,
            },
            outcome = &mut handle => {
                // The task finished between polls; hand over what it queued.
                while let Some(line) = rx.recv().await {
                    emit(line);
                }
                return outcome;
            }
        }
    }
}

/// Embed the files just indexed, so `stella search` can rank by meaning on
/// its **first** invocation.
///
/// # Why eagerly, and why here
///
/// Nothing embeds on the query path any more (#4043): a search ranks over
/// what the index holds and never fills it. The two passes that do fill it
/// are this one and the background pass at session start
/// ([`crate::search_cmd::backfill`]), and this is the one that runs where the
/// user asked for it — `stella init` is the command whose whole job is to
/// make the workspace ready, so its work is free from every later session's
/// perspective.
///
/// # Why it is automatic rather than a flag
///
/// `stella init` is not a free command: it already resolves a provider and
/// spends a model call on domain inference, and already prints what that cost.
/// An embedding pass is the same kind of spend on the same command, so it
/// needs the same treatment — do it and say what it did — not a
/// flag that leaves the capability off for everyone who never found it. With
/// no embedder configured it is a labelled no-op, which is the normal case.
async fn warm_semantic_index(
    workspace_root: &std::path::Path,
    embed_env: &stella_embed::EmbedderEnv,
    emit: &mut dyn FnMut(InitLine),
) {
    use stella_embed::Embedder;

    use crate::search_cmd::semantic::NO_FILE_CEILING;

    let embedder = match stella_embed::resolve(embed_env) {
        stella_embed::Resolution::Configured(embedder) => embedder,
        // Named rather than silent: init is where a workspace is configured,
        // so it is the one moment a hint about an unused capability is help
        // instead of noise.
        stella_embed::Resolution::Unconfigured => {
            emit(InitLine::Step(
                "· semantic index: skipped — no embedding backend configured (set \
                 VOYAGE_API_KEY, OPENAI_API_KEY, or STELLA_EMBED_URL + STELLA_EMBED_MODEL to \
                 let `stella search` rank by meaning)"
                    .to_string(),
            ));
            return;
        }
        // A half-configured backend is neither off nor working, and a typo
        // must not read as a deliberate opt-out.
        stella_embed::Resolution::Incomplete(reason) => {
            emit(InitLine::Step(format!(
                "! semantic index: not built — {reason}"
            )));
            return;
        }
    };

    emit(InitLine::Step(
        "◈ embedding files for semantic search…".to_string(),
    ));
    let mut ticker = ProgressTicker::new(INDEX_PROGRESS_INTERVAL);
    let outcome = crate::search_cmd::semantic::warm_file_vectors_with_progress(
        workspace_root,
        embedder.as_ref(),
        NO_FILE_CEILING,
        &mut |embedded| {
            if ticker.ready(Instant::now()) {
                emit(InitLine::Progress(format!(
                    "· semantic index: {embedded} files embedded…"
                )));
            }
        },
    )
    .await;
    emit(InitLine::Step(format_warm_outcome(
        &outcome,
        &embedder.fingerprint().model_id,
    )));

    // The sharper rung the search ranks first (#3098): a file-level vector
    // exists but a query naming one function in a large multi-purpose file
    // loses to a file that happens to have complete per-symbol coverage,
    // regardless of which is the true answer. Same reasoning as the pass
    // above, one layer down.
    emit(InitLine::Step(
        "◈ embedding code chunks for search…".to_string(),
    ));
    let mut ticker = ProgressTicker::new(INDEX_PROGRESS_INTERVAL);
    let chunk_outcome = crate::search_cmd::engine::warm_chunk_vectors_with_progress(
        workspace_root,
        embedder.as_ref(),
        crate::search_cmd::engine::NO_CHUNK_FILE_CEILING,
        &mut |embedded| {
            if ticker.ready(Instant::now()) {
                emit(InitLine::Progress(format!(
                    "· chunk index: {embedded} files embedded…"
                )));
            }
        },
    )
    .await;
    emit(InitLine::Step(format_chunk_warm_outcome(
        &chunk_outcome,
        &embedder.fingerprint().model_id,
    )));
}

/// The one-line report of an eager embedding pass.
///
/// A partial index says so: a pass that gave up on a broken backend names how
/// many files were left unembedded, because an index that silently ranks a
/// subset of the repository is worse than one that says which subset it
/// ranked. The leftovers are not lost — the background pass at the next
/// session start picks them up.
///
/// Unless they cannot be read, which is a different sentence and gets one: no
/// later pass will pick those up, so "they embed later" would be a promise
/// this code cannot keep (#3016).
fn format_warm_outcome(outcome: &crate::search_cmd::semantic::WarmOutcome, model: &str) -> String {
    use crate::search_cmd::semantic::WarmOutcome;
    match outcome {
        WarmOutcome::Warmed {
            embedded,
            remaining: 0,
            ..
        } => format!("✓ semantic index: {embedded} files embedded by {model}"),
        WarmOutcome::Warmed {
            embedded,
            remaining,
            unreadable,
        } if *unreadable > 0 => format!(
            "! semantic index: {embedded} files embedded by {model} — {remaining} left \
             unembedded, {unreadable} of them because their content could not be read (the \
             next `stella init` drops files that have moved or vanished)"
        ),
        WarmOutcome::Warmed {
            embedded,
            remaining,
            ..
        } => format!(
            "✓ semantic index: {embedded} files embedded by {model} — {remaining} left \
             unembedded (the background pass at the next session start finishes them)"
        ),
        WarmOutcome::Failed { embedded, reason } => format!(
            "! semantic index: stopped after {embedded} files — {reason} (the background pass \
             at the next session start finishes the rest)"
        ),
    }
}

/// The one-line report of an eager chunk-embedding pass. Same shape and the
/// same reasoning as [`format_warm_outcome`], one rung sharper.
fn format_chunk_warm_outcome(
    outcome: &crate::search_cmd::engine::ChunkWarmOutcome,
    model: &str,
) -> String {
    use crate::search_cmd::engine::ChunkWarmOutcome;
    match outcome {
        ChunkWarmOutcome::Warmed {
            files_embedded,
            files_remaining: 0,
            ..
        } => format!("✓ chunk index: {files_embedded} file(s) embedded by {model}"),
        ChunkWarmOutcome::Warmed {
            files_embedded,
            files_remaining,
            unreadable,
        } if *unreadable > 0 => format!(
            "! chunk index: {files_embedded} file(s) embedded by {model} — {files_remaining} \
             left unembedded, {unreadable} of them because their content could not be read (the \
             next `stella init` drops files that have moved or vanished)"
        ),
        ChunkWarmOutcome::Warmed {
            files_embedded,
            files_remaining,
            ..
        } => format!(
            "✓ chunk index: {files_embedded} file(s) embedded by {model} — {files_remaining} \
             left unembedded (the background pass at the next session start finishes them)"
        ),
        ChunkWarmOutcome::Failed {
            files_embedded,
            reason,
        } => format!(
            "! chunk index: stopped after {files_embedded} file(s) — {reason} (the background \
             pass at the next session start finishes the rest)"
        ),
    }
}

/// Blocking: create `.stella/`, open the store, run one full incremental index
/// pass (sha-skip makes byte-identical files free, L-C2), and shut down.
/// Returns the stats or a ready-to-emit human warning. `progress` receives
/// the running per-file counts from inside the pass (#3102) — callers narrate
/// through [`drive_index_blocking`], which throttles and ferries the lines
/// across the async boundary; both `stella init`'s [`build_code_graph`] and
/// the session auto-builder [`spawn_session_graph`] drive it that way.
pub(super) fn index_workspace_graph_blocking(
    workspace_root: &std::path::Path,
    progress: &mut dyn FnMut(&stella_graph::IndexStats),
) -> Result<GraphSummary, String> {
    let db_path = stella_store::workspace_private_sqlite_path(workspace_root, "codegraph.db")
        .map_err(|e| format!("! could not prepare private code graph state: {e} — skipped"))?;
    let graph = stella_graph::CodeGraph::open(workspace_root, &db_path)
        .map_err(|e| format!("! code-graph store unavailable: {e} — skipped"))?;
    let stats = graph.index_all_with_progress(progress).map_err(|e| {
        format!("! code-graph indexing failed: {e} — run `stella init` again to retry")
    })?;
    // Report the whole-index TOTALS (what the model can actually query), not
    // this pass's parse delta: an incremental pass over an unchanged tree
    // re-parses nothing, so `stats.symbols`/`stats.imports` are 0 even though
    // the graph is fully populated — the misleading "0 symbols" line users saw.
    let summary = GraphSummary {
        total_symbols: graph.symbol_count().unwrap_or(0),
        total_imports: graph.import_count().unwrap_or(0),
        total_files: graph
            .file_count()
            .unwrap_or(stats.files_parsed + stats.files_skipped_unchanged),
        files_parsed: stats.files_parsed,
        files_unchanged: stats.files_skipped_unchanged,
        files_skipped_generated: stats.files_skipped_generated,
        files_skipped_too_large: stats.files_skipped_too_large,
        entry_points: graph.entry_points(ENTRY_POINT_SAMPLE).unwrap_or_default(),
    };
    graph.shutdown();
    Ok(summary)
}

/// How many entry points the orientation line names. Small on purpose: the
/// line is a starting point for a reader, not the roster — a tree with two
/// hundred uningested test files would otherwise print two hundred paths at
/// the end of every `stella init`. The graph answers the full question for
/// anyone who wants it.
const ENTRY_POINT_SAMPLE: usize = 5;

/// Whole-index totals plus this pass's parse/skip split, for the startup line.
pub(super) struct GraphSummary {
    pub(super) total_symbols: usize,
    pub(super) total_imports: usize,
    pub(super) total_files: usize,
    pub(super) files_parsed: usize,
    pub(super) files_unchanged: usize,
    /// Files this pass excluded as generated/minified (issue #272:
    /// `.gitattributes` `linguist-generated=true`, `*.min.*`, or the
    /// minified-content heuristic — see `stella_graph::generated`). Reported
    /// separately from `total_files` so the exclusion is visible, not just
    /// silently absent from the count.
    pub(super) files_skipped_generated: usize,
    /// Files this pass excluded for exceeding the indexer's 4 MiB ceiling (a
    /// memory bound on tree-sitter, whose syntax tree runs several times the
    /// size of its source). Same reason the generated count is here: a user
    /// whose file left the index otherwise gets no signal at all, and reads
    /// the graph's silence about it as the file having no symbols.
    pub(super) files_skipped_too_large: usize,
    /// Up to [`ENTRY_POINT_SAMPLE`] indexed files nothing in the index
    /// imports, shallowest path first — binaries, scripts, tests, dead code.
    /// The orientation half of the summary: the totals say how much was
    /// indexed, this says where a reader starts.
    ///
    /// Empty when the index is empty, and also when every indexed file is
    /// imported by another — a real answer in a tree with one cyclic module
    /// graph, not a failure to compute one.
    pub(super) entry_points: Vec<String>,
}

/// The `✓ code graph: N symbols, M imports…` summary line, shared by `stella
/// init` and the session auto-builder so both surfaces read identically.
/// Reports index totals; the parenthetical is this pass's parse/skip split.
/// Each exclusion this pass made gets its own trailing clause — "skipped N
/// generated files" (issue #272), "skipped N files over the size limit" — so
/// a file that left the index says so, instead of silently vanishing from the
/// count and reading as a file with no symbols in it.
pub(super) fn format_graph_stats(summary: &GraphSummary) -> String {
    let base = format!(
        "✓ code graph: {} symbols, {} imports across {} file{} ({} re-parsed, {} unchanged this pass)",
        summary.total_symbols,
        summary.total_imports,
        summary.total_files,
        if summary.total_files == 1 { "" } else { "s" },
        summary.files_parsed,
        summary.files_unchanged,
    );
    let clauses: Vec<String> = [
        (summary.files_skipped_generated, "generated file"),
        (summary.files_skipped_too_large, "file over the size limit"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, noun)| {
        // "file over the size limit" pluralizes on the noun, not the tail.
        let plural = noun.replacen("file", "files", 1);
        format!(
            "skipped {count} {}",
            if count == 1 { noun } else { &plural }
        )
    })
    .collect();
    if clauses.is_empty() {
        return base;
    }
    format!("{base} — {}", clauses.join(", "))
}

/// The `· start here: …` line that follows [`format_graph_stats`], naming the
/// files nothing in the index imports. `None` when the index found none,
/// because a line reading `start here:` with nothing after it tells a reader
/// less than no line at all.
///
/// Deliberately its own line rather than another clause on the stats line:
/// the totals answer "how much is indexed" and this answers "where do I
/// start", and a reader scanning `stella init`'s output for the second
/// should not have to find it at the end of the first.
pub(super) fn format_entry_points(summary: &GraphSummary) -> Option<String> {
    if summary.entry_points.is_empty() {
        return None;
    }
    Some(format!(
        "· start here: {} — nothing in the index imports {}",
        summary.entry_points.join(", "),
        if summary.entry_points.len() == 1 {
            "it"
        } else {
            "them"
        }
    ))
}

/// A session-lifetime holder for the live code graph. It keeps the in-process
/// `notify` watcher (and its debounce task) alive so file changes — the
/// agent's own edits and external ones — incrementally re-index into
/// `.stella/private/codegraph.db` for the rest of the session. Dropping it (or calling
/// [`SessionGraph::shutdown`]) tears the watcher down cleanly. The mounted
/// graph is installed only once the background build finishes, so an early
/// session exit simply leaves the slot empty (and the never-installed watcher
/// never armed).
pub(crate) struct SessionGraph {
    graph: Arc<std::sync::Mutex<Option<stella_graph::CodeGraph>>>,
}

impl SessionGraph {
    /// Stop the watcher and its background tasks. Idempotent; also runs on drop.
    pub(crate) fn shutdown(&self) {
        if let Some(graph) = self.graph.lock().unwrap_or_else(|p| p.into_inner()).take() {
            graph.shutdown();
        }
    }
}

impl Drop for SessionGraph {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Ensure the workspace code-graph index exists and stays fresh for the life of
/// a session — WITHOUT blocking startup and WITHOUT re-running the full,
/// LLM-driven [`init_workspace`]. This is the *data* side of init only:
///
/// 1. If there is no index yet it is built in the background
///    ([`index_workspace_graph_blocking`], the same index step `stella init`
///    runs); if one already exists it is a cheap incremental catch-up
///    (byte-identical files are skipped, L-C2). The finished index is what
///    `stella search` ranks over and the deck's Graph tab renders.
/// 2. The live `notify` watcher is then armed via
///    [`stella_graph::CodeGraph::mount`] so subsequent edits incrementally
///    re-index. mount's own catch-up sha-skips everything just indexed in
///    step 1 — the watcher is the point of the second open.
///
/// Non-blocking: returns immediately with a [`SessionGraph`] the caller keeps
/// alive for the session (dropping it stops the watcher) and the setup task's
/// `JoinHandle`, which completes once the index build has settled — a
/// deterministic "index ready" signal for tests. `status` receives the same
/// `◈ indexing code graph…` / `✓ …` lines `stella init` prints (route it to
/// stderr or the deck transcript, never to a machine-readable stdout);
/// `on_ready` fires once after the build (the deck refreshes its Graph tab
/// there; other callers pass a no-op).
///
/// `readiness` receives the semantic index's coverage as the background
/// embedding pass fills it, and once more marked settled when that pass
/// stops. It exists for the interactive prompt gate (#4043), so **only the
/// deck passes a real one**: the headless doors (`stella run`, `stella
/// resume`, the plain REPL) take a goal that is already typed, and holding it
/// would turn a script into a hang. Their answer is the same one a partial
/// index has always given — rank what is there, say how much that was.
/// Report — or, with `prune`, reclaim — semantic vectors left behind by an
/// embedding model this workspace no longer uses (#3652).
///
/// Reporting is the default and sweeping is the flag, which is the opposite
/// of what "leaked rows" usually warrants. The reason is that these rows are
/// not leaked by accident: vectors are keyed by embedder fingerprint
/// specifically so two models never share a vector space, and the same key is
/// what lets a user switch back to a previous model and reuse its vectors for
/// free. Sweeping on every init would quietly bill a full re-embed to anyone
/// evaluating two embedders against each other — so `stella init` states the
/// cost and leaves the decision to whoever is paying it.
///
/// Silent when no embedder is configured: without an active fingerprint there
/// is no way to say which of the stored ones is retired, and guessing here
/// would delete the wrong corpus.
pub(super) fn report_retired_vectors(
    workspace_root: &std::path::Path,
    prune: bool,
    emit: &mut dyn FnMut(String),
) {
    use stella_embed::Embedder;

    let stella_embed::Resolution::Configured(embedder) =
        stella_embed::resolve(&stella_embed::EmbedderEnv::from_process())
    else {
        return;
    };
    let active = embedder.fingerprint().id();

    let Ok(db_path) = stella_store::workspace_private_sqlite_path(workspace_root, "codegraph.db")
    else {
        return;
    };
    let Ok(graph) = stella_graph::CodeGraph::open(workspace_root, &db_path) else {
        return;
    };
    let retired = graph
        .retired_vector_fingerprints(&active)
        .unwrap_or_default();
    if retired.is_empty() {
        graph.shutdown();
        return;
    }

    let rows: usize = retired.iter().map(|(_, count)| count).sum();
    if !prune {
        emit(format!(
            "· semantic index: {rows} vector(s) from {} retired embedding model(s) are still \
             stored — they are reused for free if you switch back; run `stella init \
             --prune-vectors` to reclaim the space",
            retired.len()
        ));
        graph.shutdown();
        return;
    }

    let mut removed = 0usize;
    for (fingerprint, _) in &retired {
        removed += graph
            .prune_vector_fingerprint(fingerprint, &active)
            .unwrap_or(0);
    }
    emit(format!(
        "· semantic index: reclaimed {removed} vector(s) from {} retired embedding model(s)",
        retired.len()
    ));
    graph.shutdown();
}

/// Fill the semantic index in the background, for the whole session's
/// benefit and off every query's critical path (#4043, replacing #3649's
/// bounded top-up).
///
/// # Why it runs unasked, and every launch
///
/// Its predecessor skipped a workspace with no vectors at all, on the
/// argument that embedding a tree the user never asked for spends their
/// money. That argument held while a search would eventually fill the index
/// itself; with the lazy per-query pass deleted, "skip it" now means "this
/// workspace has no semantic search, ever, and nothing will tell you why". So
/// the opt-in signal is now the embedder being configured at all — a key the
/// user set, for a capability whose entire purpose is this index. An
/// unconfigured host still does nothing and says nothing.
///
/// Every launch, because the pass is *cheap when there is nothing to do*: it
/// asks the graph what is pending and finds nothing. The first launch in a
/// workspace is the expensive one, which is exactly the one
/// [`crate::search_cmd::readiness`] holds the first prompt for.
///
/// # Narration
///
/// Deliberately quieter than `stella init`'s pass. Init is where a workspace
/// is *configured*, so naming an absent embedder there is help; naming it on
/// every session start is nagging about a capability the user has already
/// chosen not to use. Only a pass that embedded something — or failed while
/// trying — is worth a line.
///
/// Both callbacks keep their `Send` bound deliberately: this runs inside the
/// `tokio::spawn`ed session task, and a bare `&mut dyn FnMut(_)` would make
/// the whole future non-`Send` — the same trap `drive_index_blocking`
/// documents above.
async fn backfill_vectors_quietly(
    root: &std::path::Path,
    status: &mut (dyn FnMut(InitLine) + Send),
    readiness: &mut (dyn FnMut(IndexReadiness) + Send),
) {
    use crate::search_cmd::backfill::{BackfillOutcome, backfill_workspace_vectors};
    use crate::search_cmd::semantic::WarmOutcome;

    let stella_embed::Resolution::Configured(embedder) =
        stella_embed::resolve(&stella_embed::EmbedderEnv::from_process())
    else {
        return;
    };

    let mut ticker = ProgressTicker::new(INDEX_PROGRESS_INTERVAL);
    // Every tick is the pass saying "still filling"; the settled report is
    // this function's last act, below, and belongs to nobody else — a pass
    // that declared itself settled from the inside would release the prompt
    // gate one batch before it was true.
    let outcome = backfill_workspace_vectors(root, embedder.as_ref(), &mut |measured| {
        readiness(measured);
        if ticker.ready(Instant::now()) {
            status(InitLine::Progress(format!(
                "· semantic index: {} of {} files embedded…",
                measured.indexed_files(),
                measured.total_files
            )));
        }
    })
    .await;

    match &outcome {
        BackfillOutcome::Ran {
            files: WarmOutcome::Warmed { embedded, .. },
            ..
        } if *embedded > 0 => {
            status(InitLine::Step(format!(
                "✓ semantic index: {embedded} file(s) embedded — search ranks the whole \
                 workspace by meaning"
            )));
        }
        BackfillOutcome::Ran {
            files: WarmOutcome::Failed { reason, .. },
            ..
        } => {
            status(InitLine::Step(format!(
                "! semantic index: the background pass stopped — {reason} (search still ranks \
                 over the vectors already stored, and says how many that is)"
            )));
        }
        BackfillOutcome::Unavailable(reason) => {
            status(InitLine::Step(format!(
                "! semantic index: not filled — {reason}"
            )));
        }
        // Nothing was pending, or another session holds the lease and is
        // doing this work. Neither is news.
        BackfillOutcome::Ran { .. } | BackfillOutcome::Busy => {}
    }

    // Settled, whatever happened — including a failure. A gate that outlives
    // the pass behind it is a wedge: a workspace whose embedder is down must
    // still take prompts (`search_cmd::readiness`).
    readiness(settled_readiness(root, embedder.as_ref()));
}

/// The workspace's index coverage, marked settled — what the prompt gate
/// reads once the background pass has stopped.
///
/// Opens its own connection rather than borrowing the pass's: the pass may
/// have failed before it opened one at all, and this report is owed either
/// way. An unopenable graph reports [`IndexReadiness::unknown`], which holds
/// nothing — the direction `readiness::measure` argues for.
fn settled_readiness(
    root: &std::path::Path,
    embedder: &dyn stella_embed::Embedder,
) -> IndexReadiness {
    let Ok(db_path) = stella_store::workspace_private_sqlite_path(root, "codegraph.db") else {
        return IndexReadiness::unknown();
    };
    let Ok(graph) = stella_graph::CodeGraph::open(root, &db_path) else {
        return IndexReadiness::unknown();
    };
    let measured =
        crate::search_cmd::readiness::measure(&graph, &embedder.fingerprint().id(), true);
    graph.shutdown();
    measured
}

pub(crate) fn spawn_session_graph(
    workspace_root: &std::path::Path,
    mut status: Box<dyn FnMut(InitLine) + Send>,
    on_ready: Box<dyn FnOnce() + Send>,
    mut readiness: Box<dyn FnMut(IndexReadiness) + Send>,
) -> (SessionGraph, tokio::task::JoinHandle<()>) {
    let slot: Arc<std::sync::Mutex<Option<stella_graph::CodeGraph>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot_task = slot.clone();
    let root = workspace_root.to_path_buf();
    let handle = tokio::spawn(async move {
        // 1) Build (fresh) or incrementally refresh (existing) the index to
        //    completion, on the blocking pool, with the same throttled
        //    progress lines `stella init` prints (#3102) — `status` is a
        //    `Send` box, so [`drive_index_blocking`]'s future stays `Send`
        //    and this task stays spawnable. (We drive the shared helper
        //    rather than `build_code_graph`, whose `&mut dyn FnMut` emit is
        //    not `Send` and whose embedding pass a session start skips.)
        status(InitLine::Step("◈ indexing code graph…".to_string()));
        let outcome =
            drive_index_blocking(root.clone(), INDEX_PROGRESS_INTERVAL, &mut status).await;
        match outcome {
            Ok(Ok(stats)) => {
                status(InitLine::Step(format_graph_stats(&stats)));
                if let Some(line) = format_entry_points(&stats) {
                    status(InitLine::Step(line));
                }
            }
            Ok(Err(warning)) => status(InitLine::Step(warning)),
            Err(e) => status(InitLine::Step(format!(
                "! code-graph indexing task failed: {e} — search ranks over the last good index \
                 this session"
            ))),
        }
        on_ready();
        // 2) Arm the live watcher on a mounted graph kept alive for the
        //    session. Best-effort: a mount failure only loses live refresh, it
        //    never loses the index built in step 1.
        //
        //    **Before** the embedding pass, not after (#4043). While that pass
        //    was bounded to a couple of hundred files this ran second and cost
        //    a second or two of live-refresh; the pass is unbounded now, so
        //    ordering it first would leave a cold monorepo with no watcher for
        //    however long a whole-tree embed takes — and the agent's own edits
        //    unindexed for all of it. The two overlap safely: they are separate
        //    SQLite connections writing different tables, and the embed lease
        //    is what keeps two embedding passes apart.
        //    Resolved through the same rule the index pass above used, so a
        //    session whose private state is redirected cannot build in one
        //    database and watch another (#4394).
        let mounted = match crate::search_cmd::codegraph::graph_db_path(&root) {
            Ok(Some(db_path)) => stella_graph::CodeGraph::mount(&root, &db_path)
                .await
                .map_err(|e| e.to_string()),
            // The build in step 1 creates the index, so reaching here means it
            // failed and has already said so — there is nothing to watch.
            Ok(None) => Err("no index was built this session".to_owned()),
            Err(error) => Err(error.to_string()),
        };
        match mounted {
            Ok(graph) => {
                *slot_task.lock().unwrap_or_else(|p| p.into_inner()) = Some(graph);
            }
            Err(e) => status(InitLine::Step(format!(
                "! code-graph watcher unavailable: {e} — the index will refresh on the next launch"
            ))),
        }
        // 3) Fill the semantic index (#4043). The index pass above has just
        //    stamped every changed file's `indexed_at`, and `vectors::pending`
        //    orders on exactly that, so this pass covers the commit or merge
        //    this session started after before it reaches the long-stable
        //    tail. Nothing else fills the index any more — the per-query
        //    catch-up inside `search` is deleted — so this runs to exhaustion
        //    rather than to a cap, and reports its coverage as it goes so the
        //    prompt gate can hold the first turn while a cold workspace fills.
        //
        //    Last on purpose: everything above it is what makes the session
        //    usable, and this is the part that can take minutes. It is
        //    single-flight across processes (the embed lease), and a host with
        //    no embedder configured does nothing at all.
        backfill_vectors_quietly(&root, &mut status, &mut readiness).await;
    });
    (SessionGraph { graph: slot }, handle)
}
