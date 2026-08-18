//! The `stella init` flow — domain-taxonomy resolution (with the #3102
//! inference-skip gate), the code-graph build, extension sync, and the
//! shared [`init_workspace`] entry that `stella init` and the deck's `/init`
//! command both drive.
//!
//! Progress is a transcript, not an animation (#3102): `init` on a large
//! workspace does enough work to look wedged, and a spinner is
//! indistinguishable from a wedge. Every stage narrates itself through
//! `emit` as it happens — live counts from inside the index pass, batch
//! counts from the embedding passes, and an explicit line when a model call
//! is spent or skipped — so what the user reads afterwards is a record of
//! what ran.

use std::path::PathBuf;

use super::graph::build_code_graph;
use super::*;
use crate::domains::{cached_taxonomy, summarize_repo};
use crate::interactive::{AskUserIo, TtyAskUserIo};

/// How init talks to the user: the progress transcript it writes, and the
/// questions it is allowed to ask.
///
/// These travel together because they are two halves of one thing — the
/// surface the human is looking at — and every surface answers both at once.
/// The plain CLI prints to stdout and reads stdin; the Command Deck routes
/// both through its own channels, and a `TtyAskUserIo` there would print
/// straight through the full-screen render. Passing them as one injected
/// seam is also what keeps `init_workspace`'s arity fixed, which matters
/// concretely: both of its interactive call sites live in files closed to
/// growth (`agent.rs`, `command_deck.rs`).
///
/// `ask` is `None` exactly when nobody can answer — a piped or redirected
/// run. Init never derives that itself; the surface knows, and a second
/// derivation is how two consumers end up disagreeing about whether anyone
/// is listening.
///
/// `user_root` is the user-global config root (`~/.stella`) init is allowed to
/// touch, and it rides here for the same reason `ask` does: init must not
/// derive it. What this feeds — the first-session conversion offer — *writes
/// files* into that root, so resolving it inside `init_workspace` made the
/// blast radius an environment the caller inherits rather than a value it
/// chooses, and no test could drive the accepting path without converting the
/// developer's real `~/.stella/commands/` (#3641). The production constructors
/// resolve it once, in the open; [`InitIo::scoped`] names it.
pub(crate) struct InitIo<'a> {
    emit: Box<dyn FnMut(InitLine) + 'a>,
    ask: Option<&'a dyn AskUserIo>,
    user_root: Option<PathBuf>,
}

impl<'a> InitIo<'a> {
    /// An io over a caller-supplied transcript sink and question channel,
    /// against the real user-global config root.
    ///
    /// The sink takes an [`InitLine`], not a `String`: a surface has to render
    /// a live counter differently from a permanent step, and collapsing the
    /// two here would put back the flood [`InitLine`] exists to stop.
    pub(crate) fn new(emit: impl FnMut(InitLine) + 'a, ask: Option<&'a dyn AskUserIo>) -> Self {
        Self {
            emit: Box::new(emit),
            ask,
            user_root: crate::extensions::user_config_root(),
        }
    }

    /// An io whose every root is named by the caller — nothing is read from
    /// the environment.
    ///
    /// Test-only, and that is the point: it is what makes an end-to-end init
    /// test possible at all, because the production constructors resolve a
    /// real home no test run may write to (#3641).
    #[cfg(test)]
    pub(crate) fn scoped(
        emit: impl FnMut(InitLine) + 'a,
        ask: Option<&'a dyn AskUserIo>,
        user_root: Option<PathBuf>,
    ) -> Self {
        Self {
            emit: Box::new(emit),
            ask,
            user_root,
        }
    }

    /// Narrate one line, whichever kind it is.
    pub(crate) fn emit(&mut self, line: InitLine) {
        (self.emit)(line);
    }

    /// Narrate one permanent line of the record.
    pub(crate) fn step(&mut self, text: String) {
        self.emit(InitLine::Step(text));
    }

    /// The raw sink, for a stage that narrates both kinds itself — the
    /// code-graph build is the only one, and its counters are the reason the
    /// distinction exists.
    pub(crate) fn sink(&mut self) -> &mut (dyn FnMut(InitLine) + 'a) {
        &mut *self.emit
    }

    /// A plain `FnMut(String)` view for the stages whose every line is part
    /// of the record. Naming that once here beats making each of the three
    /// call sites wrap its own closure.
    pub(crate) fn step_sink(&mut self) -> impl FnMut(String) + '_ {
        move |text: String| (self.emit)(InitLine::Step(text))
    }

    /// The plain-CLI io: the indented stdout transcript both `stella init`
    /// and the non-deck REPL's `/init` print, with TTY questions enabled
    /// only when [`crate::interactive::human_is_present`] says someone can
    /// answer them.
    pub(crate) fn stdout_tty() -> InitIo<'static> {
        let ask: Option<&'static dyn AskUserIo> =
            crate::interactive::human_is_present(true).then_some(&TtyAskUserIo);
        InitIo::new(stdout_narrator(), ask)
    }
}

/// One narrated line of init, carrying whether it is part of the **record** or
/// part of the **liveness**.
///
/// Both used to be the same thing — a `String` appended wherever it landed —
/// and the liveness half won: a large workspace ticks its three long passes
/// (the code-graph walk, then the file and chunk embedding passes) once a
/// second for minutes, so the ✓ summaries that say what init actually did
/// arrived buried under a hundred near-identical `· chunk index: N files
/// embedded…` lines. The counter still has to be *live* — that is the whole
/// reason #3102 replaced the spinner with real counts — so it stays, and only
/// its accumulation goes: each surface rewrites the previous tick.
///
/// The distinction is in the type rather than in a prefix a surface could
/// sniff (`starts_with("· ")`), because a caller adding a fourth long pass
/// should have to answer which kind of line it emits.
pub(crate) enum InitLine {
    /// A permanent line: appended, and read afterwards as the record of init.
    Step(String),
    /// A live counter: replaces the previous `Progress` line rather than
    /// appending. The last tick of a pass stays on screen — a counter that
    /// erased itself would leave nothing to read, which is the failure #3102
    /// diagnosed in the cinematic it retired.
    Progress(String),
}

impl InitLine {
    /// The line's text, whichever kind it is — for a test asserting on what
    /// was said rather than on how it was shown.
    ///
    /// Test-only on purpose: every shipping surface has to render the two
    /// kinds differently, and an accessor that discards the distinction is
    /// exactly the shortcut that would put the flood back.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Step(text) | Self::Progress(text) => text,
        }
    }
}

/// The domain step of init, wearing the #3102 inference-skip gate: a
/// model-inferred taxonomy whose recorded repo-shape fingerprint still
/// matches the tree is reused **without a model call** — the cost the gate
/// exists to stop is re-buying an unchanged answer on every `stella init`
/// (observed: $0.034 on a run whose index pass re-parsed zero files).
/// Every other case pays: no taxonomy yet, a changed repo shape, or a
/// heuristic taxonomy a provider run should upgrade. Offline, the
/// deterministic directory heuristic answers as before.
pub(crate) async fn resolve_domains(
    provider: Option<&dyn Provider>,
    workspace_root: &std::path::Path,
    model_hint: Option<&str>,
    budget_limit: Option<f64>,
    emit: &mut dyn FnMut(String),
) -> (Domains, f64) {
    let Some(provider) = provider else {
        return (heuristic_domains(workspace_root), 0.0);
    };
    let summary = summarize_repo(workspace_root);
    if let Some(existing) = cached_taxonomy(workspace_root, &summary) {
        emit(format!(
            "✓ domain taxonomy: repo shape unchanged — reusing .stella/domains.toml \
             ({} domains, no model call; delete the file to force re-inference)",
            existing.domains.len()
        ));
        return (existing, 0.0);
    }
    let model = model_hint
        .or_else(|| provider.model())
        .unwrap_or(UNKNOWN_MODEL);
    emit(format!("◈ inferring domain taxonomy with {model}…"));
    infer_domains(provider, workspace_root, model, budget_limit).await
}

/// The shared init flow behind `stella init` and the `/init` chat command:
/// infer the domain taxonomy (model-assisted when a provider is available,
/// directory heuristic otherwise, reused without a call when the repo shape
/// is unchanged — see [`resolve_domains`]), build the code-graph index,
/// persist `.stella/domains.toml`, and record the taxonomy into the context
/// plane. Progress lines stream to `emit` — the CLI prints them, the deck
/// forwards them into the transcript — so both surfaces share one
/// implementation.
pub(crate) async fn init_workspace(
    provider: Option<&dyn Provider>,
    workspace_root: &std::path::Path,
    model_hint: Option<&str>,
    budget_limit: Option<f64>,
    io: &mut InitIo<'_>,
) -> Result<(Domains, f64), String> {
    // The stages that narrate nothing live keep the plain `String` sink:
    // every line they emit is part of the record, so the wrapper naming that
    // once is better than making each call site repeat it.
    let (domains, inference_cost_usd) = {
        let mut step = io.step_sink();
        resolve_domains(
            provider,
            workspace_root,
            model_hint,
            budget_limit,
            &mut step,
        )
        .await
    };

    // The code graph needs no provider — build it regardless of how the
    // domains were inferred, so the index exists even fully offline.
    build_code_graph(workspace_root, io.sink()).await;

    // Adopt commands/skills/agents other code agents keep in `.claude/` and
    // `.agents/` (workspace + user scope) as symlinks into stella's own
    // directories — idempotent, never clobbers, never fatal.
    {
        let mut step = io.step_sink();
        crate::extensions::sync_extensions(workspace_root, &mut step);
    }

    // Then, once — and only once per workspace — offer to convert the
    // markdown definitions just adopted into TOML, the format the rest of
    // stella's config speaks. This runs AFTER the sync deliberately: the
    // definitions worth converting are largely the ones the sync just
    // linked, and asking before they exist would spend the single ask this
    // workspace gets on an empty directory. It never converts on its own —
    // see `crate::commands_offer` for why that trade stays the user's.
    {
        // Read the question channel and the user root out before borrowing
        // the sink: the offer needs all three halves of the io at once, and
        // only one of them mutably.
        let ask = io.ask;
        let user_root = io.user_root.clone();
        let mut step = io.step_sink();
        crate::commands_offer::offer_conversion(
            workspace_root,
            user_root.as_deref(),
            ask,
            &mut step,
        )
        .await;
    }

    let path = domains.save(workspace_root)?;

    // Persist the taxonomy into the context plane too: domain descriptions
    // plus bi-temporal `covers_path` facts, so recall can fuse on them and a
    // re-run after the taxonomy shifts supersedes (never deletes) the old
    // beliefs. Best-effort — a store that won't open already warned.
    if let Some(m) = SessionMemory::open(workspace_root, false) {
        m.record_taxonomy(&domains).await;
    }

    io.step(format!(
        "✓ {} domains ({}) → {}",
        domains.domains.len(),
        domains.inferred_by,
        path.display()
    ));
    if inference_cost_usd > 0.0 {
        io.step(format!(
            "domain inference model cost: ${inference_cost_usd:.6}"
        ));
    }
    Ok((domains, inference_cost_usd))
}

/// The plain-stdout narrator both non-deck init paths hand to
/// [`init_workspace`] — `stella init` and the plain REPL's `/init`.
///
/// An [`InitLine::Progress`] counter rewrites its own line where the terminal
/// can take one back, and is simply appended where it cannot: a redirected
/// `stella init` log has to stay readable, and a `\r` in a file is not. Either
/// way the last tick survives — nothing here erases what it drew, which is the
/// failure #3102 diagnosed in the cinematic it retired.
pub(crate) fn stdout_narrator() -> impl FnMut(InitLine) + Send {
    narrator(false)
}

/// [`stdout_narrator`] against stderr — what the session-startup index build
/// uses, so its progress never lands in a machine-readable stdout
/// (`crate::agent::spawn_session_graph`).
pub(crate) fn stderr_narrator() -> impl FnMut(InitLine) + Send {
    narrator(true)
}

/// The Command Deck's narrator for `/init`: a step joins `agent`'s transcript,
/// a counter rewrites the counter before it.
///
/// A step carries its own terminator because the transcript fold coalesces
/// consecutive `Text` verbatim — without one, init's stages run together into a
/// single paragraph.
pub(crate) fn deck_narrator(
    tx: tokio::sync::mpsc::UnboundedSender<stella_tui::Inbound>,
    agent: &str,
) -> impl FnMut(InitLine) {
    let agent = agent.to_string();
    move |line| {
        let _ = tx.send(match line {
            InitLine::Step(text) => stella_tui::Inbound::Event {
                agent: agent.clone(),
                event: AgentEvent::Text {
                    text: format!("{text}\n"),
                },
            },
            InitLine::Progress(text) => stella_tui::Inbound::Progress {
                agent: agent.clone(),
                text,
            },
        });
    }
}

/// [`deck_narrator`] for the deck's session-startup index build, whose
/// milestones are chrome rather than session speech: a step is a dwell notice,
/// while a counter still rewrites in place rather than stacking another
/// near-identical row in that dialog.
pub(crate) fn deck_notice_narrator(
    tx: tokio::sync::mpsc::UnboundedSender<stella_tui::Inbound>,
    agent: &str,
) -> impl FnMut(InitLine) + Send {
    let agent = agent.to_string();
    move |line| {
        let _ = tx.send(match line {
            InitLine::Step(text) => stella_tui::Inbound::Notice(text),
            InitLine::Progress(text) => stella_tui::Inbound::Progress {
                agent: agent.clone(),
                text,
            },
        });
    }
}

fn narrator(to_stderr: bool) -> impl FnMut(InitLine) + Send {
    use std::io::IsTerminal;

    let rewritable = if to_stderr {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    };
    // True while the cursor is parked on a counter line the next tick may
    // overwrite; a step landing there has to close it first, or it would print
    // on top of the count.
    let mut counter_open = false;
    move |line: InitLine| {
        use std::io::Write;

        let mut out: Box<dyn Write> = if to_stderr {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        };
        let _ = match line {
            InitLine::Step(text) => {
                let lead = if std::mem::take(&mut counter_open) {
                    "\n"
                } else {
                    ""
                };
                writeln!(out, "{lead}  {text}")
            }
            InitLine::Progress(text) if rewritable => {
                counter_open = true;
                // Erase to end of line before parking the cursor back at the
                // start: without it, a shorter count leaves the tail of the
                // longer one it replaced.
                write!(out, "\r  {text}\x1b[K").and_then(|()| out.flush())
            }
            InitLine::Progress(text) => writeln!(out, "  {text}"),
        };
    }
}

/// `stella init` — infer the workspace's domain taxonomy, build the code-graph
/// index, and write `.stella/domains.toml` (see `crate::domains`). Domain
/// inference is model-assisted when a provider resolves, with a deterministic
/// directory heuristic fallback, so init always succeeds — offline included.
/// The code graph (`.stella/private/codegraph.db`) is built unconditionally: it needs
/// no provider, only the on-disk source tree.
///
/// Progress prints straight to stdout as it happens — real counts in the
/// transcript, not a loading animation (#3102 retired the init cinematic:
/// a spinner is indistinguishable from a wedge, and what it drew was erased
/// on exit, leaving nothing to read afterwards).
pub async fn run_init(
    model_override: Option<&str>,
    api_key_override: Option<&str>,
    base_url_override: Option<&str>,
    prune_vectors: bool,
) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;

    plain::section_header("Stella init");

    let (provider, model_hint) =
        match Config::load(model_override, api_key_override, base_url_override) {
            Ok(cfg) => {
                let provider = build_provider(&cfg)?;
                println!(
                    "  {} provider {}/{}",
                    "◈".bright_cyan(),
                    cfg.provider.id,
                    cfg.model_id
                );
                (Some(provider), Some(cfg.model_id))
            }
            Err(_) => {
                println!(
                    "  {} no provider configured — using the directory heuristic \
                 (re-run `stella init` with a key for a better taxonomy)",
                    "!".yellow()
                );
                (None, None)
            }
        };

    let mut io = InitIo::stdout_tty();
    let (domains, _inference_cost_usd) = init_workspace(
        provider.as_deref(),
        &workspace_root,
        model_hint.as_deref(),
        None,
        &mut io,
    )
    .await?;

    // After the embedding pass, so the count reflects the vectors this run
    // just wrote rather than the state it started in.
    super::graph::report_retired_vectors(&workspace_root, prune_vectors, &mut io.step_sink());

    for domain in &domains.domains {
        println!(
            "    {} {} — {} [{}]",
            "·".dimmed(),
            domain.name.bright_magenta(),
            domain.description.dimmed(),
            domain.paths.join(", ").dimmed()
        );
    }
    println!(
        "\n  {}",
        "Domains tag memories, reflections, and every code-graph node/edge; recall uses them \
         for relevance."
            .dimmed()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use stella_protocol::{CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError};

    use super::*;

    /// A provider that counts its completions — the observable the #3102
    /// skip gate is judged by.
    struct CountingProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl stella_protocol::Provider for CountingProvider {
        fn id(&self) -> &str {
            "counting"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResult {
                upstream_provider: None,
                text: r#"[{"name":"api","description":"routes","paths":["api"]}]"#.into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 10,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "counting-model".into(),
                cost_usd: 0.006,
                finish_reason: None,
            })
        }
    }

    /// The witness for #3102 finding 3, end to end: the second `stella init`
    /// over an unchanged repo shape spends **zero** model calls and says so
    /// in the transcript, while the first paid for its inference exactly
    /// once. Fails on old code, which re-bought the taxonomy on every run.
    #[tokio::test]
    async fn an_unchanged_repo_shape_spends_no_second_inference_call() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let provider = CountingProvider {
            calls: AtomicUsize::new(0),
        };

        let mut lines: Vec<String> = Vec::new();
        let (first, first_cost) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut |line| lines.push(line),
        )
        .await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(first_cost > 0.0, "the first run pays for its inference");
        first.save(&root).expect("save");

        let (second, second_cost) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut |line| lines.push(line),
        )
        .await;

        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "an unchanged repo shape must not re-buy the taxonomy"
        );
        assert_eq!(second_cost, 0.0);
        assert_eq!(second.domains, first.domains);
        assert!(
            lines.iter().any(|l| l.contains("no model call")),
            "the skip must be stated in the transcript, not silent: {lines:?}"
        );
    }

    /// The witness for the two-design collision that broke the build: one
    /// [`InitIo`] carries **both** halves of the seam at once — the question
    /// channel and an [`InitLine`] sink that still tells a permanent step
    /// from a live counter.
    ///
    /// The two PRs that landed for this seam each kept one half and dropped
    /// the other, so anything asserting only one of them passes on a design
    /// that lost the other. This asserts both against one call: the record
    /// stages narrate through `step_sink()` (the ✓ domains line), the
    /// code-graph build narrates through the raw `sink()` (its own step), and
    /// the sink's two kinds stay distinguishable rather than collapsing to
    /// `String` — which is what a re-collapse of `InitIo::emit` would undo.
    #[tokio::test]
    async fn init_narrates_both_line_kinds_through_one_io() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        std::fs::write(root.join("api/routes.rs"), "pub fn route() {}\n").expect("write");
        let provider = CountingProvider {
            calls: AtomicUsize::new(0),
        };

        // `(is_step, text)` — the kind is the half a `String` sink erases.
        let seen: std::sync::Mutex<Vec<(bool, String)>> = std::sync::Mutex::new(Vec::new());
        {
            // `ask: None` is the no-human case, and the one an automated test
            // may take: the conversion offer must never block on stdin here.
            let mut io = InitIo::new(
                |line: InitLine| {
                    let step = matches!(line, InitLine::Step(_));
                    seen.lock()
                        .expect("sink lock")
                        .push((step, line.text().to_string()));
                },
                None,
            );
            init_workspace(
                Some(&provider),
                &root,
                Some("counting-model"),
                None,
                &mut io,
            )
            .await
            .expect("init_workspace");

            // The kinds survive the trip through the io rather than being
            // flattened on the way in.
            io.emit(InitLine::Progress("· counter tick".to_string()));
            io.step("permanent".to_string());
        }

        let seen = seen.into_inner().expect("sink lock");
        let steps: Vec<&str> = seen
            .iter()
            .filter(|(step, _)| *step)
            .map(|(_, text)| text.as_str())
            .collect();
        assert!(
            steps.iter().any(|l| l.contains("domains")),
            "the record stages must narrate through the io: {seen:?}"
        );
        assert!(
            steps.iter().any(|l| l.contains("code graph")),
            "the code-graph build must narrate through the same io: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|(step, text)| !*step && text == "· counter tick"),
            "a counter must stay a counter, not become a step: {seen:?}"
        );
    }

    /// Scripted question io for the init-level offer test.
    struct ScriptedIo {
        answer: String,
        asked: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl crate::interactive::AskUserIo for ScriptedIo {
        async fn prompt(&self, question: &str, _options: &[String]) -> Result<String, String> {
            self.asked.lock().unwrap().push(question.to_string());
            Ok(self.answer.clone())
        }
    }

    /// The end-to-end witness for #3641, and the one the offer's own unit
    /// tests structurally cannot provide: a real `init_workspace` run adopts
    /// a command authored in `.claude/commands/`, offers to convert it, and —
    /// on a yes — writes the TOML.
    ///
    /// This is the *accepting* path through init, which had no automated
    /// coverage: the only end-to-end evidence was a manual headless run, and
    /// headless is precisely the branch that converts nothing.
    ///
    /// The user root is named rather than resolved, so the offer cannot reach
    /// the developer's real `~/.stella` — the failure that made this test
    /// unwritable before `user_root` moved onto [`InitIo`]. It is a second
    /// temp directory rather than `None` so the plumbing is exercised, not
    /// bypassed; being empty, it contributes nothing to convert.
    #[tokio::test]
    async fn init_adopts_a_claude_command_and_converts_it_when_the_offer_is_accepted() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let user = tempfile::tempdir().expect("tempdir");
        let root = workspace.path();
        std::fs::create_dir_all(root.join(".claude/commands")).expect("mkdir");
        std::fs::write(
            root.join(".claude/commands/demo.md"),
            "---\nname: demo\ndescription: a demo command\n---\n\nDo the demo.\n",
        )
        .expect("write");

        let io = ScriptedIo {
            answer: "1".to_string(),
            asked: std::sync::Mutex::new(Vec::new()),
        };
        let mut lines: Vec<String> = Vec::new();
        let mut init_io = InitIo::scoped(
            |line: InitLine| lines.push(line.text().to_string()),
            Some(&io),
            Some(user.path().to_path_buf()),
        );

        init_workspace(None, root, None, None, &mut init_io)
            .await
            .expect("init");
        // The io holds the transcript sink's borrow until it is dropped.
        drop(init_io);

        assert_eq!(
            io.asked.lock().unwrap().len(),
            1,
            "init asks about conversion exactly once: {lines:?}"
        );
        assert!(
            root.join(".stella/commands/demo.toml").is_file(),
            "accepting at init writes the TOML: {lines:?}"
        );
        assert!(
            root.join(".stella/commands/demo.md").exists(),
            "the adopted markdown definition is never removed"
        );
        // The whole point of the injected root: the offer wrote inside the
        // temp tree and nowhere else.
        assert!(
            !user.path().join("commands").exists(),
            "an empty user root gains nothing"
        );
    }

    /// The gate never hides a changed repo from the model: grow the tree's
    /// shape and the next run re-infers (and pays) again.
    #[tokio::test]
    async fn a_changed_repo_shape_re_infers() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("api")).expect("mkdir");
        let provider = CountingProvider {
            calls: AtomicUsize::new(0),
        };

        let mut emit = |_line: String| {};
        let (first, _) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut emit,
        )
        .await;
        first.save(&root).expect("save");

        std::fs::create_dir_all(root.join("billing")).expect("mkdir");
        let (_, _) = resolve_domains(
            Some(&provider),
            &root,
            Some("counting-model"),
            None,
            &mut emit,
        )
        .await;

        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "a changed repo shape must re-infer"
        );
    }
}
