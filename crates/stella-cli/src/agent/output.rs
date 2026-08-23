//! Headless output, durable benchmark telemetry, and reflection controls.

use std::io::Write;
use std::sync::Arc;

use stella_core::ports::Clock;
use stella_core::{EventSendError, EventSender};
use stella_protocol::AgentEvent;
use tokio::sync::mpsc;

use crate::OutputFormat;
use crate::runtime::WallClock;

/// Trusted launcher-only sink for timeout-survivable stream-json telemetry.
///
/// Harbor captures stdout only after its exec call returns; an outer timeout
/// can therefore lose all partial output. For benchmark runs the adapter sets
/// this exact mounted-log destination and Stella appends+flushes every complete
/// event itself. No shell/`tee` parent is needed, which is also what lets the
/// adapter `exec stella` with a credential arriving solely on stdin.
const DURABLE_STREAM_JSON_ENV: &str = "STELLA_DURABLE_STREAM_JSON_PATH";
const HARBOR_DURABLE_STREAM_PATH: &str = "/logs/agent/stella-events.jsonl";
const DURABLE_STREAM_FAILURE_EXIT: i32 = 74;

/// Resolve the launcher's fixed durable path without ever accepting a
/// repository-selected append destination. Invalid Unicode is invalid too,
/// and the rejected value is intentionally absent from the error text.
fn configured_durable_stream_path() -> Result<Option<&'static std::path::Path>, String> {
    let Some(path) = std::env::var_os(DURABLE_STREAM_JSON_ENV) else {
        return Ok(None);
    };
    // This is deliberately not a general arbitrary-file output feature.
    // Accepting only Harbor's fixed mounted path prevents a repository .env
    // file from turning stream rendering into an append primitive elsewhere.
    if path != std::ffi::OsStr::new(HARBOR_DURABLE_STREAM_PATH) {
        return Err("invalid durable stream-json path".to_string());
    }
    Ok(Some(std::path::Path::new(HARBOR_DURABLE_STREAM_PATH)))
}

fn open_durable_stream(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "durable stream-json sink is not a regular file",
        ));
    }
    Ok(file)
}

fn append_durable_stream_json_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    let mut file = open_durable_stream(path)?;
    writeln!(file, "{line}")?;
    file.flush()
}

/// Establish the exact mounted sink before a benchmark can spend. A durable
/// path on a non-streaming invocation is a launcher error, not permission to
/// create a file that will never receive events.
pub(super) fn preflight_durable_stream(format: OutputFormat) -> Result<(), String> {
    let Some(path) = configured_durable_stream_path()? else {
        return Ok(());
    };
    if format != OutputFormat::StreamJson {
        return Err(format!(
            "{DURABLE_STREAM_JSON_ENV} requires --output-format stream-json"
        ));
    }
    preflight_durable_stream_path(path)
        .map_err(|error| format!("durable stream-json preflight failed: {error}"))
}

fn preflight_durable_stream_path(path: &std::path::Path) -> std::io::Result<()> {
    let mut file = open_durable_stream(path)?;
    file.flush()
}

/// Persist first, then publish the same complete event to stdout. This order
/// makes stdout a truthful acknowledgement: no observer can see an event that
/// the timeout-survivable sink did not already accept and flush.
fn write_stream_json_line(
    line: &str,
    durable_path: Option<&std::path::Path>,
    stdout: &mut impl Write,
) -> std::io::Result<()> {
    if let Some(path) = durable_path {
        append_durable_stream_json_line(path, line)?;
    }
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

fn emit_stream_json_line(line: &str) -> Result<(), String> {
    let durable_path = configured_durable_stream_path()?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    write_stream_json_line(line, durable_path, &mut stdout)
        .map_err(|error| format!("stream-json persistence/output failed: {error}"))
}

/// Renderer tasks cannot safely return a sink failure to the provider loop:
/// the loop could issue another paid request before observing the join error.
/// Exit immediately instead, after a secret-free stderr diagnostic.
pub(super) fn emit_stream_json_line_or_terminate(line: &str) {
    if let Err(error) = emit_stream_json_line(line) {
        terminate_stream_json(&error);
    }
}

pub(super) fn terminate_stream_json(error: &str) -> ! {
    eprintln!(
        "{}",
        serde_json::json!({
            "type": "error",
            "message": error,
        })
    );
    std::process::exit(DURABLE_STREAM_FAILURE_EXIT)
}

/// Build the event boundary for one run. With Harbor's durable path enabled,
/// every producer clone shares one mutex: serialize, append+flush, then enqueue
/// the exact same event while holding that lock. Durable JSONL order therefore
/// equals renderer-channel order even with concurrent streaming callbacks, and
/// a paid StepUsage producer cannot advance until its metering row is durable.
pub(super) fn event_sender_for_run(
    sender: mpsc::UnboundedSender<AgentEvent>,
    format: OutputFormat,
) -> (EventSender, bool) {
    // An arena run (`stella arena`) records its contextgraph-trace journal at
    // this same persist-first boundary. The recorder is wired programmatically
    // by the subcommand, never by ambient env, so it cannot become a
    // repository-selected append primitive.
    if let Some(recorder) = crate::arena::installed_recorder() {
        return (
            crate::arena::recording_event_sender(sender, recorder),
            false,
        );
    }
    if format == OutputFormat::StreamJson {
        match configured_durable_stream_path() {
            Ok(Some(path)) => {
                return (
                    ordered_durable_event_sender(sender, path.to_path_buf(), Arc::new(WallClock)),
                    true,
                );
            }
            Ok(None) => {}
            Err(error) => terminate_stream_json(&error),
        }
    }
    (EventSender::new(sender), false)
}

/// Open a raw (non-staged) run's turn on `events`: what this workspace's trust
/// gate withheld, then this turn's `ContextRecall` if recall ran, and then the
/// run owner's own `Stage(Execute)`.
///
/// All three are ordered deliberately. The withheld-steering notice goes first
/// because it is a fact about the *session* rather than about this turn: it
/// was established while settings loaded, before any of this existed, and a
/// harness reading the stream should learn that the repository did not steer
/// this run before it reads a single thing the run did (#3616). Recall goes
/// next because the context was assembled before the turn began, and a receipt
/// that ordered it after the first stage would misdescribe when it entered.
/// The stage boundary is here at all because the engine emits none —
/// `StageKind` is the run owner's vocabulary, not the loop's (#3416) — and it
/// lives in this module rather than at the call site because `agent.rs` is a
/// god file closed to growth.
///
/// `withheld` is carried in rather than surveyed here for the same reason
/// `recall_event` is: the answer exists long before this channel does, and
/// re-deriving it could announce something the session did not actually run
/// under.
///
/// It is also announced **once per session**, which is what [`to_announce`]
/// is for: this function is per-turn, and the notice is not.
pub(super) fn open_raw_turn(
    events: &EventSender,
    recall_event: Option<AgentEvent>,
    withheld: Option<&crate::settings::WithheldNotice>,
) {
    if let Some(withheld) = to_announce(withheld, &ANNOUNCED) {
        let _ = events.send(withheld.event());
    }
    if let Some(event) = recall_event {
        let _ = events.send(event);
    }
    let _ = events.send(AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
}

/// Whether this session has already put its withheld-steering notice on a
/// stream.
///
/// Process-scoped because a session **is** a process for every door that
/// reaches [`open_raw_turn`]: `stella run` and `stella goal` are one run per
/// invocation, and the interactive REPL is one loop inside one. The deck's
/// boot announcement (`command_deck::steering::announce_withheld`, named in
/// prose rather than linked because that module is private and an intra-doc
/// link to it does not resolve) shares it, so a deck that later drives a raw
/// turn does not say it twice.
///
/// A static rather than a field on [`crate::settings::WithheldNotice`] because
/// that type is `Copy` and rides a `Serialize`/`Deserialize`/`Eq`
/// `AuthorityPolicy` — interior mutability there would cost all four to record
/// something that is not part of the setting.
static ANNOUNCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The notice this call should announce: `Some` the first time a session with
/// something withheld asks, `None` on every later ask.
///
/// The latch is taken as an argument so the decision is testable without the
/// process's own, and it is spent **only** when there is something to
/// announce: a trusted checkout must not consume the session's one
/// announcement on a notice it never had.
fn to_announce<'a>(
    withheld: Option<&'a crate::settings::WithheldNotice>,
    announced: &std::sync::atomic::AtomicBool,
) -> Option<&'a crate::settings::WithheldNotice> {
    withheld.filter(|_| !announced.swap(true, std::sync::atomic::Ordering::Relaxed))
}

/// Spend this session's withheld-steering announcement from outside the raw
/// door, for a surface that announces at boot instead of at a turn.
///
/// The deck is that surface (#4463): the refusal is established before any
/// turn opens, and sending it from `run_lead_turn` would repeat it on every
/// prompt. Sharing the latch is what keeps the two answers to "has this
/// session been told?" from being two answers.
pub(crate) fn claim_withheld_announcement() -> bool {
    !ANNOUNCED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Give this test the session's latch, unspent, and hold every other test that
/// wants it until this one is done.
///
/// [`ANNOUNCED`] is process state and `cargo test` runs a crate's unit tests in
/// one process, on many threads — so two tests that both spend it are two
/// tests whose result depends on which ran first. The guard is returned rather
/// than dropped here so it lives for the caller's body; binding it to `_`
/// releases it immediately and is the one way to misuse this.
///
/// `#[cfg(test)]` rather than an `#[allow(dead_code)]` production item: it
/// exists for the tests, and the compiler is the right thing to enforce that
/// (AGENTS.md § "Code style").
#[cfg(test)]
pub(crate) fn latch_for_withheld_test() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ANNOUNCED.store(false, std::sync::atomic::Ordering::Relaxed);
    guard
}

/// [`event_sender_for_run`] for a **raw** (non-staged) run: the same boundary,
/// plus the run owner's closing `Stage(Complete)` paired onto the engine's
/// terminal event — see [`EventSender::pairing_stage_complete`].
///
/// A separate constructor rather than a flag because the staged run shares
/// `event_sender_for_run` and must not be paired: the pipeline emits every
/// stage boundary of its own.
///
/// It pairs but does not *seal*: the run-terminal `RunComplete` is emitted
/// explicitly by each raw owner (`persistence::emit_run_complete_on_raw`),
/// and a `RunEnding` (`stella_core::event_sender::RunEnding`) here would put
/// a second terminator on the goal path,
/// which already emits its own. See #3379 for the ending contract.
///
/// `door` contributes the caller's own observer, when it asked for one
/// ([`TurnDoor::observing`](super::persistence::TurnDoor::observing)): the tap
/// sits **outermost**, so it folds every event this turn will send — the
/// engine's, the registry's, and the turn-boundary `FileChange`s
/// (`crate::turn_files`) alike — before any of them reach the durable sink or
/// the renderer (#3552).
pub(super) fn raw_event_sender_for_run(
    sender: mpsc::UnboundedSender<AgentEvent>,
    format: OutputFormat,
    door: &super::persistence::TurnDoor<'_>,
) -> (EventSender, bool) {
    let (events, durable_pre_persisted) = event_sender_for_run(sender, format);
    (
        door.observing(events.pairing_stage_complete()),
        durable_pre_persisted,
    )
}

/// The durable sink, and the point at which an event becomes a *line*.
///
/// The line carries this sink's own `ts` (#2111). The renderer publishes the
/// same event to stdout a moment later and stamps its own, so the two copies
/// differ by the persist — which is the truth, and is why the stamp is
/// documented as "when this sink admitted the line" rather than "when the event
/// happened". Nothing downstream may diff the two streams byte-for-byte; they
/// are two recordings of one event, not two copies of one recording.
fn ordered_durable_event_sender(
    sender: mpsc::UnboundedSender<AgentEvent>,
    path: std::path::PathBuf,
    clock: Arc<dyn Clock>,
) -> EventSender {
    let ordering = Arc::new(std::sync::Mutex::new(()));
    EventSender::from_fn(move |event| {
        // Read the clock before taking the ordering lock, not after: a stamp is
        // the instant the sink *admitted* the event, and blocking behind another
        // producer's write would attribute that wait to this event.
        let line = match stella_protocol::stamped_line(&event, clock.now_ms()) {
            Ok(line) => line,
            Err(error) => {
                terminate_stream_json(&format!("stream-json serialization failed: {error}"))
            }
        };
        let _guard = ordering
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(error) = append_durable_stream_json_line(&path, &line) {
            terminate_stream_json(&format!("durable stream-json write failed: {error}"));
        }
        if sender.send(event).is_err() {
            terminate_stream_json("stream-json renderer stopped before event admission");
        }
        Ok::<(), EventSendError>(())
    })
}

pub(super) fn emit_pre_persisted_stream_json_line_or_terminate(line: &str) {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    if let Err(error) = write_stream_json_line(line, None, &mut stdout) {
        terminate_stream_json(&format!("stream-json stdout write failed: {error}"));
    }
}

pub(super) const DISABLE_REFLECTION_ENV: &str = "STELLA_DISABLE_REFLECTION";

/// Whether a one-shot run may make the post-turn reflection model call.
///
/// Reflection is part of Stella's default learning behavior for every output
/// format. Automation that needs to suppress the extra provider call must do
/// so explicitly with `STELLA_DISABLE_REFLECTION=1` (also accepts `true`,
/// `yes`, or `on`, case-insensitively and with surrounding whitespace).
pub(crate) fn one_shot_reflection_enabled(format: OutputFormat) -> bool {
    let supported_format = matches!(
        format,
        OutputFormat::Text | OutputFormat::Json | OutputFormat::StreamJson
    );
    supported_format && !reflection_explicitly_disabled()
}

/// Whether *this* one-shot turn reflects — the whole rule the call site
/// branches on, in one place a test can reach.
///
/// [`one_shot_reflection_enabled`] answers only "is reflection available for
/// this format", and a test has asserted since it was written that the answer
/// for `Json` is yes. That test passed for the entire period during which no
/// JSON turn ever reflected, because the call site carried a fourth condition
/// — `format == OutputFormat::Text` — that the test could not see. The
/// declaration and the behaviour disagreed and nothing in the gate connected
/// them.
///
/// So the rule lives here now, taking the two per-turn facts as arguments
/// rather than reading them: `warrants_reflection` is whether the turn did
/// enough to have a lesson in it, and `has_memory` is whether there is a
/// workspace memory open to record one into. A caller that adds a condition
/// adds it to this function, where it is covered.
pub(crate) fn should_reflect_after_one_shot(
    format: OutputFormat,
    warrants_reflection: bool,
    has_memory: bool,
) -> bool {
    one_shot_reflection_enabled(format) && warrants_reflection && has_memory
}

/// The reflection opt-out itself, separated from the one-shot format question
/// above because it is neither about one-shots nor about formats: it is the
/// workspace-wide "do not spend the extra provider call" switch, and every door
/// that spends one owes it. The fleet door reads it directly (#3956) — a wave
/// of attempts is the largest batch of reflection calls Stella can make, so an
/// automation that opted out would otherwise have opted out of the cheapest
/// door and kept the dearest.
pub(crate) fn reflection_explicitly_disabled() -> bool {
    std::env::var(DISABLE_REFLECTION_ENV).is_ok_and(|value| is_truthy_env_value(&value))
}

pub(super) fn is_truthy_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod durable_stream_tests {
    use super::*;

    /// A [`Clock`] that never advances, so a durable line's bytes are a
    /// function of the event alone and an assertion can pin them.
    struct FixedClock(u64);

    impl Clock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    struct SinkCheckingWriter {
        path: std::path::PathBuf,
        written: Vec<u8>,
    }

    impl Write for SinkCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert_eq!(
                std::fs::read_to_string(&self.path).unwrap(),
                "{\"type\":\"step_usage\"}\n",
                "durable sink must be flushed before the first stdout write"
            );
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn each_complete_event_is_visible_without_a_terminal_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.jsonl");
        let event = r#"{"type":"step_usage","cost_usd":0.01}"#;

        append_durable_stream_json_line(&path, event).unwrap();

        // This read happens immediately, before any `complete` event or clean
        // process shutdown. It is the unit witness for Harbor killing Stella
        // after a paid call: the completed JSONL record has already crossed
        // the userspace buffer boundary into the mounted file.
        assert_eq!(std::fs::read_to_string(path).unwrap(), format!("{event}\n"));
    }

    /// **Witness (#4500).** The withheld-steering notice is a fact about the
    /// **session**, so a second turn of the same session announces nothing.
    ///
    /// `open_raw_turn` sits inside a per-turn function and `AuthorityPolicy`
    /// never clears `withheld`, so before this the interactive REPL — which
    /// calls `agent::run_turn` once per prompt with the one `Config` it loaded
    /// at boot — put the notice on the stream on every turn of the session.
    /// The deck already answered this by announcing at boot instead
    /// (`command_deck::steering`, #4463); the raw door had no such place to
    /// stand, and this is it.
    ///
    /// Driven through the exported entry point rather than [`to_announce`]
    /// alone, so the wiring is proved and not only the decision. That means it
    /// spends the process's own latch, so it takes
    /// [`latch_for_withheld_test`] first — as every test that spends it must.
    #[test]
    fn the_withheld_notice_is_announced_once_per_session_not_once_per_turn() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(dir.path().join(".stella/memories")).expect("memories");
        std::fs::write(dir.path().join(".stella/memories/a.md"), "lesson").expect("memory");
        let notice = crate::settings::withheld_notice(
            dir.path(),
            Some(stella_protocol::Withholder::ProjectUntrusted),
        )
        .expect("the fixture withholds");

        let (raw_tx, mut rx) = mpsc::unbounded_channel();
        let sender = EventSender::new(raw_tx);
        let _latch = latch_for_withheld_test();
        for _ in 0..3 {
            open_raw_turn(&sender, None, Some(&notice));
        }
        drop(sender);

        let mut announced = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AgentEvent::SteeringWithheld { .. }) {
                announced += 1;
            }
        }
        assert_eq!(
            announced, 1,
            "three turns of one session are owed one notice, not three"
        );
    }

    /// The decision itself, over a latch of the caller's own — the arm the
    /// test above cannot reach twice, and the arm that must stay silent.
    #[test]
    fn a_session_with_nothing_withheld_claims_nothing_and_leaves_the_latch_alone() {
        let latch = std::sync::atomic::AtomicBool::new(false);
        assert!(to_announce(None, &latch).is_none());
        assert!(
            !latch.load(std::sync::atomic::Ordering::Relaxed),
            "a checkout that was owed no notice must not spend the session's one \
             announcement on it"
        );
    }

    #[test]
    fn durable_sink_is_flushed_before_stdout_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut stdout = SinkCheckingWriter {
            path: path.clone(),
            written: Vec::new(),
        };

        write_stream_json_line(r#"{"type":"step_usage"}"#, Some(&path), &mut stdout).unwrap();

        assert_eq!(stdout.written, b"{\"type\":\"step_usage\"}\n");
    }

    #[test]
    fn sink_failure_prevents_stdout_publication() {
        let dir = tempfile::tempdir().unwrap();
        let missing_parent = dir.path().join("missing").join("events.jsonl");
        let mut stdout = Vec::new();

        assert!(write_stream_json_line("{}", Some(&missing_parent), &mut stdout).is_err());
        assert!(stdout.is_empty());
    }

    #[test]
    fn every_durable_line_is_anchored_to_wall_clock() {
        // The evidence file is the only record a finished trial leaves. Without
        // this key nothing in it can be placed on a clock, so an idle gap before
        // a timeout is unmeasurable and the arena transcript has no offsets to
        // render (#2111).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let (raw_tx, _renderer) = mpsc::unbounded_channel();
        let sender = ordered_durable_event_sender(
            raw_tx,
            path.clone(),
            Arc::new(FixedClock(1_754_582_400_123)),
        );

        sender
            .send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Run,
            })
            .unwrap();

        let recorded = std::fs::read_to_string(&path).unwrap();
        let line: serde_json::Value = serde_json::from_str(recorded.trim_end()).unwrap();
        assert_eq!(line["ts"], serde_json::json!(1_754_582_400_123u64));
        // Flattened, not nested: every existing reader still finds the tag.
        assert_eq!(line["type"], serde_json::json!("stage"));
    }

    #[test]
    fn preflight_rejects_non_regular_sink() {
        let dir = tempfile::tempdir().unwrap();
        assert!(preflight_durable_stream_path(dir.path()).is_err());
    }

    #[test]
    fn paid_usage_is_durable_with_renderer_paused_then_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let (raw_tx, mut paused_renderer) = mpsc::unbounded_channel();
        let sender = ordered_durable_event_sender(raw_tx, path.clone(), Arc::new(FixedClock(7)));
        let stage = AgentEvent::Stage {
            name: stella_protocol::StageKind::Execute.into(),
            scope: stella_protocol::StageScope::Run,
        };
        let usage = AgentEvent::StepUsage {
            upstream_provider: None,
            reasoning_tokens: None,
            step: 0,
            role: stella_protocol::ModelCallRole::Worker,
            provider: "provider".to_string(),
            output_text: None,
            model: "provider/model".to_string(),
            input_tokens: 11,
            output_tokens: 7,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            estimated_input_tokens: 10,
            cost_usd: 0.01,
            duration_ms: 5,
            retries: 0,
            tool_calls: 0,
            complete: true,
            finish_reason: None,
            sub_agent_id: None,
        };

        sender.send(stage.clone()).unwrap();
        sender.send(usage.clone()).unwrap();

        // The renderer has not polled once, yet both preceding context and
        // paid-call metering are already ordered and flushed. Dropping it now
        // simulates timeout/cancellation after provider completion.
        let expected = format!(
            "{}\n{}\n",
            stella_protocol::stamped_line(&stage, 7).unwrap(),
            stella_protocol::stamped_line(&usage, 7).unwrap()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(
            serde_json::to_string(&paused_renderer.try_recv().unwrap()).unwrap(),
            serde_json::to_string(&stage).unwrap()
        );
        drop(paused_renderer);
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("step_usage")
        );
    }
}
